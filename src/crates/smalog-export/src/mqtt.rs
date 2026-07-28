//! Native MQTT publisher.
//!
//! smalog publishes one **structured** topic tree per inverter under
//! `base_topic` (default `smalog/{serial}`): grouped scalar leaf topics,
//! a self-describing `attributes` document and online/offline
//! availability. When `homeassistant = true` it additionally emits
//! retained MQTT-Discovery configs so Home Assistant creates every entity
//! (and nests each string under the inverter) automatically.
//!
//! Every reading comes from the crate's metric registry — see
//! `docs/mqtt.md`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

use chrono_tz::Tz;
use rumqttc::{AsyncClient, LastWill, MqttOptions, QoS};
use serde_json::{json, Map, Value as Json};
use tracing::{debug, warn};

use crate::config::MqttConfig;
use crate::error::{Error, Result};
use crate::metrics::{self, Context, Metric, Owner};
use crate::view::{from_cycle, ExportInverter};
use smalog_observation::PollCycleObservation;

/// Per-inverter state remembered across poll cycles: which AC phases and
/// MPP trackers have ever reported data (the published set only grows, so
/// discovery is re-emitted on growth but never churns), plus the last
/// discovery signature.
#[derive(Default)]
struct SerialState {
    seen_phases: u8,
    seen_trackers: BTreeSet<u8>,
    sig: Option<(u8, Vec<u8>, bool)>,
}

/// A single MQTT message to send.
struct Msg {
    topic: String,
    payload: String,
    retain: bool,
}

pub struct Publisher {
    client: AsyncClient,
    cfg: MqttConfig,
    plant_name: String,
    tz: Tz,
    bridge_topic: String,
    version: String,
    state: Mutex<HashMap<u32, SerialState>>,
}

#[derive(Debug, Clone, Copy)]
pub struct SunTimes {
    /// Local decimal hours.
    pub sunrise: f64,
    /// Local decimal hours.
    pub sunset: f64,
}

impl Publisher {
    /// Create the client and spawn its event loop.
    pub fn start(cfg: &MqttConfig, plant_name: &str, tz: Tz, version: &str) -> Result<Publisher> {
        let client_id = cfg
            .client_id
            .clone()
            .unwrap_or_else(|| format!("smalog-{}", std::process::id()));
        let bridge_topic = format!("{}/bridge/availability", root_prefix(&cfg.base_topic));

        let mut opts = MqttOptions::new(client_id, &cfg.host, cfg.port);
        opts.set_keep_alive(std::time::Duration::from_secs(30));
        if let (Some(user), Some(pass)) = (&cfg.username, &cfg.password) {
            opts.set_credentials(user.clone(), pass.clone());
        }
        // Broker marks the bridge offline if smalog dies ungracefully.
        opts.set_last_will(LastWill::new(
            bridge_topic.clone(),
            "offline",
            QoS::AtLeastOnce,
            true,
        ));

        let (client, mut eventloop) = AsyncClient::new(opts, 16);
        // Announce online (retained); queued until the connection is up.
        let _ = client.try_publish(&bridge_topic, QoS::AtLeastOnce, true, "online");
        tokio::spawn(async move {
            loop {
                match eventloop.poll().await {
                    Ok(_) => {}
                    Err(e) => {
                        warn!(error = %e, "mqtt connection error, retrying");
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    }
                }
            }
        });
        Ok(Publisher {
            client,
            cfg: cfg.clone(),
            plant_name: plant_name.to_string(),
            tz,
            bridge_topic,
            version: version.to_string(),
            state: Mutex::new(HashMap::new()),
        })
    }

    fn qos(&self) -> QoS {
        match self.cfg.qos {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    }

    /// Publish the structured tree (and, if enabled, discovery) for every
    /// inverter.
    pub async fn publish(&self, cycle: &PollCycleObservation, sun: Option<SunTimes>) -> Result<()> {
        let inverters = from_cycle(cycle);
        let msgs = {
            // Synchronous assembly: the state lock is never held across an
            // await.
            let mut state = self.state.lock().unwrap();
            let context = Context {
                plant_name: &self.plant_name,
                tz: self.tz,
                sun,
                version: &self.version,
            };
            build_messages(
                &self.cfg,
                &context,
                &self.bridge_topic,
                &mut state,
                &inverters,
            )
        };
        for m in msgs {
            debug!(topic = %m.topic, "mqtt publish");
            self.client
                .publish(m.topic, self.qos(), m.retain, m.payload)
                .await
                .map_err(|e| Error::Mqtt(e.to_string()))?;
        }
        Ok(())
    }
}

/// Assemble every message for one poll cycle. Pure and synchronous so it
/// can be unit-tested without a broker; `state` carries the growing
/// phase/tracker sets across cycles.
fn build_messages(
    cfg: &MqttConfig,
    ctx: &Context<'_>,
    bridge_topic: &str,
    state: &mut HashMap<u32, SerialState>,
    inverters: &[ExportInverter],
) -> Vec<Msg> {
    let mut msgs: Vec<Msg> = Vec::new();

    for inv in inverters {
        let base = cfg
            .base_topic
            .replace("{plantname}", ctx.plant_name)
            .replace("{serial}", &inv.serial.to_string());
        let avail_topic = format!("{base}/availability");

        // Grow the seen-sets and decide the phase/tracker lists.
        let st = state.entry(inv.serial).or_default();
        st.seen_phases |= 1;
        if inv.uac2 != 0 || inv.pac2 != 0 || inv.iac2 != 0 {
            st.seen_phases |= 1 << 1;
        }
        if inv.uac3 != 0 || inv.pac3 != 0 || inv.iac3 != 0 {
            st.seen_phases |= 1 << 2;
        }
        st.seen_trackers
            .extend(inv.mpp.keys().copied().filter(|tracker| *tracker != 0));
        let phases: Vec<u8> = (1..=3u8)
            .filter(|n| st.seen_phases & (1 << (n - 1)) != 0)
            .collect();
        let trackers: Vec<u8> = st.seen_trackers.iter().copied().collect();

        // The full registry is always published; discovery (when enabled)
        // and the `attributes` document describe exactly this set.
        let all = metrics::build_view(inv, ctx, &phases, &trackers);

        // Discovery, re-emitted only when the published set grows.
        if cfg.homeassistant {
            let cur = (st.seen_phases, trackers.clone(), inv.has_battery);
            if st.sig.as_ref() != Some(&cur) {
                st.sig = Some(cur);
                for m in &all {
                    msgs.push(discovery_msg(
                        cfg,
                        ctx.plant_name,
                        bridge_topic,
                        inv,
                        &base,
                        &avail_topic,
                        m,
                    ));
                }
            }
        }

        // Availability + self-describing metadata.
        msgs.push(Msg {
            topic: avail_topic,
            payload: "online".into(),
            retain: true,
        });
        msgs.push(Msg {
            topic: format!("{base}/attributes"),
            payload: attributes_doc(&all),
            retain: true,
        });

        // Leaf state topics.
        for m in &all {
            msgs.push(Msg {
                topic: format!("{base}/{}", m.path),
                payload: m.value.to_payload(ctx.tz),
                retain: cfg.retain,
            });
        }
    }
    msgs
}

/// Build one Home Assistant discovery config message for `m`.
#[allow(clippy::too_many_arguments)]
fn discovery_msg(
    cfg: &MqttConfig,
    plant_name: &str,
    bridge_topic: &str,
    inv: &ExportInverter,
    base: &str,
    avail_topic: &str,
    m: &Metric,
) -> Msg {
    let unique_id = format!("smalog_{}_{}", inv.serial, m.object_id);
    let topic = format!("{}/sensor/{}/config", cfg.discovery_prefix, unique_id);

    let mut conf = Map::new();
    conf.insert("name".into(), json!(m.name));
    conf.insert("unique_id".into(), json!(unique_id));
    conf.insert("state_topic".into(), json!(format!("{base}/{}", m.path)));
    if let Some(u) = m.unit {
        conf.insert("unit_of_measurement".into(), json!(u));
    }
    if let Some(dc) = m.device_class {
        conf.insert("device_class".into(), json!(dc));
    }
    if let Some(sc) = m.state_class {
        conf.insert("state_class".into(), json!(sc));
    }
    if m.diagnostic {
        conf.insert("entity_category".into(), json!("diagnostic"));
    }
    conf.insert(
        "availability".into(),
        json!([{ "topic": bridge_topic }, { "topic": avail_topic }]),
    );
    conf.insert("availability_mode".into(), json!("all"));
    conf.insert("device".into(), device_block(plant_name, inv, m.owner));

    Msg {
        topic,
        payload: Json::Object(conf).to_string(),
        retain: true,
    }
}

/// The HA `device` block: the inverter, or a string child linked to it via
/// `via_device`.
fn device_block(plant_name: &str, inv: &ExportInverter, owner: Owner) -> Json {
    let inv_id = format!("smalog_{}", inv.serial);
    let inv_name = if inv.display_name().is_empty() {
        plant_name.to_string()
    } else {
        inv.display_name().to_string()
    };
    match owner {
        Owner::Inverter => json!({
            "identifiers": [inv_id],
            "name": inv_name,
            "manufacturer": "SMA",
            "model": inv.device_type,
            "sw_version": inv.sw_version,
        }),
        Owner::Mppt(n) => json!({
            "identifiers": [format!("{inv_id}_mppt{n}")],
            "name": format!("{inv_name} String {n}"),
            "via_device": inv_id,
        }),
    }
}

/// The `attributes` document: units/classes for every published metric
/// that carries them, so non-Home-Assistant consumers are self-describing.
fn attributes_doc(metrics: &[Metric]) -> String {
    let mut map = Map::new();
    for m in metrics {
        if m.unit.is_none() && m.device_class.is_none() {
            continue;
        }
        let mut meta = Map::new();
        if let Some(u) = m.unit {
            meta.insert("unit".into(), json!(u));
        }
        if let Some(dc) = m.device_class {
            meta.insert("device_class".into(), json!(dc));
        }
        if let Some(sc) = m.state_class {
            meta.insert("state_class".into(), json!(sc));
        }
        map.insert(m.path.clone(), Json::Object(meta));
    }
    Json::Object(map).to_string()
}

/// The topic prefix before `{serial}` — used for the shared bridge
/// availability topic (`<prefix>/bridge/availability`).
fn root_prefix(base_topic: &str) -> String {
    match base_topic.split_once("{serial}") {
        Some((pre, _)) => pre.trim_end_matches('/').to_string(),
        None => base_topic.trim_end_matches('/').to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Mppt;

    fn inv() -> ExportInverter {
        ExportInverter {
            serial: 42,
            device_name: "SB5000TL".into(),
            device_type: "SB5000TL-21".into(),
            total_pac: 4200,
            uac1: 23012,
            mpp: std::collections::BTreeMap::from([
                (
                    1,
                    Mppt {
                        pdc: 800,
                        udc: 38000,
                        idc: 2100,
                    },
                ),
                (
                    2,
                    Mppt {
                        pdc: 700,
                        udc: 37000,
                        idc: 1900,
                    },
                ),
            ]),
            ..ExportInverter::default()
        }
    }

    fn cfg(homeassistant: bool) -> MqttConfig {
        MqttConfig {
            homeassistant,
            ..MqttConfig::default()
        }
    }

    fn context(plant_name: &str) -> Context<'_> {
        Context {
            plant_name,
            tz: Tz::UTC,
            sun: None,
            version: "test",
        }
    }

    fn run(cfg: &MqttConfig) -> (Vec<Msg>, HashMap<u32, SerialState>) {
        let bridge = format!("{}/bridge/availability", root_prefix(&cfg.base_topic));
        let mut state = HashMap::new();
        let msgs = build_messages(cfg, &context("MyPlant"), &bridge, &mut state, &[inv()]);
        (msgs, state)
    }

    fn topics(msgs: &[Msg]) -> Vec<&str> {
        msgs.iter().map(|m| m.topic.as_str()).collect()
    }

    #[test]
    fn bridge_topic_derived_from_prefix() {
        assert_eq!(root_prefix("smalog/{serial}"), "smalog");
        assert_eq!(root_prefix("pv/{plantname}/{serial}"), "pv/{plantname}");
        assert_eq!(root_prefix("flat"), "flat");
    }

    #[test]
    fn structured_layout_publishes_full_tree_without_discovery() {
        let (msgs, _) = run(&cfg(false));
        let t = topics(&msgs);
        assert!(t.contains(&"smalog/42/availability"));
        assert!(t.contains(&"smalog/42/attributes"));
        // The full registry is always published — every group present.
        assert!(t.contains(&"smalog/42/ac/power_total"));
        assert!(t.contains(&"smalog/42/mppt/1/power"));
        assert!(t.contains(&"smalog/42/mppt/1/voltage"));
        assert!(t.contains(&"smalog/42/mppt/2/power"));
        assert!(t.contains(&"smalog/42/energy/total"));
        // ...but no discovery in the non-HA layout.
        assert!(!t.iter().any(|x| x.starts_with("homeassistant/")));
    }

    #[test]
    fn homeassistant_adds_discovery_on_top_of_full_tree() {
        let (msgs, _) = run(&cfg(true));
        let t = topics(&msgs);
        assert!(t.contains(&"smalog/42/energy/total"));
        assert!(t.contains(&"homeassistant/sensor/smalog_42_ac_power_total/config"));
        assert!(t.contains(&"homeassistant/sensor/smalog_42_mppt_1_power/config"));
    }

    #[test]
    fn discovery_config_shape() {
        let (msgs, _) = run(&cfg(true));
        let disco = msgs
            .iter()
            .find(|m| m.topic == "homeassistant/sensor/smalog_42_ac_power_total/config")
            .expect("discovery for ac/power_total");
        let v: Json = serde_json::from_str(&disco.payload).unwrap();
        assert!(disco.retain);
        assert_eq!(v["state_topic"], "smalog/42/ac/power_total");
        assert_eq!(v["unit_of_measurement"], "W");
        assert_eq!(v["device_class"], "power");
        assert_eq!(v["state_class"], "measurement");
        assert_eq!(v["device"]["identifiers"][0], "smalog_42");
        assert_eq!(v["availability_mode"], "all");

        // A string entity is its own device linked via_device.
        let str1 = msgs
            .iter()
            .find(|m| m.topic == "homeassistant/sensor/smalog_42_mppt_1_power/config")
            .expect("discovery for string 1");
        let s: Json = serde_json::from_str(&str1.payload).unwrap();
        assert_eq!(s["device"]["identifiers"][0], "smalog_42_mppt1");
        assert_eq!(s["device"]["via_device"], "smalog_42");
        assert_eq!(s["device"]["name"], "SB5000TL String 1");
    }

    #[test]
    fn discovery_emitted_once_then_only_on_growth() {
        let cfg = cfg(true);
        let bridge = "smalog/bridge/availability".to_string();
        let mut state = HashMap::new();

        let first = build_messages(&cfg, &context("MyPlant"), &bridge, &mut state, &[inv()]);
        assert!(first.iter().any(|m| m.topic.contains("/config")));

        // Same shape next cycle -> no discovery re-emitted.
        let second = build_messages(&cfg, &context("MyPlant"), &bridge, &mut state, &[inv()]);
        assert!(!second.iter().any(|m| m.topic.contains("/config")));

        // A third string appears -> discovery grows again.
        let mut bigger = inv();
        bigger.mpp.insert(
            3,
            Mppt {
                pdc: 500,
                udc: 30000,
                idc: 1500,
            },
        );
        let third = build_messages(&cfg, &context("MyPlant"), &bridge, &mut state, &[bigger]);
        assert!(third
            .iter()
            .any(|m| m.topic == "homeassistant/sensor/smalog_42_mppt_3_power/config"));
    }

    #[test]
    fn phase_two_appears_only_after_nonzero_seen() {
        let cfg = cfg(false);
        let bridge = "smalog/bridge/availability".to_string();
        let mut state = HashMap::new();

        // Single-phase sample: no L2 voltage ever seen.
        let one = build_messages(&cfg, &context("P"), &bridge, &mut state, &[inv()]);
        assert!(!topics(&one).contains(&"smalog/42/ac/voltage_l2"));

        // Once L2 reports, it sticks.
        let mut three = inv();
        three.uac2 = 23000;
        let two = build_messages(&cfg, &context("P"), &bridge, &mut state, &[three]);
        assert!(topics(&two).contains(&"smalog/42/ac/voltage_l2"));
    }

    #[test]
    fn observed_tracker_keys_publish_even_when_sparse_or_zero() {
        type TrackerCase<'a> = (&'a [(u8, Mppt)], &'a [u8]);
        let cfg = cfg(false);
        let bridge = "smalog/bridge/availability".to_string();
        let cases: &[TrackerCase<'_>] = &[
            (&[], &[]),
            (
                &[(
                    7,
                    Mppt {
                        pdc: 0,
                        udc: 0,
                        idc: 0,
                    },
                )],
                &[7],
            ),
            (
                &[
                    (
                        2,
                        Mppt {
                            pdc: 0,
                            udc: 0,
                            idc: 0,
                        },
                    ),
                    (
                        255,
                        Mppt {
                            pdc: 0,
                            udc: 0,
                            idc: 0,
                        },
                    ),
                ],
                &[2, 255],
            ),
            (
                &[(
                    255,
                    Mppt {
                        pdc: 0,
                        udc: 0,
                        idc: 0,
                    },
                )],
                &[255],
            ),
        ];

        for (trackers, expected) in cases {
            let mut inverter = inv();
            inverter.mpp.clear();
            inverter.mpp.extend(trackers.iter().copied());
            let mut state = HashMap::new();
            let msgs = build_messages(&cfg, &context("P"), &bridge, &mut state, &[inverter]);
            let actual: Vec<u8> = state[&42].seen_trackers.iter().copied().collect();
            assert_eq!(&actual, expected);
            for &tracker in *expected {
                let topic = format!("smalog/42/mppt/{tracker}/power");
                assert!(msgs.iter().any(|msg| msg.topic == topic));
            }
        }
    }

    #[test]
    fn unicode_identity_survives_leaf_and_discovery_payloads() {
        let cfg = cfg(true);
        let bridge = "smalog/bridge/availability".to_string();
        let mut inverter = inv();
        inverter.configured_name = Some("Grüße aus 東京 🌞".into());
        inverter.device_type = "Wechselrichter Δ".into();
        let mut state = HashMap::new();
        let msgs = build_messages(
            &cfg,
            &context("Anlage München"),
            &bridge,
            &mut state,
            &[inverter],
        );

        let name = msgs
            .iter()
            .find(|msg| msg.topic == "smalog/42/info/name")
            .unwrap();
        assert_eq!(name.payload, "Grüße aus 東京 🌞");
        assert!(std::str::from_utf8(name.payload.as_bytes()).is_ok());

        let discovery = msgs
            .iter()
            .find(|msg| msg.topic == "homeassistant/sensor/smalog_42_ac_power_total/config")
            .unwrap();
        assert!(std::str::from_utf8(discovery.payload.as_bytes()).is_ok());
        let payload: Json = serde_json::from_str(&discovery.payload).unwrap();
        assert_eq!(payload["device"]["name"], "Grüße aus 東京 🌞");
        assert_eq!(payload["device"]["model"], "Wechselrichter Δ");
    }
}
