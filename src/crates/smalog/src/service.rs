//! The long-running service: wall-clock-aligned poll ticks, daylight
//! gating, database exports, MQTT publish, and an axum HTTP server
//! (`/healthz`, `/status`, `/api/*`, and the embedded UI on `--features
//! ui`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

use crate::applog::{LogBuffer, LogLevel, LogQuery};
use crate::config::{Config, InverterCommunication, InverterConfig};
use crate::daylight;
use crate::diagnostics::{CollectorSink, DiagnosticsWriter, RingBounds, Shutdown, WriteQueue};
use crate::error::Result;
use crate::storage::Db;
use smalog_connection::{
    BluetoothConnection, Collector, Connection, PollOptions, SpeedwireConnection,
    SpeedwireInverterSpec,
};
use smalog_export::{CsvWriter, MqttPublisher, SunTimes as ExportSunTimes};
use smalog_observation::{
    ArchiveOutcome, CanonicalText, EventValue, InverterDailyYield, InverterEnergySample,
    InverterEvent, InverterPollObservation, PollCycleObservation, UnixSeconds, WattHours, Watts,
};
use smalog_storage::diagnostics as storage_diagnostics;
use smalog_tags as tags;

#[derive(Default)]
struct Status {
    last_poll: Option<i64>,
    last_error: Option<String>,
    is_light: bool,
    inverters: Vec<InverterStatus>,
}

struct InverterStatus {
    serial: u32,
    name: String,
    total_pac: i32,
    e_today: i64,
    e_total: i64,
    status: String,
}

pub struct Service {
    config: Config,
    tz: Tz,
    collectors: Vec<ConfiguredCollector>,
    db: Arc<Db>,
    mqtt: Option<MqttPublisher>,
    status: Arc<RwLock<Status>>,
    last_daily: Option<chrono::NaiveDate>,
    diagnostics: Arc<WriteQueue>,
    transmission_ring: RingBounds,
    log_buffer: Arc<LogBuffer>,
}

struct ConfiguredCollector {
    target: String,
    collector: Collector,
}

impl Service {
    pub async fn new(
        config: Config,
        diagnostics: Arc<WriteQueue>,
        log_buffer: Arc<LogBuffer>,
    ) -> Result<Service> {
        let tz = config.timezone()?;
        // Select the tag-text language (event descriptions, CSV headers).
        // validate() has already confirmed the locale parses.
        if let Some(locale) = tags::Locale::parse(&config.locale) {
            tags::set_locale(locale);
        }
        let db = Arc::new(if config.database.daily_statistics {
            Db::connect_with_daily_statistics(&config.database.url, tz, config.service.interval)
                .await?
        } else {
            Db::connect(&config.database.url, tz).await?
        });
        let transmission_ring = RingBounds::new(
            config.service.transmission_log_retention_hours,
            config.service.transmission_log_max_entries,
        );
        // Create the optional diagnostics tables only when transmissions are
        // recorded into them. Disabling later never drops them: that would
        // delete the history an operator disabled recording to preserve.
        // The application log needs no table at all.
        if transmission_ring.enabled() {
            db.enable_diagnostics().await?;
        }
        let collectors = Self::build_collectors(
            &config,
            tz,
            transmission_ring.enabled().then(|| diagnostics.clone()),
        )
        .await?;
        let mqtt = if config.mqtt.enabled {
            Some(MqttPublisher::start(
                &config.mqtt,
                &config.plant.name,
                tz,
                crate::VERSION,
            )?)
        } else {
            None
        };
        Ok(Service {
            config,
            tz,
            collectors,
            db,
            mqtt,
            status: Arc::new(RwLock::new(Status::default())),
            last_daily: None,
            diagnostics,
            transmission_ring,
            log_buffer,
        })
    }

    /// Build one shared Ethernet collector plus one collector for every
    /// configured Bluetooth inverter.
    async fn build_collectors(
        config: &Config,
        tz: Tz,
        diagnostics: Option<Arc<WriteQueue>>,
    ) -> Result<Vec<ConfiguredCollector>> {
        let options = PollOptions {
            calc_missing_spot: config.service.calc_missing_spot,
            poll_consumption: config.service.poll_consumption,
        };
        let mut collectors = Vec::new();
        let ethernet_specs: Vec<_> = config
            .inverters
            .iter()
            .filter_map(|inverter| {
                let InverterCommunication::Ethernet { address, serial } = &inverter.communication
                else {
                    return None;
                };
                Some(SpeedwireInverterSpec {
                    address: address.clone(),
                    serial: *serial,
                    password: inverter.password.clone(),
                    user_group: inverter.user_group,
                })
            })
            .collect();
        if !ethernet_specs.is_empty() {
            let target = config
                .inverters
                .iter()
                .filter(|inverter| inverter.is_ethernet())
                .map(inverter_target)
                .collect::<Vec<_>>()
                .join(", ");
            let connector: Box<dyn Connection> =
                Box::new(SpeedwireConnection::connect(ethernet_specs).await?);
            let collector = match &diagnostics {
                Some(queue) => Collector::with_sink(
                    connector,
                    tz,
                    options,
                    CollectorSink::new(queue.clone(), &target),
                ),
                None => Collector::new(connector, tz, options),
            };
            collectors.push(ConfiguredCollector { target, collector });
        }
        for inverter in config
            .inverters
            .iter()
            .filter(|inverter| !inverter.is_ethernet())
        {
            let bluetooth: BluetoothConnection =
                BluetoothConnection::new(inverter.to_bluetooth_params(tz)?);
            let target = inverter_target(inverter);
            let collector = match &diagnostics {
                Some(queue) => Collector::with_sink(
                    Box::new(bluetooth),
                    tz,
                    options,
                    CollectorSink::new(queue.clone(), &target),
                ),
                None => Collector::new(Box::new(bluetooth), tz, options),
            };
            collectors.push(ConfiguredCollector { target, collector });
        }
        Ok(collectors)
    }

    fn apply_configured_names(config: &Config, inverters: &mut [InverterPollObservation]) {
        for inverter in inverters {
            if let Some(configured) =
                config
                    .inverters
                    .iter()
                    .find(|configured| match &configured.communication {
                        InverterCommunication::Ethernet { address, serial } => {
                            serial.is_some_and(|serial| serial == inverter.identity.serial_number)
                                || address.as_deref().is_some_and(|address| {
                                    inverter
                                        .communication
                                        .endpoint
                                        .as_ref()
                                        .is_some_and(|endpoint| endpoint.as_str() == address)
                                })
                        }
                        InverterCommunication::Bluetooth { address, .. } => inverter
                            .communication
                            .endpoint
                            .as_ref()
                            .is_some_and(|endpoint| {
                                address.eq_ignore_ascii_case(endpoint.as_str())
                            }),
                    })
            {
                inverter.identity.configured_name = CanonicalText::new(&configured.name).ok();
            }
        }
    }

    /// Main loop: run until SIGINT/SIGTERM.
    pub async fn run(mut self) -> Result<()> {
        // The writer owns every database round trip for diagnostics, so a
        // slow or failing database can never reach the poll loop below.
        let shutdown = Shutdown::new();
        let writer = self.transmission_ring.enabled().then(|| {
            let writer = DiagnosticsWriter::new(
                self.diagnostics.clone(),
                self.db.clone(),
                self.transmission_ring,
            );
            let shutdown = shutdown.clone();
            tokio::spawn(writer.run(shutdown))
        });

        if let Some(addr) = self.config.service.listen {
            let state = ApiState {
                status: self.status.clone(),
                db: self.db.clone(),
                tz: self.tz,
                diagnostics: self.diagnostics.clone(),
                transmission_ring: self.transmission_ring,
                log_buffer: self.log_buffer.clone(),
            };
            tokio::spawn(async move {
                if let Err(e) = serve_http(addr, state).await {
                    error!(error = %e, "http server failed");
                }
            });
            log_http_endpoints(addr);
        }

        let interval = self.config.service.interval;
        info!(
            interval,
            plant = %self.config.plant.name,
            "smalog service started"
        );

        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            let sleep = Duration::from_secs(next_tick_in(interval));
            tokio::select! {
                _ = tokio::time::sleep(sleep) => {}
                _ = tokio::signal::ctrl_c() => { info!("SIGINT, shutting down"); break; }
                _ = sigterm.recv() => { info!("SIGTERM, shutting down"); break; }
            }
            self.tick().await;
        }
        // Give the writer a bounded chance to persist what is still queued —
        // the records right before a restart are the ones worth having.
        if let Some(writer) = writer {
            shutdown.trigger();
            if tokio::time::timeout(Duration::from_secs(5), writer)
                .await
                .is_err()
            {
                warn!("diagnostics writer did not finish flushing within 5 s");
            }
        }
        Ok(())
    }

    /// Run exactly one cycle (CLI `once` mode / called per tick).
    pub async fn tick(&mut self) {
        let plant = &self.config.plant;
        let gated = plant.latitude != 0.0 || plant.longitude != 0.0;
        let light = !gated
            || daylight::is_light(
                plant.latitude,
                plant.longitude,
                self.tz,
                plant.sun_rs_offset,
            );
        {
            let mut st = self.status.write().await;
            st.is_light = light;
        }
        if !light && !self.config.service.poll_at_night {
            info!("it's dark — skipping poll");
            return;
        }

        // Daily housekeeping (month + event archive) on the first tick
        // of each local day.
        let today = Utc::now().with_timezone(&self.tz).date_naive();
        let daily = if self.last_daily != Some(today) {
            Some((self.config.archive.months, self.config.archive.event_months))
        } else {
            None
        };

        let observed_at = UnixSeconds::new(Utc::now().timestamp());
        let mut status_cycle = PollCycleObservation {
            observed_at,
            inverters: Vec::new(),
            site_consumption: None,
        };
        let mut fresh_cycle = PollCycleObservation {
            observed_at,
            inverters: Vec::new(),
            site_consumption: None,
        };
        let mut errors = Vec::new();
        let mut successful_collectors = 0usize;
        for configured in &mut self.collectors {
            match configured.collector.cycle_observations(true, daily).await {
                Ok(mut collected) => {
                    Self::apply_configured_names(&self.config, &mut collected.inverters);
                    fresh_cycle.observed_at = collected.observed_at;
                    if fresh_cycle.site_consumption.is_none() {
                        fresh_cycle.site_consumption = collected.site_consumption.clone();
                    }
                    fresh_cycle.inverters.extend(collected.inverters.clone());
                    status_cycle.inverters.extend(collected.inverters);
                    successful_collectors += 1;
                }
                Err(error) => {
                    error!(
                        target = %configured.target,
                        error = %error,
                        "inverter poll failed; retrying at next interval"
                    );
                    errors.push(format!("{}: {error}", configured.target));
                    if let Ok(mut stale) = configured.collector.observations() {
                        Self::apply_configured_names(&self.config, &mut stale.inverters);
                        status_cycle.inverters.extend(stale.inverters);
                    }
                }
            }
        }
        if successful_collectors > 0 {
            if daily.is_some() && errors.is_empty() {
                self.last_daily = Some(today);
            }
            self.export(&fresh_cycle).await;
        }
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        self.update_status(&status_cycle, error).await;
    }

    /// One cycle for `smalog once`: poll, then persist the diagnostics that
    /// cycle produced. Without the flush the queued entries would be dropped
    /// when the process exits, which is the opposite of what a one-shot
    /// diagnostic run is for.
    pub async fn tick_once(&mut self) {
        self.tick().await;
        if self.transmission_ring.enabled() {
            DiagnosticsWriter::new(
                self.diagnostics.clone(),
                self.db.clone(),
                self.transmission_ring,
            )
            .flush()
            .await;
        }
    }

    async fn export(&mut self, cycle: &PollCycleObservation) {
        let db = &self.db;

        for inverter in &cycle.inverters {
            if let Some(live) = inverter.observed() {
                if let Err(e) = db.write_poll(&inverter.identity, &live.measurement).await {
                    error!(
                        serial = inverter.identity.serial_number,
                        error = %e,
                        "atomic poll database export failed"
                    );
                }
            }
            if let Some(samples) = inverter.day_archive.completed() {
                let samples = samples
                    .iter()
                    .map(|sample| InverterEnergySample {
                        measured_at: sample.measured_at,
                        total_energy: sample.total_energy,
                        power: sample.power,
                    })
                    .collect::<Vec<_>>();
                if let Err(e) = db.write_energy_samples(&inverter.identity, &samples).await {
                    error!(
                        serial = inverter.identity.serial_number,
                        error = %e,
                        "day data export failed"
                    );
                }
            }
            if let Some(samples) = inverter.month_yield_archive.completed() {
                let samples = samples
                    .iter()
                    .map(|sample| InverterDailyYield {
                        measured_at: sample.measured_at,
                        total_energy: sample.total_energy,
                        daily_energy: sample.daily_energy,
                    })
                    .collect::<Vec<_>>();
                if let Err(e) = db.write_daily_yields(&inverter.identity, &samples).await {
                    error!(
                        serial = inverter.identity.serial_number,
                        error = %e,
                        "month data export failed"
                    );
                }
            }
            if let ArchiveOutcome::Complete(events) = &inverter.event_archive {
                for event in events {
                    if let Err(e) = self.export_event(event).await {
                        error!(
                            serial = inverter.identity.serial_number,
                            error = %e,
                            "event export failed"
                        );
                        break;
                    }
                }
            }
        }
        if let Some(consumption) = &cycle.site_consumption {
            if let Err(e) = db.write_consumption(consumption).await {
                error!(error = %e, "consumption export failed");
            }
        }

        // SBFspot-compatible CSV files (off unless [csv].enabled). Each
        // export self-gates on the data present this cycle: spot/battery
        // append every tick, day is rewritten every tick, month/events
        // only on the daily housekeeping tick.
        if self.config.csv.enabled {
            let w = CsvWriter::new(
                &self.config.csv,
                &self.config.plant.name,
                self.tz,
                crate::VERSION,
            );
            for (what, res) in [
                ("spot", w.export_spot(cycle)),
                ("battery", w.export_battery(cycle)),
                ("day", w.export_day(cycle)),
                ("month", w.export_month(cycle)),
                ("events", w.export_events(cycle)),
            ] {
                if let Err(e) = res {
                    error!(what, error = %e, "csv export failed");
                }
            }
        }

        if let Some(mqtt) = &self.mqtt {
            let plant = &self.config.plant;
            let sun = if plant.latitude != 0.0 || plant.longitude != 0.0 {
                let times = daylight::sun_times(
                    plant.latitude,
                    plant.longitude,
                    Utc::now().with_timezone(&self.tz).date_naive(),
                    self.tz,
                );
                Some(ExportSunTimes {
                    sunrise: times.sunrise,
                    sunset: times.sunset,
                })
            } else {
                None
            };
            if let Err(e) = mqtt.publish(cycle, sun).await {
                warn!(error = %e, "mqtt publish failed");
            }
        }
    }

    /// Render one event to its DB row (db_SQLite_Export semantics).
    async fn export_event(&self, event: &InverterEvent) -> Result<()> {
        let old_value = event.old_value.as_ref().map(format_event_value);
        let new_value = event.new_value.as_ref().map(format_event_value);
        Ok(self
            .db
            .export_event(
                i64::from(event.entry_id),
                event.occurred_at.get(),
                event.serial_number,
                i32::from(event.susy_id),
                i64::from(event.event_code),
                event.event_type.as_str(),
                event.category.as_str(),
                tags::desc_or(event.group_tag.get(), "?"),
                tags::desc_or(event.message_tag.get(), "?"),
                old_value.as_deref(),
                new_value.as_deref(),
                tags::desc_or(event.user_group_tag.get(), "?"),
            )
            .await?)
    }

    async fn update_status(&self, cycle: &PollCycleObservation, err: Option<String>) {
        let mut st = self.status.write().await;
        st.last_poll = Some(cycle.observed_at.get());
        st.last_error = err;
        st.inverters = cycle
            .inverters
            .iter()
            .filter_map(|inverter| {
                let live = inverter.observed()?;
                Some(InverterStatus {
                    serial: inverter.identity.serial_number,
                    name: inverter.display_name().to_owned(),
                    total_pac: live
                        .reported_ac_power
                        .map(Watts::get)
                        .or_else(|| {
                            live.measurement
                                .ac_power_total_w()
                                .and_then(|value| i32::try_from(value).ok())
                        })
                        .unwrap_or(0),
                    e_today: live
                        .measurement
                        .energy_today
                        .map(WattHours::get)
                        .unwrap_or(0),
                    e_total: live
                        .measurement
                        .energy_total
                        .map(WattHours::get)
                        .unwrap_or(0),
                    status: live
                        .measurement
                        .device_status
                        .map(|status| tags::desc_or(status.get(), "?"))
                        .unwrap_or("?")
                        .to_owned(),
                })
            })
            .collect();
    }
}

fn format_event_value(value: &EventValue) -> String {
    match value {
        EventValue::Integer(value) => value.to_string(),
        EventValue::Unsigned(value) => value.to_string(),
        EventValue::Tag(value) => tags::desc_or(value.get(), "?").to_owned(),
        EventValue::Text(value) => value.as_str().to_owned(),
    }
}

fn inverter_target(inverter: &InverterConfig) -> String {
    match &inverter.communication {
        InverterCommunication::Ethernet { address, serial } => {
            let endpoint = match (serial, address) {
                (Some(serial), Some(address)) => {
                    format!("serial {serial}, Ethernet {address}")
                }
                (Some(serial), None) => format!("serial {serial}, Ethernet discovery"),
                (None, Some(address)) => format!("Ethernet {address}"),
                (None, None) => "Ethernet discovery".to_string(),
            };
            format!("{} ({endpoint})", inverter.name)
        }
        InverterCommunication::Bluetooth { address, .. } => {
            format!("{} (Bluetooth {address})", inverter.name)
        }
    }
}

/// Seconds until the next wall-clock-aligned tick.
fn next_tick_in(interval: u64) -> u64 {
    let now = Utc::now().timestamp() as u64;
    interval - (now % interval)
}

/// Shared state for the axum handlers.
#[derive(Clone)]
struct ApiState {
    status: Arc<RwLock<Status>>,
    db: Arc<Db>,
    tz: Tz,
    diagnostics: Arc<WriteQueue>,
    transmission_ring: RingBounds,
    log_buffer: Arc<LogBuffer>,
}

impl ApiState {
    /// State for a handler test: both rings enabled with their defaults.
    #[cfg(test)]
    fn for_tests(db: Arc<Db>, tz: Tz) -> ApiState {
        ApiState {
            status: Arc::new(RwLock::new(Status::default())),
            db,
            tz,
            diagnostics: WriteQueue::new(crate::diagnostics::QUEUE_CAPACITY),
            transmission_ring: RingBounds::new(48, 50_000),
            log_buffer: LogBuffer::new(48, 50_000),
        }
    }
}

/// Query parameters for `GET /api/history`.
#[derive(Deserialize)]
struct HistoryParams {
    /// `day` | `week` | `month` | `year` (default `day`).
    range: Option<String>,
    /// Selected local day (`YYYY-MM-DD`) for the day view.
    date: Option<String>,
    /// Selected local week (`YYYY-MM-DD`, normalized to Monday).
    week: Option<String>,
    /// Selected local month (`YYYY-MM`) for the month view.
    month: Option<String>,
    /// Selected calendar year (`YYYY`) for the year view.
    year: Option<String>,
    /// Restrict to one inverter; omit for the aggregate of all inverters.
    serial: Option<u32>,
    /// Break the day view into per-string DC power (needs `serial`).
    #[serde(default)]
    strings: bool,
}

#[derive(Deserialize)]
struct DiagnosticsParams {
    /// Selected local day (`YYYY-MM-DD`); omitted means today.
    date: Option<String>,
    /// Restrict to one inverter; omit to return every inverter.
    serial: Option<u32>,
}

#[derive(Serialize)]
struct DiagnosticMpptResponse {
    tracker_number: u8,
    dc_power_w: Option<i32>,
    dc_current_ma: Option<i32>,
    dc_voltage_mv: Option<i32>,
}

struct HistoryPeriod {
    date: Option<NaiveDate>,
    week: Option<NaiveDate>,
    month: Option<NaiveDate>,
    year: Option<i32>,
}

/// Base URL a browser on this machine can open for `addr`.
///
/// A wildcard bind (`0.0.0.0`, `::`) is not routable, so it is reported as the
/// matching loopback address; IPv6 hosts are bracketed as URL authorities
/// require. The result never has a trailing slash.
fn browsable_base_url(addr: std::net::SocketAddr) -> String {
    let ip = addr.ip();
    let host = match ip {
        std::net::IpAddr::V4(v4) if v4.is_unspecified() => "127.0.0.1".to_string(),
        std::net::IpAddr::V4(v4) => v4.to_string(),
        std::net::IpAddr::V6(v6) if v6.is_unspecified() => "[::1]".to_string(),
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
    };
    format!("http://{host}:{}", addr.port())
}

/// Log the HTTP endpoints as full URLs, which terminals render as clickable
/// links. The dashboard is only served when built `--features ui`; without it
/// the Vite dev server (`pnpm run dev`) proxies `/api` to this port instead.
fn log_http_endpoints(addr: std::net::SocketAddr) {
    let base = browsable_base_url(addr);
    info!(%addr, "http endpoint listening");
    if cfg!(feature = "ui") {
        info!(url = %format!("{base}/"), "dashboard");
    } else {
        info!(
            url = %format!("{base}/api/status"),
            "dashboard not embedded (build with --features ui); \
             run `pnpm run dev` in src/ui and open http://localhost:5173"
        );
    }
    info!(url = %format!("{base}/status"), "status");
    info!(url = %format!("{base}/healthz"), "health");
}

/// The HTTP server (axum): health, live status, history and inverter list;
/// plus the embedded dashboard as a fallback when built `--features ui`.
async fn serve_http(addr: std::net::SocketAddr, state: ApiState) -> Result<()> {
    #[allow(unused_mut)]
    let mut app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/status", get(status_handler))
        .route("/api/status", get(status_handler))
        .route("/api/inverters", get(inverters_handler))
        .route("/api/history", get(history_handler))
        .route("/api/diagnostics", get(diagnostics_handler))
        .route("/api/transmissions", get(transmissions_handler))
        .route("/api/logs", get(logs_handler))
        .layer(tower_http::cors::CorsLayer::permissive())
        .with_state(state);
    #[cfg(feature = "ui")]
    {
        app = app.fallback(ui::static_handler);
    }
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// `400` naming the parameter the caller got wrong.
fn bad_parameter(parameter: &str, detail: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": format!("{parameter}: {detail}"), "parameter": parameter })),
    )
        .into_response()
}

/// Resolve the page size, rejecting a value outside the supported range
/// instead of silently clamping it.
fn resolve_limit(limit: Option<i64>) -> std::result::Result<i64, String> {
    match limit {
        None => Ok(storage_diagnostics::DEFAULT_READ_LIMIT),
        Some(limit) if (1..=storage_diagnostics::MAX_READ_LIMIT).contains(&limit) => Ok(limit),
        Some(_) => Err(format!(
            "must be between 1 and {}",
            storage_diagnostics::MAX_READ_LIMIT
        )),
    }
}

/// Query parameters for `GET /api/transmissions`.
///
/// The three paging fields are repeated in [`LogParams`] rather than shared
/// through `#[serde(flatten)]`: flattening routes every field through
/// `deserialize_any`, and a query string only ever yields strings, so each
/// numeric parameter would be rejected as `invalid type: string "100",
/// expected i64`.
#[derive(Deserialize)]
struct TransmissionParams {
    /// Return only entries newer than this cursor.
    since: Option<i64>,
    /// Return only entries older than this cursor, to page backwards.
    before: Option<i64>,
    /// Page size; defaults to 100 and is capped at 1000.
    limit: Option<i64>,
    /// Restrict to `ok`, `empty` or `failed`.
    outcome: Option<String>,
    /// Restrict to one collector target.
    target: Option<String>,
    /// Restrict to entries addressing or answered by one inverter.
    serial: Option<u32>,
}

/// Query parameters for `GET /api/logs`.
///
/// See [`TransmissionParams`] for why the paging fields are not shared.
#[derive(Deserialize)]
struct LogParams {
    /// Return only records newer than this cursor.
    since: Option<i64>,
    /// Return only records older than this cursor, to page backwards.
    before: Option<i64>,
    /// Page size; defaults to 100 and is capped at 1000.
    limit: Option<i64>,
    /// Return this level and everything more severe.
    level: Option<String>,
    /// Restrict to targets starting with this value.
    target: Option<String>,
}

/// The window a ring is actually showing, next to the window it is configured
/// for — a row cap reached before the window elapses shortens the history, and
/// the difference has to be visible rather than implied.
fn ring_envelope(
    ring: RingBounds,
    stats: storage_diagnostics::RingStats,
    cursor: Option<i64>,
    dropped: u64,
) -> Value {
    json!({
        "cursor": cursor,
        "retentionHours": ring.retention.map_or(0, |window| window.as_secs() / 3_600),
        "maxEntries": ring.max_rows,
        "retained": stats.retained,
        "oldestOccurredAt": stats.oldest_occurred_at_ms,
        "dropped": dropped,
    })
}

/// `GET /api/transmissions` — one keyset page of recorded exchanges.
async fn transmissions_handler(
    State(state): State<ApiState>,
    Query(params): Query<TransmissionParams>,
) -> Response {
    let limit = match resolve_limit(params.limit) {
        Ok(limit) => limit,
        Err(detail) => return bad_parameter("limit", &detail),
    };
    if let Some(outcome) = &params.outcome {
        if !matches!(outcome.as_str(), "ok" | "empty" | "failed") {
            return bad_parameter("outcome", "must be one of ok, empty, failed");
        }
    }
    if !state.transmission_ring.enabled() {
        return Json(json!({
            "entries": [],
            "envelope": ring_envelope(
                state.transmission_ring,
                storage_diagnostics::RingStats::default(),
                None,
                0,
            ),
        }))
        .into_response();
    }

    let filter = storage_diagnostics::TransmissionFilter {
        since: params.since,
        before: params.before,
        limit,
        outcome: params.outcome.clone(),
        target: params.target.clone(),
        serial: params.serial,
    };
    let entries = match state.db.read_transmissions(&filter).await {
        Ok(entries) => entries,
        Err(error) => return api_error(error),
    };
    let stats = match state.db.diagnostics_stats().await {
        Ok(stats) => stats.transmissions,
        Err(error) => return api_error(error),
    };
    let cursor = entries.first().map(|entry| entry.sequence);
    let dropped = state.diagnostics.dropped();
    let entries: Vec<Value> = entries.iter().map(transmission_json).collect();
    Json(json!({
        "entries": entries,
        "envelope": ring_envelope(state.transmission_ring, stats, cursor, dropped),
    }))
    .into_response()
}

fn transmission_json(entry: &storage_diagnostics::StoredTransmission) -> Value {
    let row = &entry.row;
    json!({
        "sequence": entry.sequence,
        "occurredAt": row.occurred_at_ms,
        "target": row.target,
        "transport": row.transport,
        "protocol": row.protocol,
        "requestKind": row.request_kind,
        "command": row.command,
        "firstLri": row.first_lri,
        "lastLri": row.last_lri,
        "durationMs": row.duration_ms,
        "totalFrames": row.total_frames,
        "outcome": row.outcome,
        "error": row.error,
        "detail": row.detail,
        "devices": row.devices.iter().map(|device| json!({
            "serial": device.serial_number,
            "frames": device.frame_count,
            "addressed": device.addressed,
        })).collect::<Vec<_>>(),
    })
}

/// `GET /api/logs` — one page of captured log records.
///
/// Served from process memory, not the database: the ring lives in
/// [`crate::applog::LogBuffer`]. The response shape matches
/// `/api/transmissions` so both System tabs share one client.
async fn logs_handler(State(state): State<ApiState>, Query(params): Query<LogParams>) -> Response {
    let limit = match resolve_limit(params.limit) {
        Ok(limit) => limit,
        Err(detail) => return bad_parameter("limit", &detail),
    };
    let min_level = match params.level.as_deref().map(LogLevel::parse) {
        None => None,
        Some(Some(level)) => Some(level),
        Some(None) => {
            return bad_parameter("level", "must be one of error, warn, info, debug, trace")
        }
    };

    let page = state.log_buffer.read(&LogQuery {
        since: params.since.and_then(|value| u64::try_from(value).ok()),
        before: params.before.and_then(|value| u64::try_from(value).ok()),
        limit: usize::try_from(limit).unwrap_or(1),
        min_level,
        target_prefix: params.target.clone(),
    });

    let entries: Vec<Value> = page
        .records
        .iter()
        .map(|record| {
            json!({
                "sequence": record.sequence,
                "occurredAt": record.occurred_at_ms,
                "level": record.level.as_str(),
                "target": record.target,
                "message": record.message,
                "fields": record.fields,
            })
        })
        .collect();
    let cursor = page.records.first().map(|record| record.sequence);
    Json(json!({
        "entries": entries,
        "envelope": {
            "cursor": cursor,
            "retentionHours": state.log_buffer.retention_hours(),
            "maxEntries": state.log_buffer.max_records(),
            "retained": page.stats.retained,
            "oldestOccurredAt": page.stats.oldest_occurred_at_ms,
            "dropped": page.stats.dropped,
            // The ring's cursors restart with the process. A client holding a
            // cursor from before a restart is told so, instead of silently
            // never receiving another record.
            "reset": page.reset,
        },
    }))
    .into_response()
}

fn api_error(e: impl std::fmt::Display) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": e.to_string() })),
    )
        .into_response()
}

/// `GET /status`, `GET /api/status` — live per-inverter snapshot.
async fn status_handler(State(state): State<ApiState>) -> Json<Value> {
    let st = state.status.read().await;
    let inverters: Vec<Value> = st
        .inverters
        .iter()
        .map(|i| {
            json!({
                "serial": i.serial,
                "name": i.name,
                "totalPac": i.total_pac,
                "eToday": i.e_today,
                "eTotal": i.e_total,
                "status": i.status,
            })
        })
        .collect();
    Json(json!({
        "version": crate::VERSION,
        "lastPoll": st.last_poll,
        "lastError": st.last_error,
        "isLight": st.is_light,
        "inverters": inverters,
    }))
}

/// `GET /api/inverters` — the inverters known to the database (for the UI's
/// filter selector).
async fn inverters_handler(State(state): State<ApiState>) -> Response {
    match state.db.inverters().await {
        Ok(list) => Json(
            list.into_iter()
                .map(|(serial, name)| json!({ "serial": serial, "name": name }))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => api_error(e),
    }
}

/// `GET /api/history` — a labelled, multi-series dataset for the chart.
async fn history_handler(
    State(state): State<ApiState>,
    Query(params): Query<HistoryParams>,
) -> Response {
    let range = params.range.as_deref().unwrap_or("day");
    let date = match parse_history_date(params.date.as_deref()) {
        Ok(date) => date,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
        }
    };
    let month = match parse_history_month(params.month.as_deref()) {
        Ok(month) => month,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
        }
    };
    let week = match parse_history_week(params.week.as_deref()) {
        Ok(week) => week,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
        }
    };
    let year = match parse_history_year(params.year.as_deref()) {
        Ok(year) => year,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
        }
    };
    match build_history(
        &state.db,
        state.tz,
        range,
        HistoryPeriod {
            date,
            week,
            month,
            year,
        },
        params.serial,
        params.strings,
    )
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(e) => api_error(e),
    }
}

/// `GET /api/diagnostics` — electrical samples, lifetime device details and
/// recent warning/fault events for the selected day and inverter scope.
async fn diagnostics_handler(
    State(state): State<ApiState>,
    Query(params): Query<DiagnosticsParams>,
) -> Response {
    use chrono::{TimeZone, Timelike};

    let date = match parse_history_date(params.date.as_deref()) {
        Ok(date) => date.unwrap_or_else(|| Utc::now().with_timezone(&state.tz).date_naive()),
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response();
        }
    };
    let start = local_day_start(state.tz, date);
    let samples = match state
        .db
        .diagnostic_samples(start, start + 86_400, params.serial)
        .await
    {
        Ok(samples) => samples,
        Err(error) => return api_error(error),
    };
    let latest_samples = match state.db.latest_diagnostic_samples(params.serial).await {
        Ok(samples) => samples,
        Err(error) => return api_error(error),
    };
    let details = match state.db.inverter_details(params.serial).await {
        Ok(details) => details,
        Err(error) => return api_error(error),
    };
    let events = match state.db.diagnostic_events(params.serial).await {
        Ok(events) => events,
        Err(error) => return api_error(error),
    };

    let inverters = details
        .into_iter()
        .map(|detail| {
            let inverter_samples = samples
                .iter()
                .filter(|sample| sample.serial == detail.serial)
                .collect::<Vec<_>>();
            let latest_sample = latest_samples
                .iter()
                .find(|sample| sample.serial == detail.serial);
            let tracker_count = inverter_samples
                .iter()
                .copied()
                .chain(latest_sample)
                .flat_map(|sample| sample.mppts.iter().map(|mppt| mppt.tracker_number))
                .collect::<BTreeSet<_>>()
                .len();
            let efficiencies = inverter_samples
                .iter()
                .filter_map(|sample| diagnostic_efficiency(sample.pac, diagnostic_dc_power(sample)))
                .collect::<Vec<_>>();
            let average_efficiency = (!efficiencies.is_empty())
                .then(|| efficiencies.iter().sum::<f64>() / efficiencies.len() as f64);
            let rows = inverter_samples
                .iter()
                .filter_map(|sample| {
                    let timestamp = state.tz.timestamp_opt(sample.timestamp, 0).single()?;
                    Some(json!({
                        "timestamp": sample.timestamp,
                        "label": format!("{:02}:{:02}", timestamp.hour(), timestamp.minute()),
                        "mppts": diagnostic_mppts_json(sample),
                        "acPower": sample.pac,
                        "acVoltage": sample.uac,
                        "acCurrent": sample.iac,
                        "frequency": sample.frequency,
                        "efficiency": diagnostic_efficiency(sample.pac, diagnostic_dc_power(sample)),
                        "signal": (sample.bt_signal > 0.0).then_some(sample.bt_signal),
                        "status": sample.status,
                    }))
                })
                .collect::<Vec<_>>();
            let latest_measurement = latest_sample.and_then(|sample| {
                let timestamp = state.tz.timestamp_opt(sample.timestamp, 0).single()?;
                Some(json!({
                    "timestamp": sample.timestamp,
                    "label": format!("{:02}:{:02}", timestamp.hour(), timestamp.minute()),
                    "mppts": diagnostic_mppts_json(sample),
                    "acPower": sample.pac,
                    "acVoltage": sample.uac,
                    "acCurrent": sample.iac,
                    "frequency": sample.frequency,
                    "efficiency": diagnostic_efficiency(sample.pac, diagnostic_dc_power(sample)),
                    "signal": (sample.bt_signal > 0.0).then_some(sample.bt_signal),
                    "status": sample.status,
                }))
            });

            json!({
                "serial": detail.serial,
                "name": if detail.name.is_empty() {
                    format!("#{}", detail.serial)
                } else {
                    detail.name
                },
                "model": detail.model,
                "firmware": detail.firmware,
                "status": detail.status,
                "totalEnergy": detail.total_energy_wh as f64 / 1000.0,
                "operatingTime": detail.operating_time_hours,
                "feedInTime": detail.feed_in_time_hours,
                "trackerCount": tracker_count,
                "averageEfficiency": average_efficiency,
                "latestMeasurement": latest_measurement,
                "rows": rows,
            })
        })
        .collect::<Vec<_>>();
    let events = events
        .into_iter()
        .map(|event| {
            json!({
                "timestamp": event.timestamp,
                "serial": event.serial,
                "code": event.event_code,
                "type": event.event_type,
                "category": event.category,
                "group": event.event_group,
                "message": event.tag,
                "oldValue": event.old_value,
                "newValue": event.new_value,
            })
        })
        .collect::<Vec<_>>();

    Json(json!({
        "date": date.to_string(),
        "inverters": inverters,
        "events": events,
    }))
    .into_response()
}

fn diagnostic_efficiency(ac_power: i32, dc_power: i32) -> Option<f64> {
    if dc_power < 500 || ac_power < 0 {
        return None;
    }
    let efficiency = ac_power as f64 / dc_power as f64 * 100.0;
    (efficiency <= 110.0).then_some(efficiency)
}

fn diagnostic_dc_power(sample: &crate::storage::DiagnosticSample) -> i32 {
    sample
        .mppts
        .iter()
        .filter_map(|mppt| mppt.dc_power_w)
        .fold(0, i32::saturating_add)
}

fn diagnostic_mppts_json(sample: &crate::storage::DiagnosticSample) -> Vec<DiagnosticMpptResponse> {
    let mut mppts = sample
        .mppts
        .iter()
        .map(|mppt| DiagnosticMpptResponse {
            tracker_number: mppt.tracker_number,
            dc_power_w: mppt.dc_power_w,
            dc_current_ma: mppt.dc_current_ma,
            dc_voltage_mv: mppt.dc_voltage_mv,
        })
        .collect::<Vec<_>>();
    mppts.sort_unstable_by_key(|mppt| mppt.tracker_number);
    mppts
}

fn parse_history_date(raw: Option<&str>) -> std::result::Result<Option<NaiveDate>, String> {
    raw.map(|value| {
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| "date must use YYYY-MM-DD format".to_string())
    })
    .transpose()
}

fn parse_history_month(raw: Option<&str>) -> std::result::Result<Option<NaiveDate>, String> {
    raw.map(|value| {
        NaiveDate::parse_from_str(&format!("{value}-01"), "%Y-%m-%d")
            .map_err(|_| "month must use YYYY-MM format".to_string())
    })
    .transpose()
}

fn parse_history_week(raw: Option<&str>) -> std::result::Result<Option<NaiveDate>, String> {
    raw.map(|value| {
        NaiveDate::parse_from_str(value, "%Y-%m-%d")
            .map_err(|_| "week must use YYYY-MM-DD format".to_string())
    })
    .transpose()
}

fn parse_history_year(raw: Option<&str>) -> std::result::Result<Option<i32>, String> {
    raw.map(|value| {
        if value.len() != 4 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("year must use YYYY format".to_string());
        }
        value
            .parse::<i32>()
            .map_err(|_| "year must use YYYY format".to_string())
    })
    .transpose()
}

/// Build the history dataset: `day` → per-5-minute power (kW); `week`/`month`
/// → per-day yield (kWh); `year` → cumulative yield per calendar year (kWh).
/// `serial` filters to one inverter (omit for the aggregate); `date`, `week`,
/// `month`, and `year` select a local calendar period. `strings` (day + serial)
/// splits DC power per string. The result is `{ range, unit, keys, rows }`
/// where each row is `{ label, <key>: value, … }` ready for the chart.
async fn build_history(
    db: &Db,
    tz: Tz,
    range: &str,
    period: HistoryPeriod,
    serial: Option<u32>,
    strings: bool,
) -> Result<Value> {
    use chrono::{Datelike, Duration, TimeZone, Timelike};

    let today = Utc::now().with_timezone(&tz).date_naive();
    let selected_day = period.date.unwrap_or(today);
    let current_week_start = week_start(today);
    let selected_week_start = period.week.map(week_start).unwrap_or(current_week_start);
    let current_month = today.with_day(1).unwrap();
    let selected_month = period.month.unwrap_or(current_month);
    let current_year = today.year();
    let selected_year = period.year.unwrap_or(current_year);
    let daily_statistics = if range == "day" {
        db.daily_statistics(selected_day, serial).await?
    } else {
        None
    };

    // Per-string DC power for one inverter (day view): arbitrary sparse MPPTs.
    if range == "day" && strings {
        let Some(serial) = serial else {
            let mut response = json!({
                "range": range,
                "date": selected_day.to_string(),
                "today": today.to_string(),
                "live": selected_day == today,
                "unit": "W",
                "keys": [],
                "rows": [],
            });
            attach_daily_statistics(&mut response, daily_statistics.as_deref());
            return Ok(response);
        };
        let start = local_day_start(tz, selected_day);
        let samples = db.spot_strings(serial, start, start + 86_400).await?;
        let trackers = samples
            .iter()
            .flat_map(|(_, mppts)| mppts.iter().map(|(tracker, _)| *tracker))
            .collect::<BTreeSet<_>>();
        let keys = trackers
            .iter()
            .map(|tracker| format!("String {tracker}"))
            .collect::<Vec<_>>();
        let rows: Vec<Value> = samples
            .into_iter()
            .filter_map(|(ts, mppts)| {
                let t = tz.timestamp_opt(ts, 0).single()?;
                let mut row = Map::new();
                row.insert(
                    "label".into(),
                    json!(format!("{:02}:{:02}", t.hour(), t.minute())),
                );
                for (tracker, power) in mppts {
                    row.insert(format!("String {tracker}"), json!(power));
                }
                Some(Value::Object(row))
            })
            .collect();
        let mut response = json!({
            "range": range,
            "date": selected_day.to_string(),
            "today": today.to_string(),
            "live": selected_day == today,
            "unit": "W",
            "keys": keys,
            "rows": rows,
        });
        attach_daily_statistics(&mut response, daily_statistics.as_deref());
        return Ok(response);
    }

    // Single series (aggregate across inverters, or one inverter).
    let (unit, buckets): (&str, BTreeMap<String, f64>) = match range {
        "day" => {
            let start = local_day_start(tz, selected_day);

            if serial.is_none() {
                let mut merged_rows = BTreeMap::new();
                let mut keys = Vec::new();
                let mut series = Vec::new();
                let total_power_key = "total_power".to_string();
                let total_energy_key = "total_energy".to_string();
                let total_rows = build_day_metric_rows(
                    db.day_metrics(start, start + 86_400, None).await?,
                    db.day_power(start, start + 86_400, None).await?,
                    tz,
                );

                merge_inverter_day_rows(
                    &mut merged_rows,
                    total_rows,
                    &total_power_key,
                    &total_energy_key,
                );
                keys.extend([total_power_key.clone(), total_energy_key.clone()]);
                series.extend([
                    json!({
                        "key": total_power_key.clone(),
                        "label": "Total Power",
                        "metric": "power",
                        "aggregate": true,
                    }),
                    json!({
                        "key": total_energy_key.clone(),
                        "label": "Total Energy",
                        "metric": "energy",
                        "aggregate": true,
                    }),
                ]);

                for (inverter_serial, inverter_name) in db.inverters().await? {
                    let display_name = if inverter_name.is_empty() {
                        format!("#{inverter_serial}")
                    } else {
                        inverter_name
                    };
                    let power_key = format!("inverter_{inverter_serial}_power");
                    let energy_key = format!("inverter_{inverter_serial}_energy");
                    let rows = build_day_metric_rows(
                        db.day_metrics(start, start + 86_400, Some(inverter_serial))
                            .await?,
                        db.day_power(start, start + 86_400, Some(inverter_serial))
                            .await?,
                        tz,
                    );

                    merge_inverter_day_rows(&mut merged_rows, rows, &power_key, &energy_key);
                    keys.extend([power_key.clone(), energy_key.clone()]);
                    series.extend([
                        json!({
                            "key": power_key,
                            "label": format!("{display_name} Power"),
                            "metric": "power",
                            "aggregate": false,
                        }),
                        json!({
                            "key": energy_key,
                            "label": format!("{display_name} Energy"),
                            "metric": "energy",
                            "aggregate": false,
                        }),
                    ]);
                }

                let rows = merged_rows
                    .into_values()
                    .map(Value::Object)
                    .collect::<Vec<_>>();
                let previous_energy = previous_day_energy(db, tz, selected_day, serial).await?;
                let summary =
                    build_day_summary(&rows, &total_power_key, &total_energy_key, previous_energy);
                let mut response = json!({
                    "range": range,
                    "date": selected_day.to_string(),
                    "today": today.to_string(),
                    "live": selected_day == today,
                    "unit": "mixed",
                    "keys": keys,
                    "series": series,
                    "summary": summary,
                    "rows": rows,
                });
                attach_daily_statistics(&mut response, daily_statistics.as_deref());
                return Ok(response);
            }

            let samples = db.day_metrics(start, start + 86_400, serial).await?;
            let power = db.day_power(start, start + 86_400, serial).await?;
            let rows = build_day_metric_rows(samples, power, tz);
            let previous_energy = previous_day_energy(db, tz, selected_day, serial).await?;
            let summary = build_day_summary(&rows, "power", "energy", previous_energy);

            let mut response = json!({
                "range": range,
                "date": selected_day.to_string(),
                "today": today.to_string(),
                "live": selected_day == today,
                "unit": "mixed",
                "keys": ["power", "energy", "temperature"],
                "series": [
                    { "key": "power", "label": "Power", "metric": "power" },
                    { "key": "energy", "label": "Energy Generated", "metric": "energy" },
                    { "key": "temperature", "label": "Temperature", "metric": "temperature" },
                ],
                "summary": summary,
                "rows": rows,
            });
            attach_daily_statistics(&mut response, daily_statistics.as_deref());
            return Ok(response);
        }
        "week" => {
            let start_date = selected_week_start;
            let end_date = start_date + Duration::days(7);
            (
                "kWh",
                daily_energy_buckets(db, tz, start_date, end_date, serial).await?,
            )
        }
        "month" => {
            let start_date = selected_month;
            let end_date = add_month(start_date);
            (
                "kWh",
                daily_energy_buckets(db, tz, start_date, end_date, serial).await?,
            )
        }
        "year" => {
            let mut buckets = year_energy_buckets(db, tz, selected_year, serial).await?;
            complete_year_buckets(selected_year, &mut buckets);
            ("kWh", buckets)
        }
        _ => ("", BTreeMap::new()),
    };

    // Series key: "Total" for the aggregate, else the inverter's name.
    let key = match serial {
        None => "Total".to_string(),
        Some(s) => db
            .inverters()
            .await?
            .into_iter()
            .find(|(serial, _)| *serial == s)
            .map(|(_, name)| {
                if name.is_empty() {
                    format!("#{s}")
                } else {
                    name
                }
            })
            .unwrap_or_else(|| format!("#{s}")),
    };
    let rows: Vec<Value> = buckets
        .into_iter()
        .map(|(label, value)| {
            let mut row = Map::new();
            row.insert("label".into(), json!(label));
            row.insert(key.clone(), json!(value));
            Value::Object(row)
        })
        .collect();
    let mut response = json!({ "range": range, "unit": unit, "keys": [key], "rows": rows });
    if range == "day" {
        response["date"] = json!(selected_day.to_string());
        response["today"] = json!(today.to_string());
        response["live"] = json!(selected_day == today);
    } else if range == "week" {
        let previous_start = selected_week_start - Duration::days(7);
        let previous_end =
            previous_week_comparison_end(selected_week_start, current_week_start, today);
        let previous_total = daily_energy_buckets(db, tz, previous_start, previous_end, serial)
            .await?
            .values()
            .sum();
        response["summary"] = build_week_summary(&buckets_from_rows(&response), previous_total);
        response["weekStart"] = json!(selected_week_start.to_string());
        response["weekEnd"] = json!((selected_week_start + Duration::days(6)).to_string());
        response["currentWeekStart"] = json!(current_week_start.to_string());
    } else if range == "month" {
        let start_date = selected_month;
        let previous_start = previous_month(start_date);
        let previous_end = previous_month_comparison_end(start_date, current_month, today);
        let previous_total = daily_energy_buckets(db, tz, previous_start, previous_end, serial)
            .await?
            .values()
            .sum();
        response["summary"] = build_month_summary(&buckets_from_rows(&response), previous_total);
        response["month"] = json!(start_date.format("%Y-%m").to_string());
        response["currentMonth"] = json!(current_month.format("%Y-%m").to_string());
    } else if range == "year" {
        let previous_year = selected_year - 1;
        let previous_start =
            NaiveDate::from_ymd_opt(previous_year, 1, 1).expect("four-digit year is supported");
        let previous_end = previous_year_comparison_end(selected_year, current_year, today);
        let previous_total = daily_energy_buckets(db, tz, previous_start, previous_end, serial)
            .await?
            .values()
            .sum();
        response["summary"] = build_year_summary(&buckets_from_rows(&response), previous_total);
        response["year"] = json!(selected_year.to_string());
        response["currentYear"] = json!(current_year.to_string());
    }
    Ok(response)
}

fn attach_daily_statistics(
    response: &mut Value,
    statistics: Option<&[crate::storage::DailyStatistics]>,
) {
    let Some(statistics) = statistics else {
        return;
    };
    response["dailyStatistics"] = Value::Array(
        statistics
            .iter()
            .map(|statistics| {
                let coverage = statistics
                    .expected_measurement_count
                    .filter(|expected| *expected > 0)
                    .map(|expected| statistics.measurement_count as f64 / expected as f64);
                json!({
                    "serial": statistics.serial,
                    "date": statistics.date.to_string(),
                    "peak": {
                        "acPower": {
                            "value": statistics.peak_ac_power_w,
                            "unit": "W",
                        },
                        "dcPower": {
                            "value": statistics.peak_dc_power_w,
                            "unit": "W",
                        },
                    },
                    "mean": {
                        "acPower": {
                            "value": statistics.mean_ac_power_w,
                            "unit": "W",
                        },
                        "dcPower": {
                            "value": statistics.mean_dc_power_w,
                            "unit": "W",
                        },
                    },
                    "measurements": {
                        "actualCount": statistics.measurement_count,
                        "expectedCount": statistics.expected_measurement_count,
                    },
                    "coverage": {
                        "ratio": coverage,
                        "firstMeasuredAt": statistics.first_measurement_at,
                        "lastMeasuredAt": statistics.last_measurement_at,
                    },
                    "complete": statistics.is_complete,
                    "sourceMaxMeasuredAt": statistics.source_max_measured_at,
                    "calculatedAt": statistics.calculated_at,
                    "stale": statistics.is_stale,
                })
            })
            .collect(),
    );
}

fn week_start(date: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::{Datelike, Duration};

    date - Duration::days(date.weekday().num_days_from_monday() as i64)
}

fn previous_week_comparison_end(
    selected_week_start: NaiveDate,
    current_week_start: NaiveDate,
    today: NaiveDate,
) -> NaiveDate {
    use chrono::{Datelike, Duration};

    if selected_week_start == current_week_start {
        selected_week_start - Duration::days(7)
            + Duration::days(today.weekday().num_days_from_monday() as i64 + 1)
    } else {
        selected_week_start
    }
}

fn previous_month_comparison_end(
    selected_month: NaiveDate,
    current_month: NaiveDate,
    today: NaiveDate,
) -> NaiveDate {
    use chrono::{Datelike, Duration};

    if selected_month != current_month {
        return selected_month;
    }
    let previous_start = previous_month(selected_month);
    let previous_last_day = add_month(previous_start) - Duration::days(1);
    previous_start
        .with_day(today.day().min(previous_last_day.day()))
        .expect("day is capped to the previous month")
        + Duration::days(1)
}

fn previous_year_comparison_end(
    selected_year: i32,
    current_year: i32,
    today: NaiveDate,
) -> NaiveDate {
    use chrono::{Datelike, Duration};

    if selected_year != current_year {
        return NaiveDate::from_ymd_opt(selected_year, 1, 1).expect("four-digit year is supported");
    }
    let previous_year = selected_year - 1;
    let same_day = NaiveDate::from_ymd_opt(previous_year, today.month(), today.day())
        .unwrap_or_else(|| {
            NaiveDate::from_ymd_opt(previous_year, today.month(), today.day() - 1)
                .expect("only February 29 needs capping")
        });
    same_day + Duration::days(1)
}

fn buckets_from_rows(response: &Value) -> BTreeMap<String, f64> {
    let Some(key) = response["keys"].as_array().and_then(|keys| keys.first()) else {
        return BTreeMap::new();
    };
    let Some(key) = key.as_str() else {
        return BTreeMap::new();
    };
    response["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| Some((row["label"].as_str()?.to_string(), row[key].as_f64()?)))
        .collect()
}

fn build_month_summary(buckets: &BTreeMap<String, f64>, previous_total: f64) -> Value {
    let total: f64 = buckets.values().sum();
    let recorded_days = buckets.len();
    let average_daily = if recorded_days == 0 {
        0.0
    } else {
        total / recorded_days as f64
    };
    let best_day = buckets
        .iter()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(label, value)| json!({ "label": label, "value": value }));
    let change_percent =
        (previous_total > 0.0).then(|| (total - previous_total) / previous_total * 100.0);

    json!({
        "total": total,
        "averageDaily": average_daily,
        "recordedDays": recorded_days,
        "bestDay": best_day,
        "previousMonthTotal": previous_total,
        "changePercent": change_percent,
    })
}

async fn previous_day_energy(
    db: &Db,
    tz: Tz,
    selected_day: NaiveDate,
    serial: Option<u32>,
) -> Result<f64> {
    use chrono::Duration;

    let previous_day = selected_day - Duration::days(1);
    Ok(
        daily_energy_buckets(db, tz, previous_day, selected_day, serial)
            .await?
            .values()
            .sum(),
    )
}

fn build_day_summary(
    rows: &[Value],
    power_key: &str,
    energy_key: &str,
    previous_energy: f64,
) -> Value {
    let power_values = rows
        .iter()
        .filter_map(|row| row[power_key].as_f64())
        .collect::<Vec<_>>();
    let peak_power = power_values.iter().copied().fold(0.0_f64, f64::max);
    let average_power = if power_values.is_empty() {
        0.0
    } else {
        power_values.iter().sum::<f64>() / power_values.len() as f64
    };
    let total_energy = rows
        .iter()
        .filter_map(|row| row[energy_key].as_f64())
        .fold(0.0_f64, f64::max);
    let change_percent =
        (previous_energy > 0.0).then(|| (total_energy - previous_energy) / previous_energy * 100.0);

    json!({
        "totalEnergy": total_energy,
        "peakPower": peak_power,
        "averagePower": average_power,
        "recordedIntervals": power_values.len(),
        "previousDayEnergy": previous_energy,
        "changePercent": change_percent,
    })
}

fn build_week_summary(buckets: &BTreeMap<String, f64>, previous_total: f64) -> Value {
    let total: f64 = buckets.values().sum();
    let recorded_days = buckets.len();
    let average_daily = if recorded_days == 0 {
        0.0
    } else {
        total / recorded_days as f64
    };
    let best_day = buckets
        .iter()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(label, value)| json!({ "label": label, "value": value }));
    let change_percent =
        (previous_total > 0.0).then(|| (total - previous_total) / previous_total * 100.0);

    json!({
        "total": total,
        "averageDaily": average_daily,
        "recordedDays": recorded_days,
        "bestDay": best_day,
        "previousWeekTotal": previous_total,
        "changePercent": change_percent,
    })
}

fn build_year_summary(buckets: &BTreeMap<String, f64>, previous_total: f64) -> Value {
    let total: f64 = buckets.values().sum();
    let recorded_months = buckets.values().filter(|value| **value > 0.0).count();
    let average_monthly = if recorded_months == 0 {
        0.0
    } else {
        total / recorded_months as f64
    };
    let best_month = buckets
        .iter()
        .filter(|(_, value)| **value > 0.0)
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map(|(label, value)| json!({ "label": label, "value": value }));
    let change_percent =
        (previous_total > 0.0).then(|| (total - previous_total) / previous_total * 100.0);

    json!({
        "total": total,
        "averageMonthly": average_monthly,
        "recordedMonths": recorded_months,
        "bestMonth": best_month,
        "previousYearTotal": previous_total,
        "changePercent": change_percent,
    })
}

async fn daily_energy_buckets(
    db: &Db,
    tz: Tz,
    start_date: NaiveDate,
    end_date: NaiveDate,
    serial: Option<u32>,
) -> Result<BTreeMap<String, f64>> {
    let start = local_day_start(tz, start_date);
    let end = local_day_start(tz, end_date);
    use chrono::TimeZone;

    let mut buckets = BTreeMap::new();
    for (timestamp, _serial, daily_energy_wh) in db.daily_yield(start, end, serial).await? {
        let Some(local_time) = tz.timestamp_opt(timestamp, 0).single() else {
            continue;
        };
        *buckets
            .entry(local_time.format("%Y-%m-%d").to_string())
            .or_default() += daily_energy_wh as f64 / 1000.0;
    }
    Ok(buckets)
}

async fn year_energy_buckets(
    db: &Db,
    tz: Tz,
    year: i32,
    serial: Option<u32>,
) -> Result<BTreeMap<String, f64>> {
    let start_date = NaiveDate::from_ymd_opt(year, 1, 1).expect("four-digit year is supported");
    let end_date = NaiveDate::from_ymd_opt(year + 1, 1, 1).expect("four-digit year is supported");
    let start = local_day_start(tz, start_date);
    let end = local_day_start(tz, end_date);
    use chrono::TimeZone;

    let mut buckets = BTreeMap::new();
    for (timestamp, _serial, daily_energy_wh) in db.daily_yield(start, end, serial).await? {
        let Some(local_time) = tz.timestamp_opt(timestamp, 0).single() else {
            continue;
        };
        *buckets
            .entry(local_time.format("%Y-%m").to_string())
            .or_default() += daily_energy_wh as f64 / 1000.0;
    }
    Ok(buckets)
}

fn complete_year_buckets(year: i32, buckets: &mut BTreeMap<String, f64>) {
    for month in 1..=12 {
        buckets.entry(format!("{year:04}-{month:02}")).or_default();
    }
}

type DayMetricSample = (i64, u32, i64, i64, Option<f32>);
type DayPowerSample = (i64, u32, i64);
type DeviceMetric = (i64, i64, Option<f32>);
type DeviceMetricSlots = BTreeMap<i64, BTreeMap<u32, DeviceMetric>>;

fn merge_inverter_day_rows(
    merged: &mut BTreeMap<String, Map<String, Value>>,
    rows: Vec<Value>,
    power_key: &str,
    energy_key: &str,
) {
    for row in rows {
        let Value::Object(row) = row else {
            continue;
        };
        let Some(label) = row.get("label").and_then(Value::as_str) else {
            continue;
        };
        let target = merged.entry(label.to_string()).or_insert_with(|| {
            let mut row = Map::new();
            row.insert("label".into(), json!(label));
            row
        });
        if let Some(power) = row.get("power") {
            target.insert(power_key.into(), power.clone());
        }
        if let Some(energy) = row.get("energy") {
            target.insert(energy_key.into(), energy.clone());
        }
    }
}

fn build_day_metric_rows(
    samples: Vec<DayMetricSample>,
    power_samples: Vec<DayPowerSample>,
    tz: Tz,
) -> Vec<Value> {
    use chrono::{TimeZone, Timelike};

    // Keep the latest sample per inverter in each five-minute slot, then
    // combine devices. Poll timestamps differ by seconds, so grouping by the
    // raw timestamp would split one plant point.
    let mut device_slots = DeviceMetricSlots::new();
    for (ts, device, power, energy, temperature) in samples {
        let remainder = ts.rem_euclid(300);
        let slot = ts - remainder + if remainder >= 150 { 300 } else { 0 };
        device_slots
            .entry(slot)
            .or_default()
            .insert(device, (power, energy, temperature));
    }

    // DayData is the inverter's aligned five-minute archive and produces a
    // stable power curve even when an individual spot query was missed.
    let mut archive_power: BTreeMap<i64, i64> = BTreeMap::new();
    for (ts, _device, power) in power_samples {
        *archive_power.entry(ts).or_default() += power;
    }

    #[derive(Default)]
    struct PlantSlot {
        power_w: i64,
        energy_wh: i64,
        temperature_sum: f64,
        temperature_count: u32,
    }

    struct LastSample {
        slot: i64,
        power_w: i64,
        energy_wh: i64,
        temperature: Option<f32>,
    }

    let mut plant_slots: BTreeMap<i64, PlantSlot> = BTreeMap::new();
    let mut latest: BTreeMap<u32, LastSample> = BTreeMap::new();
    for (slot, samples) in device_slots {
        for (device, (power, energy, temperature)) in samples {
            let energy = latest
                .get(&device)
                .map(|previous| previous.energy_wh.max(energy))
                .unwrap_or(energy);
            latest.insert(
                device,
                LastSample {
                    slot,
                    power_w: power,
                    energy_wh: energy,
                    temperature,
                },
            );
        }

        let point = plant_slots.entry(slot).or_default();
        for sample in latest.values() {
            // EToday is cumulative, so it remains part of the plant total
            // after an inverter stops answering near sunset.
            point.energy_wh += sample.energy_wh;

            // Instantaneous values may bridge one missed poll only.
            if slot - sample.slot <= 300 {
                point.power_w += sample.power_w;
                if let Some(temperature) = sample.temperature {
                    point.temperature_sum += temperature as f64;
                    point.temperature_count += 1;
                }
            }
        }
    }

    plant_slots
        .into_iter()
        .filter_map(|(ts, point)| {
            let t = tz.timestamp_opt(ts, 0).single()?;
            let temperature = (point.temperature_count > 0)
                .then(|| point.temperature_sum / point.temperature_count as f64);
            let power_w = archive_power.get(&ts).copied().unwrap_or(point.power_w);
            Some(json!({
                "label": format!("{:02}:{:02}", t.hour(), t.minute()),
                "power": power_w as f64 / 1000.0,
                "energy": point.energy_wh as f64 / 1000.0,
                "temperature": temperature,
            }))
        })
        .collect()
}

/// The embedded web UI (built with `--features ui`). Assets come from
/// `src/ui/dist`; unknown paths fall back to `index.html` (SPA routing).
#[cfg(feature = "ui")]
mod ui {
    use axum::http::{header, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use rust_embed::RustEmbed;

    #[derive(RustEmbed)]
    #[folder = "../../ui/dist"]
    struct Assets;

    pub async fn static_handler(uri: Uri) -> Response {
        let path = uri.path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };
        match Assets::get(path).or_else(|| Assets::get("index.html")) {
            Some(file) => {
                let mime = file.metadata.mimetype().to_string();
                ([(header::CONTENT_TYPE, mime)], file.data).into_response()
            }
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

/// Epoch seconds of local midnight for `date` in `tz`.
fn local_day_start(tz: Tz, date: chrono::NaiveDate) -> i64 {
    use chrono::TimeZone;
    tz.from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map(|d| d.timestamp())
        .unwrap_or(0)
}

/// First day of the month after `date`'s month.
fn add_month(date: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    if date.month() == 12 {
        chrono::NaiveDate::from_ymd_opt(date.year() + 1, 1, 1).unwrap()
    } else {
        chrono::NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1).unwrap()
    }
}

/// First day of the month before `date`'s month.
fn previous_month(date: chrono::NaiveDate) -> chrono::NaiveDate {
    use chrono::Datelike;
    if date.month() == 1 {
        chrono::NaiveDate::from_ymd_opt(date.year() - 1, 12, 1).unwrap()
    } else {
        chrono::NaiveDate::from_ymd_opt(date.year(), date.month() - 1, 1).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{NaiveDate, Utc};
    use chrono_tz::Tz;

    use std::collections::BTreeMap;
    use std::sync::Arc;

    use axum::body::to_bytes;
    use axum::extract::{Query, State};
    use serde_json::{json, Map, Value};

    use crate::config::Config;
    use crate::domain::{
        CanonicalText, InverterIdentity, InverterMeasurement, MilliVolts, Milliamperes,
        MpptMeasurement, UnixSeconds, Watts,
    };
    use crate::storage::{local_day_utc_bounds, Db};
    use smalog_observation::{
        ArchiveOutcome, CommunicationIdentity, InverterPollObservation, LiveObservation,
        LiveOutcome, ProtocolFamily, Transport,
    };

    use super::{
        browsable_base_url, build_day_metric_rows, build_day_summary, build_month_summary,
        build_week_summary, build_year_summary, complete_year_buckets, diagnostic_efficiency,
        diagnostics_handler, history_handler, inverter_target, log_http_endpoints,
        merge_inverter_day_rows, parse_history_date, parse_history_month, parse_history_week,
        parse_history_year, previous_month_comparison_end, previous_week_comparison_end,
        previous_year_comparison_end, resolve_limit, storage_diagnostics, transmissions_handler,
        week_start, ApiState, DiagnosticsParams, HistoryParams, LogParams, RingBounds, Service,
        TransmissionParams,
    };
    use super::{logs_handler, LogBuffer, LogLevel, Response, StatusCode};

    #[test]
    fn wildcard_binds_are_reported_as_loopback() {
        assert_eq!(
            browsable_base_url("0.0.0.0:8080".parse().unwrap()),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            browsable_base_url("[::]:8080".parse().unwrap()),
            "http://[::1]:8080"
        );
    }

    #[test]
    fn endpoint_log_renders_openable_urls() {
        #[derive(Clone, Default)]
        struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);

        impl std::io::Write for Buffer {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_http_endpoints("0.0.0.0:8080".parse().unwrap())
        });
        let logged = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();

        assert!(
            logged.contains("url=http://127.0.0.1:8080/healthz"),
            "{logged}"
        );
        assert!(
            logged.contains("url=http://127.0.0.1:8080/status"),
            "{logged}"
        );
        assert!(!logged.contains("http://0.0.0.0:8080"), "{logged}");
        let ui_url = if cfg!(feature = "ui") {
            "url=http://127.0.0.1:8080/"
        } else {
            "url=http://127.0.0.1:8080/api/status"
        };
        assert!(logged.contains(ui_url), "{logged}");
    }

    #[test]
    fn concrete_binds_keep_their_address() {
        assert_eq!(
            browsable_base_url("192.168.1.50:9000".parse().unwrap()),
            "http://192.168.1.50:9000"
        );
        assert_eq!(
            browsable_base_url("[fe80::1]:9000".parse().unwrap()),
            "http://[fe80::1]:9000"
        );
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    #[test]
    fn history_date_requires_iso_calendar_date() {
        assert_eq!(
            parse_history_date(Some("2026-07-25"))
                .expect("valid date")
                .expect("date present")
                .to_string(),
            "2026-07-25"
        );
        assert!(parse_history_date(Some("25.07.2026")).is_err());
        assert!(parse_history_date(Some("2026-02-30")).is_err());
        assert_eq!(parse_history_date(None).expect("optional date"), None);
    }

    /// A database with the diagnostics rings enabled and `count` recorded
    /// transmissions, oldest first.
    async fn seeded_rings(directory: &tempfile::TempDir, count: i64) -> Db {
        let database_url = format!("sqlite://{}", directory.path().join("rings.db").display());
        let db = Db::connect(&database_url, Tz::UTC).await.unwrap();
        db.enable_diagnostics().await.unwrap();
        let rows: Vec<storage_diagnostics::TransmissionRow> = (0..count)
            .map(|i| storage_diagnostics::TransmissionRow {
                occurred_at_ms: 1_000 + i,
                target: if i % 2 == 0 { "eth" } else { "bt" }.to_owned(),
                transport: "ethernet".to_owned(),
                protocol: "sma_data_2_plus".to_owned(),
                request_kind: "spot.ac_power".to_owned(),
                command: Some(0x5100_0200),
                first_lri: Some(0x0046_4000),
                last_lri: Some(0x0046_42FF),
                duration_ms: 5,
                total_frames: 1,
                outcome: if i == 0 { "failed" } else { "ok" }.to_owned(),
                error: (i == 0).then(|| "timeout".to_owned()),
                detail: None,
                devices: vec![storage_diagnostics::TransmissionDeviceRow {
                    serial_number: 11,
                    frame_count: 1,
                    addressed: true,
                }],
            })
            .collect();
        db.write_transmissions(&rows).await.unwrap();
        db
    }

    /// A state whose log buffer already holds one info and one error record.
    fn state_with_log_records(db: Arc<Db>) -> ApiState {
        let state = ApiState::for_tests(db, Tz::UTC);
        state.log_buffer.capture_for_test(
            1_000,
            LogLevel::Info,
            "smalog::service",
            "started",
            None,
        );
        state.log_buffer.capture_for_test(
            2_000,
            LogLevel::Error,
            "smalog::service",
            "poll failed",
            Some("serial=11"),
        );
        state
    }

    fn transmission_params(
        since: Option<i64>,
        before: Option<i64>,
        limit: Option<i64>,
    ) -> TransmissionParams {
        TransmissionParams {
            since,
            before,
            limit,
            outcome: None,
            target: None,
            serial: None,
        }
    }

    fn log_params(limit: Option<i64>) -> LogParams {
        LogParams {
            since: None,
            before: None,
            limit,
            level: None,
            target: None,
        }
    }

    async fn body_json(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// The query string is where the two System endpoints actually meet the
    /// outside world, and constructing their parameter structs in Rust skips
    /// exactly that step. These parse real URIs through the same extractor
    /// axum uses, which is what a shared `#[serde(flatten)]` silently broke:
    /// every numeric parameter came back as `invalid type: string "100",
    /// expected i64`, so the dashboard's very first request failed.
    #[test]
    fn transmission_query_parameters_deserialize_from_a_real_uri() {
        let uri: axum::http::Uri =
            "/api/transmissions?since=5&before=90&limit=100&outcome=failed&target=eth&serial=42"
                .parse()
                .unwrap();
        let Query(params) = Query::<TransmissionParams>::try_from_uri(&uri)
            .expect("every supported parameter must parse");

        assert_eq!(params.since, Some(5));
        assert_eq!(params.before, Some(90));
        assert_eq!(params.limit, Some(100));
        assert_eq!(params.outcome.as_deref(), Some("failed"));
        assert_eq!(params.target.as_deref(), Some("eth"));
        assert_eq!(params.serial, Some(42));
    }

    #[test]
    fn log_query_parameters_deserialize_from_a_real_uri() {
        let uri: axum::http::Uri =
            "/api/logs?since=5&before=90&limit=100&level=warn&target=smalog::service"
                .parse()
                .unwrap();
        let Query(params) =
            Query::<LogParams>::try_from_uri(&uri).expect("every supported parameter must parse");

        assert_eq!(params.since, Some(5));
        assert_eq!(params.before, Some(90));
        assert_eq!(params.limit, Some(100));
        assert_eq!(params.level.as_deref(), Some("warn"));
        assert_eq!(params.target.as_deref(), Some("smalog::service"));
    }

    #[test]
    fn each_paging_parameter_parses_on_its_own() {
        // The bug only showed when a numeric parameter was present at all, so
        // an empty query string kept working and hid it.
        for query in ["", "since=1", "before=1", "limit=1", "since=1&limit=2"] {
            let uri: axum::http::Uri = format!("/api/transmissions?{query}").parse().unwrap();
            assert!(
                Query::<TransmissionParams>::try_from_uri(&uri).is_ok(),
                "transmissions rejected {query:?}"
            );
            let uri: axum::http::Uri = format!("/api/logs?{query}").parse().unwrap();
            assert!(
                Query::<LogParams>::try_from_uri(&uri).is_ok(),
                "logs rejected {query:?}"
            );
        }
    }

    #[test]
    fn the_dashboard_default_request_parses() {
        // What the System views send on their first load and every refresh.
        let uri: axum::http::Uri = "/api/transmissions?limit=100".parse().unwrap();
        let Query(params) = Query::<TransmissionParams>::try_from_uri(&uri).expect("first load");
        assert_eq!(resolve_limit(params.limit), Ok(100));
    }

    #[test]
    fn a_non_numeric_paging_value_is_still_rejected() {
        let uri: axum::http::Uri = "/api/transmissions?limit=lots".parse().unwrap();
        assert!(Query::<TransmissionParams>::try_from_uri(&uri).is_err());
    }

    #[test]
    fn resolve_limit_defaults_and_bounds_the_page() {
        assert_eq!(
            resolve_limit(None),
            Ok(storage_diagnostics::DEFAULT_READ_LIMIT)
        );
        assert_eq!(resolve_limit(Some(1)), Ok(1));
        assert_eq!(
            resolve_limit(Some(storage_diagnostics::MAX_READ_LIMIT)),
            Ok(storage_diagnostics::MAX_READ_LIMIT)
        );
        assert!(resolve_limit(Some(0)).is_err());
        assert!(resolve_limit(Some(storage_diagnostics::MAX_READ_LIMIT + 1)).is_err());
    }

    #[tokio::test]
    async fn transmissions_handler_returns_newest_first_with_its_envelope() {
        let directory = tempfile::tempdir().unwrap();
        let db = seeded_rings(&directory, 5).await;
        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);

        let body = body_json(
            transmissions_handler(State(state), Query(transmission_params(None, None, None))).await,
        )
        .await;

        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 5);
        assert_eq!(entries[0]["occurredAt"], 1_004);
        assert_eq!(entries[0]["requestKind"], "spot.ac_power");
        assert_eq!(entries[0]["command"], 0x5100_0200);
        assert_eq!(entries[0]["devices"][0]["serial"], 11);
        let envelope = &body["envelope"];
        assert_eq!(envelope["retained"], 5);
        assert_eq!(envelope["retentionHours"], 48);
        assert_eq!(envelope["maxEntries"], 50_000);
        assert_eq!(envelope["oldestOccurredAt"], 1_000);
        assert_eq!(envelope["dropped"], 0);
        assert_eq!(envelope["cursor"], entries[0]["sequence"]);
    }

    #[tokio::test]
    async fn transmissions_handler_pages_with_since_and_before() {
        let directory = tempfile::tempdir().unwrap();
        let db = seeded_rings(&directory, 6).await;
        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);

        let first = body_json(
            transmissions_handler(
                State(state.clone()),
                Query(transmission_params(None, None, Some(2))),
            )
            .await,
        )
        .await;
        let first_entries = first["entries"].as_array().unwrap();
        assert_eq!(first_entries.len(), 2);
        let oldest_seen = first_entries[1]["sequence"].as_i64().unwrap();

        let older = body_json(
            transmissions_handler(
                State(state.clone()),
                Query(transmission_params(None, Some(oldest_seen), Some(10))),
            )
            .await,
        )
        .await;
        let older_entries = older["entries"].as_array().unwrap();
        assert_eq!(older_entries.len(), 4);
        assert!(older_entries
            .iter()
            .all(|entry| entry["sequence"].as_i64().unwrap() < oldest_seen));

        // Following the tail from the newest cursor yields nothing new.
        let newest = first_entries[0]["sequence"].as_i64().unwrap();
        let tail = body_json(
            transmissions_handler(
                State(state),
                Query(transmission_params(Some(newest), None, Some(10))),
            )
            .await,
        )
        .await;
        assert!(tail["entries"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn transmissions_handler_applies_each_filter() {
        let directory = tempfile::tempdir().unwrap();
        let db = seeded_rings(&directory, 4).await;
        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);

        let mut params = transmission_params(None, None, Some(10));
        params.outcome = Some("failed".to_owned());
        let failed =
            body_json(transmissions_handler(State(state.clone()), Query(params)).await).await;
        assert_eq!(failed["entries"].as_array().unwrap().len(), 1);
        assert_eq!(failed["entries"][0]["error"], "timeout");

        let mut params = transmission_params(None, None, Some(10));
        params.target = Some("bt".to_owned());
        let by_target =
            body_json(transmissions_handler(State(state.clone()), Query(params)).await).await;
        assert_eq!(by_target["entries"].as_array().unwrap().len(), 2);

        let mut params = transmission_params(None, None, Some(10));
        params.serial = Some(11);
        let by_serial =
            body_json(transmissions_handler(State(state.clone()), Query(params)).await).await;
        assert_eq!(by_serial["entries"].as_array().unwrap().len(), 4);

        let mut params = transmission_params(None, None, Some(10));
        params.serial = Some(999);
        let unknown = body_json(transmissions_handler(State(state), Query(params)).await).await;
        assert!(unknown["entries"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn diagnostics_handlers_reject_a_malformed_parameter_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(seeded_rings(&directory, 1).await);
        let state = ApiState::for_tests(db, Tz::UTC);

        let mut params = transmission_params(None, None, Some(10));
        params.outcome = Some("maybe".to_owned());
        let response = transmissions_handler(State(state.clone()), Query(params)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["parameter"], "outcome");

        let response = transmissions_handler(
            State(state.clone()),
            Query(transmission_params(None, None, Some(0))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["parameter"], "limit");

        let response = transmissions_handler(
            State(state.clone()),
            Query(transmission_params(None, None, Some(1_001))),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let mut params = log_params(Some(10));
        params.level = Some("verbose".to_owned());
        let response = logs_handler(State(state), Query(params)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(response).await["parameter"], "level");
    }

    #[tokio::test]
    async fn logs_handler_returns_records_and_filters_by_level() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(seeded_rings(&directory, 1).await);
        let state = state_with_log_records(db);

        let body =
            body_json(logs_handler(State(state.clone()), Query(log_params(None))).await).await;
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["message"], "poll failed");
        assert_eq!(entries[0]["level"], "error");
        assert_eq!(entries[0]["fields"], "serial=11");
        assert_eq!(body["envelope"]["retained"], 2);

        let mut params = log_params(Some(10));
        params.level = Some("warn".to_owned());
        let warn = body_json(logs_handler(State(state), Query(params)).await).await;
        assert_eq!(warn["entries"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn disabled_rings_return_an_empty_page_rather_than_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let db = Arc::new(seeded_rings(&directory, 3).await);
        let mut state = ApiState::for_tests(db, Tz::UTC);
        state.transmission_ring = RingBounds::new(0, 50_000);
        state.log_buffer = LogBuffer::new(0, 50_000);

        let body = body_json(
            transmissions_handler(
                State(state.clone()),
                Query(transmission_params(None, None, None)),
            )
            .await,
        )
        .await;
        assert!(body["entries"].as_array().unwrap().is_empty());
        assert_eq!(body["envelope"]["retentionHours"], 0);
        assert_eq!(body["envelope"]["retained"], 0);

        let body = body_json(logs_handler(State(state), Query(log_params(None))).await).await;
        assert!(body["entries"].as_array().unwrap().is_empty());
        assert_eq!(body["envelope"]["retentionHours"], 0);
    }

    #[test]
    fn diagnostic_efficiency_filters_low_power_and_outliers() {
        assert_eq!(diagnostic_efficiency(950, 1_000), Some(95.0));
        assert_eq!(diagnostic_efficiency(100, 200), None);
        assert_eq!(diagnostic_efficiency(1_200, 1_000), None);
    }

    #[tokio::test]
    async fn diagnostics_handler_serializes_dynamic_mppts_for_all_tracker_shapes() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!("sqlite://{}", directory.path().join("api.db").display());
        let db = Db::connect(&database_url, Tz::UTC).await.unwrap();
        let serial = 42;
        let identity = InverterIdentity {
            serial_number: serial,
            susy_id: Some(125),
            configured_name: Some(CanonicalText::new("Roof").unwrap()),
            device_name: Some(CanonicalText::new("Device").unwrap()),
            model: Some(CanonicalText::new("Model").unwrap()),
            firmware_version: Some(CanonicalText::new("1.2.3").unwrap()),
            transport: None,
        };
        let date = Utc::now().date_naive();
        let start = date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
        let tracker_sets = [
            vec![],
            vec![(7, 700, 1_700, 370_000)],
            vec![
                (3, 300, 1_300, 330_000),
                (1, 100, 1_100, 310_000),
                (2, 200, 1_200, 320_000),
            ],
            vec![(255, 2_550, 3_550, 555_000)],
            vec![
                (255, 2_555, 3_555, 555_000),
                (9, 900, 1_900, 390_000),
                (2, 200, 1_200, 320_000),
            ],
        ];

        for (index, trackers) in tracker_sets.iter().enumerate() {
            let measurement = InverterMeasurement {
                measured_at: UnixSeconds::new(start + index as i64 + 1),
                ac_power: [Some(Watts::new(900)), None, None],
                ac_current: [None; 3],
                ac_voltage: [None; 3],
                grid_frequency: None,
                grid_import_power: None,
                grid_export_power: None,
                energy_today: None,
                energy_total: None,
                operating_time: None,
                feed_in_time: None,
                device_status: None,
                grid_relay_status: None,
                temperature: None,
                bluetooth_signal: None,
                mppts: trackers
                    .iter()
                    .map(
                        |&(tracker_number, dc_power_w, dc_current_ma, dc_voltage_mv)| {
                            MpptMeasurement {
                                tracker_number,
                                dc_power: Some(Watts::new(dc_power_w)),
                                dc_current: Some(Milliamperes::new(dc_current_ma)),
                                dc_voltage: Some(MilliVolts::new(dc_voltage_mv)),
                            }
                        },
                    )
                    .collect(),
                battery: None,
            };
            db.write_poll(&identity, &measurement).await.unwrap();
        }

        assert_eq!(
            db.spot_strings(serial, start, start + 10).await.unwrap(),
            vec![
                (start + 1, vec![]),
                (start + 2, vec![(7, 700)]),
                (start + 3, vec![(1, 100), (2, 200), (3, 300)]),
                (start + 4, vec![(255, 2_550)]),
                (start + 5, vec![(2, 200), (9, 900), (255, 2_555)]),
            ]
        );

        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);
        let response = diagnostics_handler(
            State(state),
            Query(DiagnosticsParams {
                date: Some(date.to_string()),
                serial: Some(serial),
            }),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        let inverter = &body["inverters"][0];
        let rows = inverter["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0]["mppts"], json!([]));
        assert_eq!(
            rows[1]["mppts"],
            json!([{
                "tracker_number": 7,
                "dc_power_w": 700,
                "dc_current_ma": 1_700,
                "dc_voltage_mv": 370_000,
            }])
        );
        assert_eq!(
            rows[2]["mppts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|mppt| mppt["tracker_number"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(rows[3]["mppts"][0]["tracker_number"], 255);
        assert_eq!(
            rows[4]["mppts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|mppt| mppt["tracker_number"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![2, 9, 255]
        );
        assert_eq!(inverter["latestMeasurement"]["mppts"], rows[4]["mppts"]);

        for measurement in rows
            .iter()
            .chain(std::iter::once(&inverter["latestMeasurement"]))
        {
            for removed in ["pdc1", "pdc2", "idc1", "idc2", "udc1", "udc2"] {
                assert!(
                    measurement.get(removed).is_none(),
                    "{removed} must not remain in canonical diagnostics JSON"
                );
            }
            for stable in [
                "timestamp",
                "label",
                "acPower",
                "acVoltage",
                "acCurrent",
                "frequency",
                "efficiency",
                "signal",
                "status",
            ] {
                assert!(
                    measurement.get(stable).is_some(),
                    "non-MPPT response field {stable} changed"
                );
            }
        }
    }

    async fn history_response(
        state: ApiState,
        range: &str,
        week: Option<&str>,
        month: Option<&str>,
        year: Option<&str>,
    ) -> Value {
        let response = history_handler(
            State(state),
            Query(HistoryParams {
                range: Some(range.to_string()),
                date: None,
                week: week.map(str::to_string),
                month: month.map(str::to_string),
                year: year.map(str::to_string),
                serial: Some(42),
                strings: false,
            }),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn day_history_response(state: ApiState, selected_date: NaiveDate) -> Value {
        let response = history_handler(
            State(state),
            Query(HistoryParams {
                range: Some("day".to_string()),
                date: Some(selected_date.to_string()),
                week: None,
                month: None,
                year: None,
                serial: Some(42),
                strings: false,
            }),
        )
        .await;
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn seed_statistics_inverter(pool: &sqlx::SqlitePool, date: NaiveDate) -> (i64, i64, i64) {
        let inverter_id: i64 = sqlx::query_scalar(
            "INSERT INTO inverters
             (serial_number,configured_name,first_seen_at,last_seen_at)
             VALUES (42,'Roof',0,0)
             RETURNING inverter_id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        let (start, end) = local_day_utc_bounds(Tz::UTC, date).unwrap();
        for (offset, ac_power, dc_power) in [(0, 100, 80), (300, 300, 240)] {
            let measurement_id: i64 = sqlx::query_scalar(
                "INSERT INTO inverter_measurements
                 (inverter_id,measured_at,ac_power_l1_w)
                 VALUES ($1,$2,$3)
                 RETURNING measurement_id",
            )
            .bind(inverter_id)
            .bind(start + offset)
            .bind(ac_power)
            .fetch_one(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO mppt_measurements
                 (measurement_id,tracker_number,dc_power_w)
                 VALUES ($1,1,$2)",
            )
            .bind(measurement_id)
            .bind(dc_power)
            .execute(pool)
            .await
            .unwrap();
        }
        (inverter_id, start, end)
    }

    #[tokio::test]
    async fn day_history_handler_omits_statistics_when_optional_cache_is_disabled() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!(
            "sqlite://{}",
            directory.path().join("no-stats.db").display()
        );
        let db = Db::connect(&database_url, Tz::UTC).await.unwrap();
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool.clone(),
            Db::Postgres { .. } => unreachable!(),
        };
        sqlx::query("INSERT INTO inverters (serial_number) VALUES (42)")
            .execute(&pool)
            .await
            .unwrap();
        let table_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_schema
               WHERE type='table' AND name='inverter_daily_statistics'
             )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!table_exists);
        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);

        let response = day_history_response(state, date(2024, 6, 1)).await;
        assert!(response.get("dailyStatistics").is_none());
        let table_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(
               SELECT 1 FROM sqlite_schema
               WHERE type='table' AND name='inverter_daily_statistics'
             )",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(!table_exists, "the read path must not enable the cache");
    }

    #[tokio::test]
    async fn day_history_handler_returns_enabled_daily_statistics() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!("sqlite://{}", directory.path().join("stats.db").display());
        let db = Db::connect_with_daily_statistics(&database_url, Tz::UTC, 300)
            .await
            .unwrap();
        let selected_date = date(2024, 6, 2);
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!(),
        };
        let (_inverter_id, start, end) = seed_statistics_inverter(pool, selected_date).await;
        db.rebuild_daily_statistics(
            42,
            selected_date,
            selected_date.succ_opt().unwrap(),
            end + 1,
        )
        .await
        .unwrap();
        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);

        let response = day_history_response(state, selected_date).await;
        assert_eq!(
            response["dailyStatistics"],
            json!([{
                "serial": 42,
                "date": "2024-06-02",
                "peak": {
                    "acPower": {"value": 300, "unit": "W"},
                    "dcPower": {"value": 240, "unit": "W"},
                },
                "mean": {
                    "acPower": {"value": 100, "unit": "W"},
                    "dcPower": {"value": 80, "unit": "W"},
                },
                "measurements": {
                    "actualCount": 2,
                    "expectedCount": 288,
                },
                "coverage": {
                    "ratio": 2.0 / 288.0,
                    "firstMeasuredAt": start,
                    "lastMeasuredAt": start + 300,
                },
                "complete": false,
                "sourceMaxMeasuredAt": start + 300,
                "calculatedAt": end + 1,
                "stale": false,
            }])
        );
    }

    #[tokio::test]
    async fn day_history_handler_marks_cached_statistics_stale_after_late_measurement() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!(
            "sqlite://{}",
            directory.path().join("stale-stats.db").display()
        );
        let db = Db::connect_with_daily_statistics(&database_url, Tz::UTC, 300)
            .await
            .unwrap();
        let selected_date = date(2024, 6, 3);
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!(),
        };
        let (inverter_id, start, end) = seed_statistics_inverter(pool, selected_date).await;
        db.rebuild_daily_statistics(
            42,
            selected_date,
            selected_date.succ_opt().unwrap(),
            end + 1,
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO inverter_measurements
             (inverter_id,measured_at,ac_power_l1_w)
             VALUES ($1,$2,200)",
        )
        .bind(inverter_id)
        .bind(start + 600)
        .execute(pool)
        .await
        .unwrap();
        let state = ApiState::for_tests(Arc::new(db), Tz::UTC);

        let response = day_history_response(state, selected_date).await;
        assert_eq!(
            response["dailyStatistics"][0]["measurements"]["actualCount"],
            2
        );
        assert_eq!(
            response["dailyStatistics"][0]["sourceMaxMeasuredAt"],
            start + 300
        );
        assert_eq!(response["dailyStatistics"][0]["stale"], true);
    }

    #[tokio::test]
    async fn historical_handlers_use_only_daily_rollups_across_missing_and_incomplete_days() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!("sqlite://{}", directory.path().join("history.db").display());
        let db = Db::connect(&database_url, chrono_tz::Europe::Berlin)
            .await
            .unwrap();
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!(),
        };
        let inverter_id: i64 = sqlx::query_scalar(
            "INSERT INTO inverters
             (serial_number,configured_name,first_seen_at,last_seen_at)
             VALUES (42,'Roof',0,0)
             RETURNING inverter_id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        for (date, energy, complete) in [
            ("2024-12-30", Some(1_000_i64), 1_i64),
            ("2024-12-31", Some(2_000), 1),
            ("2025-01-01", Some(3_000), 1),
            // An explicit NULL remains missing instead of becoming a zero day.
            ("2025-01-02", None, 1),
            // Incomplete current/rebuilt data is returned with its known yield.
            ("2025-01-03", Some(4_000), 0),
            ("2025-02-01", Some(5_000), 1),
        ] {
            sqlx::query(
                "INSERT INTO inverter_daily_yields
                 (inverter_id,yield_date,total_energy_wh,daily_energy_wh,is_complete,updated_at)
                 VALUES ($1,$2,NULL,$3,$4,0)",
            )
            .bind(inverter_id)
            .bind(date)
            .bind(energy)
            .bind(complete)
            .execute(pool)
            .await
            .unwrap();
        }

        // Historical summaries must remain operational without the raw archive.
        sqlx::query("DROP TABLE inverter_energy_samples")
            .execute(pool)
            .await
            .unwrap();
        let state = ApiState::for_tests(Arc::new(db), chrono_tz::Europe::Berlin);

        let week = history_response(state.clone(), "week", Some("2025-01-01"), None, None).await;
        assert_eq!(
            week["rows"],
            json!([
                {"label": "2024-12-30", "Roof": 1.0},
                {"label": "2024-12-31", "Roof": 2.0},
                {"label": "2025-01-01", "Roof": 3.0},
                {"label": "2025-01-03", "Roof": 4.0},
            ])
        );
        assert_eq!(week["summary"]["recordedDays"], 4);

        let month = history_response(state.clone(), "month", None, Some("2025-01"), None).await;
        assert_eq!(
            month["rows"],
            json!([
                {"label": "2025-01-01", "Roof": 3.0},
                {"label": "2025-01-03", "Roof": 4.0},
            ])
        );
        assert_eq!(month["summary"]["total"], 7.0);
        assert_eq!(month["summary"]["previousMonthTotal"], 3.0);

        let year = history_response(state, "year", None, None, Some("2025")).await;
        assert_eq!(year["rows"][0], json!({"label": "2025-01", "Roof": 7.0}));
        assert_eq!(year["rows"][1], json!({"label": "2025-02", "Roof": 5.0}));
        assert_eq!(year["summary"]["total"], 12.0);
        assert_eq!(year["summary"]["previousYearTotal"], 3.0);
    }

    #[tokio::test]
    async fn historical_handler_respects_local_dates_across_a_dst_boundary() {
        let directory = tempfile::tempdir().unwrap();
        let database_url = format!(
            "sqlite://{}",
            directory.path().join("dst-history.db").display()
        );
        let db = Db::connect(&database_url, chrono_tz::Europe::Berlin)
            .await
            .unwrap();
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!(),
        };
        let inverter_id: i64 = sqlx::query_scalar(
            "INSERT INTO inverters
             (serial_number,configured_name,first_seen_at,last_seen_at)
             VALUES (42,'Roof',0,0)
             RETURNING inverter_id",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        for date in ["2024-03-30", "2024-03-31", "2024-04-01"] {
            sqlx::query(
                "INSERT INTO inverter_daily_yields
                 (inverter_id,yield_date,daily_energy_wh,is_complete,updated_at)
                 VALUES ($1,$2,1000,1,0)",
            )
            .bind(inverter_id)
            .bind(date)
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query("DROP TABLE inverter_energy_samples")
            .execute(pool)
            .await
            .unwrap();
        let state = ApiState::for_tests(Arc::new(db), chrono_tz::Europe::Berlin);

        let week = history_response(state, "week", Some("2024-03-27"), None, None).await;
        assert_eq!(
            week["rows"],
            json!([
                {"label": "2024-03-30", "Roof": 1.0},
                {"label": "2024-03-31", "Roof": 1.0},
            ])
        );
    }

    #[test]
    fn history_month_requires_iso_calendar_month() {
        assert_eq!(
            parse_history_month(Some("2026-07"))
                .expect("valid month")
                .expect("month present")
                .to_string(),
            "2026-07-01"
        );
        assert!(parse_history_month(Some("07.2026")).is_err());
        assert!(parse_history_month(Some("2026-13")).is_err());
        assert_eq!(parse_history_month(None).expect("optional month"), None);
    }

    #[test]
    fn history_week_requires_iso_date_and_starts_on_monday() {
        let selected = parse_history_week(Some("2026-07-22"))
            .expect("valid week")
            .expect("week present");
        assert_eq!(week_start(selected).to_string(), "2026-07-20");
        assert!(parse_history_week(Some("22.07.2026")).is_err());
        assert!(parse_history_week(Some("2026-02-30")).is_err());
        assert_eq!(parse_history_week(None).expect("optional week"), None);
    }

    #[test]
    fn running_period_comparisons_use_the_same_calendar_progress() {
        assert_eq!(
            previous_week_comparison_end(date(2026, 7, 20), date(2026, 7, 20), date(2026, 7, 22),),
            date(2026, 7, 16)
        );
        assert_eq!(
            previous_month_comparison_end(date(2026, 7, 1), date(2026, 7, 1), date(2026, 7, 26),),
            date(2026, 6, 27)
        );
        assert_eq!(
            previous_year_comparison_end(2026, 2026, date(2026, 7, 26)),
            date(2025, 7, 27)
        );
        assert_eq!(
            previous_year_comparison_end(2024, 2024, date(2024, 2, 29)),
            date(2023, 3, 1)
        );
    }

    #[test]
    fn completed_period_comparisons_use_the_full_previous_period() {
        assert_eq!(
            previous_week_comparison_end(date(2026, 7, 13), date(2026, 7, 20), date(2026, 7, 22),),
            date(2026, 7, 13)
        );
        assert_eq!(
            previous_month_comparison_end(date(2026, 6, 1), date(2026, 7, 1), date(2026, 7, 26),),
            date(2026, 6, 1)
        );
        assert_eq!(
            previous_year_comparison_end(2025, 2026, date(2026, 7, 26)),
            date(2025, 1, 1)
        );
    }

    #[test]
    fn history_year_requires_four_digits() {
        assert_eq!(
            parse_history_year(Some("2026")).expect("valid year"),
            Some(2026)
        );
        assert!(parse_history_year(Some("26")).is_err());
        assert!(parse_history_year(Some("year")).is_err());
        assert_eq!(parse_history_year(None).expect("optional year"), None);
    }

    #[test]
    fn day_metrics_keep_cumulative_energy_when_one_inverter_stops() {
        let rows = build_day_metric_rows(
            vec![
                (0, 1, 1_000, 10_000, Some(20.0)),
                (5, 2, 2_000, 20_000, Some(22.0)),
                (300, 1, 1_500, 11_000, Some(21.0)),
            ],
            vec![(0, 1, 900), (0, 2, 1_800), (300, 1, 1_400)],
            Tz::UTC,
        );

        assert_eq!(rows[0]["power"], 2.7);
        assert_eq!(rows[0]["energy"], 30.0);
        assert_eq!(rows[0]["temperature"], 21.0);
        assert_eq!(rows[1]["power"], 1.4);
        assert_eq!(rows[1]["energy"], 31.0);
        assert_eq!(rows[1]["temperature"], 21.5);
    }

    #[test]
    fn inverter_day_rows_keep_each_inverters_series_separate() {
        let mut merged: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
        merge_inverter_day_rows(
            &mut merged,
            vec![json!({ "label": "10:30", "power": 1.25, "energy": 4.5 })],
            "inverter_1_power",
            "inverter_1_energy",
        );
        merge_inverter_day_rows(
            &mut merged,
            vec![json!({ "label": "10:30", "power": 2.75, "energy": 8.0 })],
            "inverter_2_power",
            "inverter_2_energy",
        );

        let row = merged.get("10:30").expect("merged time slot");
        assert_eq!(row["inverter_1_power"], 1.25);
        assert_eq!(row["inverter_1_energy"], 4.5);
        assert_eq!(row["inverter_2_power"], 2.75);
        assert_eq!(row["inverter_2_energy"], 8.0);
    }

    #[test]
    fn day_summary_reports_energy_power_and_previous_day_change() {
        let summary = build_day_summary(
            &[
                json!({ "total_power": 2.0, "total_energy": 10.0 }),
                json!({ "total_power": 4.0, "total_energy": 15.0 }),
            ],
            "total_power",
            "total_energy",
            12.0,
        );

        assert_eq!(summary["totalEnergy"], 15.0);
        assert_eq!(summary["peakPower"], 4.0);
        assert_eq!(summary["averagePower"], 3.0);
        assert_eq!(summary["recordedIntervals"], 2);
        assert_eq!(summary["previousDayEnergy"], 12.0);
        assert_eq!(summary["changePercent"], 25.0);
    }

    #[test]
    fn month_summary_compares_aggregated_daily_values() {
        let buckets = BTreeMap::from([
            ("2026-07-01".to_string(), 10.0),
            ("2026-07-02".to_string(), 20.0),
        ]);

        let summary = build_month_summary(&buckets, 40.0);

        assert_eq!(summary["total"], 30.0);
        assert_eq!(summary["averageDaily"], 15.0);
        assert_eq!(summary["recordedDays"], 2);
        assert_eq!(summary["bestDay"]["label"], "2026-07-02");
        assert_eq!(summary["bestDay"]["value"], 20.0);
        assert_eq!(summary["previousMonthTotal"], 40.0);
        assert_eq!(summary["changePercent"], -25.0);
    }

    #[test]
    fn week_summary_compares_aggregated_daily_values() {
        let summary = build_week_summary(
            &BTreeMap::from([
                ("2026-07-13".to_string(), 40.0),
                ("2026-07-14".to_string(), 60.0),
            ]),
            80.0,
        );

        assert_eq!(summary["total"], 100.0);
        assert_eq!(summary["averageDaily"], 50.0);
        assert_eq!(summary["recordedDays"], 2);
        assert_eq!(
            summary["bestDay"],
            json!({ "label": "2026-07-14", "value": 60.0 })
        );
        assert_eq!(summary["previousWeekTotal"], 80.0);
        assert_eq!(summary["changePercent"], 25.0);
    }

    #[test]
    fn year_summary_aggregates_months_and_compares_previous_year() {
        let mut buckets = BTreeMap::from([
            ("2026-01".to_string(), 100.0),
            ("2026-02".to_string(), 140.0),
        ]);
        complete_year_buckets(2026, &mut buckets);
        let summary = build_year_summary(&buckets, 200.0);

        assert_eq!(buckets.len(), 12);
        assert_eq!(buckets["2026-12"], 0.0);
        assert_eq!(summary["total"], 240.0);
        assert_eq!(summary["averageMonthly"], 120.0);
        assert_eq!(summary["recordedMonths"], 2);
        assert_eq!(
            summary["bestMonth"],
            json!({ "label": "2026-02", "value": 140.0 })
        );
        assert_eq!(summary["previousYearTotal"], 200.0);
        assert_eq!(summary["changePercent"], 20.0);
    }

    #[test]
    fn configured_names_override_inverter_reported_names() {
        let config = Config::parse(
            r#"
[plant]
latitude = 0.0
longitude = 0.0
[database]
url = "sqlite::memory:"
[[inverter]]
name = "Roof"
communication = "ethernet"
address = "192.168.1.50"
password = "x"
[[inverter]]
name = "Garage"
communication = "bluetooth"
address = "00:80:25:AA:BB:CC"
password = "x"
"#,
        )
        .expect("valid mixed config");
        let make_inverter =
            |serial, endpoint: &str, name: &str, transport| InverterPollObservation {
                identity: InverterIdentity {
                    serial_number: serial,
                    susy_id: Some(1),
                    configured_name: None,
                    device_name: Some(CanonicalText::new(name).unwrap()),
                    model: None,
                    firmware_version: None,
                    transport: Some(transport),
                },
                communication: CommunicationIdentity {
                    protocol: ProtocolFamily::SmaData2Plus,
                    transport,
                    endpoint: Some(CanonicalText::new(endpoint).unwrap()),
                },
                live: LiveOutcome::Observed(Box::new(LiveObservation {
                    inverter_time: None,
                    wakeup_time: None,
                    sleep_time: None,
                    measurement: InverterMeasurement {
                        measured_at: UnixSeconds::new(0),
                        ac_power: [None; 3],
                        ac_current: [None; 3],
                        ac_voltage: [None; 3],
                        grid_frequency: None,
                        grid_import_power: None,
                        grid_export_power: None,
                        energy_today: None,
                        energy_total: None,
                        operating_time: None,
                        feed_in_time: None,
                        device_status: None,
                        grid_relay_status: None,
                        temperature: None,
                        bluetooth_signal: None,
                        mppts: Vec::new(),
                        battery: None,
                    },
                    reported_ac_power: None,
                    reported_dc_power: None,
                    device_class: 8001,
                    battery_diagnostics: None,
                })),
                day_archive: ArchiveOutcome::NotRequested,
                month_yield_archive: ArchiveOutcome::NotRequested,
                event_archive: ArchiveOutcome::NotRequested,
            };
        let ethernet = make_inverter(
            0,
            "192.168.1.50",
            "reported Ethernet name",
            Transport::Ethernet,
        );
        let bluetooth = make_inverter(
            123,
            "00:80:25:AA:BB:CC",
            "reported Bluetooth name",
            Transport::Bluetooth,
        );
        let mut inverters = [ethernet, bluetooth];

        Service::apply_configured_names(&config, &mut inverters);

        assert_eq!(
            inverters[0].identity.device_name.as_ref().unwrap().as_str(),
            "reported Ethernet name"
        );
        assert_eq!(
            inverters[1].identity.device_name.as_ref().unwrap().as_str(),
            "reported Bluetooth name"
        );
        assert_eq!(
            inverters[0]
                .identity
                .configured_name
                .as_ref()
                .map(CanonicalText::as_str),
            Some("Roof")
        );
        assert_eq!(
            inverters[1]
                .identity
                .configured_name
                .as_ref()
                .map(CanonicalText::as_str),
            Some("Garage")
        );
        assert_eq!(
            inverter_target(&config.inverters[0]),
            "Roof (Ethernet 192.168.1.50)"
        );
        assert_eq!(
            inverter_target(&config.inverters[1]),
            "Garage (Bluetooth 00:80:25:AA:BB:CC)"
        );
    }
}
