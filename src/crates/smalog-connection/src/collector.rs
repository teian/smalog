//! One poll loop for any [`Connection`].
//!
//! The collector owns the per-inverter [`InverterData`] state and runs the
//! SBFspot query sequence + archive fetches through the connector's
//! request primitive, decoding with the shared protocol layer. It is the
//! single, transport-agnostic replacement for SBFspot's per-transport
//! `Inverter::process()` loops.

use std::collections::HashMap;

use chrono::{Datelike, Utc};
use chrono_tz::Tz;
use tracing::{debug, info, warn};

use crate::connection::{ClockMode, Connection, SyncOutcome, UserGroup};
use crate::error::Result;
use crate::smadata2::archive::{
    day_request_window, event_command, event_request_window, month_request_window,
    process_day_frames, process_event_frames, process_month_frames,
};
use crate::smadata2::commands::{QueryKind, CMD_ARCHIVE_DAY, CMD_ARCHIVE_MONTH};
use crate::smadata2::decode::decode_spot_records;
use crate::smadata2::inverter::InverterData;
use crate::speedwire::packet::Datagram;
use smalog_observation::PollCycleObservation;

/// Poll behaviour toggles (SBFspot CalcMissingSpot / consumption).
#[derive(Debug, Clone, Copy, Default)]
pub struct PollOptions {
    /// Derive missing Pdc/Pac from voltage × current (SBFspot CalcMissingSpot).
    pub calc_missing_spot: bool,
    /// Poll the consumer-power LRIs into the Consumption table.
    pub poll_consumption: bool,
}

/// Drives any [`Connection`] through the SBFspot poll sequence, owning the
/// per-inverter [`InverterData`] state.
pub struct Collector {
    connector: Box<dyn Connection>,
    tz: Tz,
    opts: PollOptions,
    inverters: Vec<InverterData>,
    /// serial → month-data wobble offset, probed once (issues 115/130).
    month_offsets: HashMap<u32, i64>,
    offsets_probed: bool,
}

impl Collector {
    /// Build a collector over `connector`, using `tz` for archive windows.
    pub fn new(connector: Box<dyn Connection>, tz: Tz, opts: PollOptions) -> Collector {
        Collector {
            connector,
            tz,
            opts,
            inverters: Vec::new(),
            month_offsets: HashMap::new(),
            offsets_probed: false,
        }
    }

    /// Last polled snapshot (also returned by [`Self::cycle`]).
    pub fn inverters(&self) -> Vec<InverterData> {
        self.inverters.clone()
    }

    /// One full cycle: begin session → login → query sequence →
    /// (optional) archives → logoff. Returns a snapshot of all inverters.
    pub async fn cycle(
        &mut self,
        fetch_day: bool,
        daily: Option<(u32, u32)>,
    ) -> Result<Vec<InverterData>> {
        self.connector.begin().await?;
        self.rebuild_inverters();

        // Once begin() succeeds, always tear the session down. In particular,
        // a failed login must not leave Bluetooth or a partially logged-in
        // multi-inverter connection behind for the next poll cycle.
        let res = match self.connector.login_all().await {
            Ok(()) => self.run(fetch_day, daily).await,
            Err(error) => Err(error),
        };
        self.connector.end().await;

        res?;
        Ok(self.inverters.clone())
    }

    /// One complete Poll Cycle converted at the connection seam into
    /// protocol-neutral canonical observations.
    pub async fn cycle_observations(
        &mut self,
        fetch_day: bool,
        daily: Option<(u32, u32)>,
    ) -> Result<PollCycleObservation> {
        let observed_at = Utc::now().timestamp();
        self.cycle(fetch_day, daily).await?;
        let (protocol, transport) = self.connector.communication();
        Ok(crate::observation::poll_cycle(
            &self.inverters,
            observed_at,
            protocol,
            transport,
            fetch_day,
            daily,
        )?)
    }

    /// Last successfully decoded state as protocol-neutral observations.
    pub fn observations(&self) -> Result<PollCycleObservation> {
        let observed_at = Utc::now().timestamp();
        let (protocol, transport) = self.connector.communication();
        Ok(crate::observation::poll_cycle(
            &self.inverters,
            observed_at,
            protocol,
            transport,
            false,
            None,
        )?)
    }

    /// Non-mutating connection probe: begin session, log in, fetch a
    /// representative live/energy value and log off. Unlike [`Self::cycle`],
    /// this does not run clock synchronisation or archive requests.
    pub async fn probe(&mut self) -> Result<(Vec<InverterData>, usize)> {
        self.probe_inner(false).await
    }

    /// Non-mutating full spot-data probe: begin session, log in, run the
    /// complete spot query sequence and log off. This does not synchronize
    /// the clock or fetch day/month archives and events.
    pub async fn probe_all(&mut self) -> Result<(Vec<InverterData>, usize)> {
        self.probe_inner(true).await
    }

    async fn probe_inner(&mut self, all: bool) -> Result<(Vec<InverterData>, usize)> {
        self.connector.begin().await?;
        self.rebuild_inverters();

        let result = match self.connector.login_all().await {
            Ok(()) if all => Ok(self.query_sequence(false).await),
            Ok(()) => Ok(self.probe_sequence().await),
            Err(error) => Err(error),
        };
        self.connector.end().await;

        let received_frames = result?;
        Ok((self.inverters.clone(), received_frames))
    }

    /// Try representative read-only queries and stop at the first response.
    /// A connectivity test should not spend minutes retrying every optional
    /// LRI when the link accepts login but does not answer data requests.
    async fn probe_sequence(&mut self) -> usize {
        for kind in [
            QueryKind::EnergyProduction,
            QueryKind::SpotAcTotalPower,
            QueryKind::TypeLabel,
        ] {
            let received_frames = self.query(kind).await;
            if received_frames > 0 {
                return received_frames;
            }
        }
        0
    }

    /// Fresh per-cycle inverter state from the connector's device list,
    /// carrying the probed month offset per serial.
    fn rebuild_inverters(&mut self) {
        self.inverters = self
            .connector
            .devices()
            .into_iter()
            .map(|d| {
                let mut inv = InverterData::new(d.address);
                inv.susy_id = d.susy_id;
                inv.serial = d.serial;
                inv.month_data_offset = self.month_offsets.get(&d.serial).copied().unwrap_or(0);
                inv
            })
            .collect();
    }

    async fn run(&mut self, fetch_day: bool, daily: Option<(u32, u32)>) -> Result<()> {
        self.clock_sync().await;
        self.query_sequence(true).await;
        if fetch_day {
            self.fetch_day().await;
        }
        if let Some((months, event_months)) = daily {
            self.fetch_month(months).await;
            self.fetch_events(event_months).await;
        }
        Ok(())
    }

    async fn clock_sync(&mut self) {
        match self.connector.set_clock(ClockMode::Auto).await {
            Ok(SyncOutcome::Set) => info!("inverter clock synchronised"),
            Ok(SyncOutcome::Skipped(r)) => debug!(reason = r, "clock sync skipped"),
            Ok(SyncOutcome::VerifyFailed { drift }) => warn!(drift, "clock sync not confirmed"),
            Ok(SyncOutcome::Unsupported) => {}
            Err(e) => warn!(error = %e, "clock sync failed"),
        }
    }

    fn index_of(&self, serial: u32) -> Option<usize> {
        self.inverters.iter().position(|i| i.serial == serial)
    }

    /// Run one spot query against all devices and decode replies into the
    /// matching inverter's state.
    async fn query(&mut self, kind: QueryKind) -> usize {
        let q = kind.query();
        let map = match self
            .connector
            .request_all(q.command, q.first, q.last, false)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                warn!(?kind, error = %e, "query failed");
                return 0;
            }
        };
        let received_frames = map.values().map(Vec::len).sum();
        for (serial, frames) in map {
            let Some(i) = self.index_of(serial) else {
                continue;
            };
            for f in &frames {
                if let Some(d) = Datagram::parse(f) {
                    decode_spot_records(&d, &mut self.inverters[i]);
                }
            }
        }
        received_frames
    }

    /// The SBFspot ~19-step query sequence (`Inverter::process()` order).
    async fn query_sequence(&mut self, derive_etoday: bool) -> usize {
        let mut received_frames = 0;
        received_frames += self.query(QueryKind::SoftwareVersion).await;
        received_frames += self.query(QueryKind::TypeLabel).await;
        for inv in &mut self.inverters {
            inv.has_battery = inv.dev_class == 8007 || inv.dev_class == 8009 || inv.susy_id == 292;
        }
        if self.inverters.iter().any(|i| i.has_battery) {
            received_frames += self.query(QueryKind::BatteryChargeStatus).await;
            received_frames += self.query(QueryKind::BatteryInfo).await;
        }
        received_frames += self.query(QueryKind::MeteringGridMsTotW).await;
        if self.opts.poll_consumption {
            received_frames += self.query(QueryKind::ConsumptionEnergy).await;
            received_frames += self.query(QueryKind::ConsumptionPower).await;
        }
        received_frames += self.query(QueryKind::DeviceStatus).await;
        received_frames += self.query(QueryKind::InverterTemperature).await;
        if self.inverters.first().map(|i| i.dev_class) == Some(8001) {
            received_frames += self.query(QueryKind::GridRelayStatus).await;
        }
        received_frames += self.query(QueryKind::EnergyProduction).await;
        if derive_etoday {
            self.etoday_fallback().await;
        }
        received_frames += self.query(QueryKind::OperationTime).await;
        received_frames += self.query(QueryKind::SpotDcPower).await;
        received_frames += self.query(QueryKind::SpotDcVoltage).await;
        received_frames += self.query(QueryKind::SpotAcPower).await;
        received_frames += self.query(QueryKind::SpotAcVoltage).await;
        received_frames += self.query(QueryKind::SpotAcTotalPower).await;
        let calc = self.opts.calc_missing_spot;
        for inv in &mut self.inverters {
            if calc {
                calc_missing_spot(inv);
            }
            inv.calc_derived();
        }
        received_frames += self.query(QueryKind::SpotGridFrequency).await;
        received_frames
    }

    /// Fix #290/#459: inverters reporting EToday = 0 get it derived from
    /// ETotal − the day archive's midnight total.
    async fn etoday_fallback(&mut self) {
        let need: Vec<u32> = self
            .inverters
            .iter()
            .filter(|i| i.e_today == 0 && i.e_total != 0)
            .map(|i| i.serial)
            .collect();
        if need.is_empty() {
            return;
        }
        let now = Utc::now().timestamp();
        let (target_day, first, last) = day_request_window(now, self.tz);
        let Ok(map) = self
            .connector
            .request_all(CMD_ARCHIVE_DAY, first, last, false)
            .await
        else {
            return;
        };
        for (serial, frames) in map {
            if !need.contains(&serial) {
                continue;
            }
            let (day, has) = process_day_frames(&frames, target_day, self.tz);
            if !has {
                continue;
            }
            if let Some(i) = self.index_of(serial) {
                if let Some(first_slot) = day.iter().find(|d| d.datetime != 0) {
                    if first_slot.total_wh != 0 {
                        self.inverters[i].e_today = self.inverters[i].e_total - first_slot.total_wh;
                        debug!(serial, "EToday derived from day archive");
                    }
                }
            }
        }
    }

    async fn fetch_day(&mut self) {
        if !self.offsets_probed {
            self.probe_month_offsets().await;
            self.offsets_probed = true;
        }
        let now = Utc::now().timestamp();
        let (target_day, first, last) = day_request_window(now, self.tz);
        let map = match self
            .connector
            .request_all(CMD_ARCHIVE_DAY, first, last, false)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "day archive failed");
                return;
            }
        };
        for (serial, frames) in map {
            let (day, has) = process_day_frames(&frames, target_day, self.tz);
            if let Some(i) = self.index_of(serial) {
                self.inverters[i].day_data = day;
                self.inverters[i].has_day_data = has;
            }
        }
    }

    async fn probe_month_offsets(&mut self) {
        let now = Utc::now();
        let (first, last) = month_request_window(now.year(), now.month(), self.tz);
        let Ok(map) = self
            .connector
            .request_all(CMD_ARCHIVE_MONTH, first, last, false)
            .await
        else {
            return;
        };
        for (serial, frames) in map {
            let (md, has) = process_month_frames(&frames, now.month(), 0);
            let mut offset = 0i64;
            if has {
                for rec in md.iter().skip(1).rev() {
                    if rec.datetime != 0 {
                        let rec_day = chrono::DateTime::from_timestamp(rec.datetime, 0)
                            .unwrap()
                            .ordinal();
                        if rec_day == now.ordinal() {
                            offset = -86_400;
                        }
                        break;
                    }
                }
            }
            self.month_offsets.insert(serial, offset);
            if let Some(i) = self.index_of(serial) {
                self.inverters[i].month_data_offset = offset;
            }
        }
    }

    async fn fetch_month(&mut self, months: u32) {
        let now = Utc::now();
        let (mut y, mut m) = (now.year(), now.month());
        for _ in 0..months {
            let (first, last) = month_request_window(y, m, self.tz);
            if let Ok(map) = self
                .connector
                .request_all(CMD_ARCHIVE_MONTH, first, last, false)
                .await
            {
                for (serial, frames) in map {
                    let off = self.month_offsets.get(&serial).copied().unwrap_or(0);
                    let (md, has) = process_month_frames(&frames, m, off);
                    if let Some(i) = self.index_of(serial) {
                        self.inverters[i].month_data = md;
                        self.inverters[i].has_month_data = has;
                    }
                }
            }
            (y, m) = prev_month(y, m);
        }
    }

    async fn fetch_events(&mut self, event_months: u32) {
        for inv in &mut self.inverters {
            inv.event_data.clear();
        }
        let groups: &[UserGroup] = match self.connector.user_group() {
            UserGroup::Installer => &[UserGroup::User, UserGroup::Installer],
            UserGroup::User => &[UserGroup::User],
        };
        let now = Utc::now();
        let (mut y, mut m) = (now.year(), now.month());
        let mut eof: HashMap<u32, bool> = HashMap::new();
        for _ in 0..event_months {
            let (first, last) = event_request_window(y, m);
            for &group in groups {
                if let Ok(map) = self
                    .connector
                    .request_all(event_command(group), first, last, true)
                    .await
                {
                    for (serial, frames) in map {
                        let (events, e) = process_event_frames(&frames, group.code());
                        if let Some(i) = self.index_of(serial) {
                            self.inverters[i].event_data.extend(events);
                        }
                        *eof.entry(serial).or_insert(false) |= e;
                    }
                }
            }
            if !self.inverters.is_empty()
                && self
                    .inverters
                    .iter()
                    .all(|i| eof.get(&i.serial).copied().unwrap_or(false))
            {
                break;
            }
            (y, m) = prev_month(y, m);
        }
    }
}

fn prev_month(y: i32, m: u32) -> (i32, u32) {
    if m == 1 {
        (y - 1, 12)
    } else {
        (y, m - 1)
    }
}

/// CalcMissingSpot: derive missing power values from U·I (SBFspot).
pub(crate) fn calc_missing_spot(inv: &mut InverterData) {
    for m in inv.mpp.values_mut() {
        if m.pdc == 0 {
            m.pdc = m.idc * m.udc / 100_000;
        }
    }
    if inv.pac1 == 0 {
        inv.pac1 = inv.iac1 * inv.uac1 / 100_000;
    }
    if inv.pac2 == 0 {
        inv.pac2 = inv.iac2 * inv.uac2 / 100_000;
    }
    if inv.pac3 == 0 {
        inv.pac3 = inv.iac3 * inv.uac3 / 100_000;
    }
    if inv.total_pac == 0 {
        inv.total_pac = inv.pac1 + inv.pac2 + inv.pac3;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::connection::{DeviceId, SyncOutcome};
    use crate::error::Error;

    #[derive(Default)]
    struct FlakyState {
        login_attempts: AtomicUsize,
        ended_sessions: AtomicUsize,
        requests: AtomicUsize,
    }

    struct FlakyLoginConnector {
        state: Arc<FlakyState>,
    }

    #[async_trait]
    impl Connection for FlakyLoginConnector {
        fn communication(
            &self,
        ) -> (
            smalog_observation::ProtocolFamily,
            smalog_observation::Transport,
        ) {
            (
                smalog_observation::ProtocolFamily::SmaData2Plus,
                smalog_observation::Transport::Ethernet,
            )
        }

        fn devices(&self) -> Vec<DeviceId> {
            vec![DeviceId {
                susy_id: 123,
                serial: 456,
                address: "test".into(),
            }]
        }

        fn user_group(&self) -> UserGroup {
            UserGroup::User
        }

        async fn begin(&mut self) -> Result<()> {
            Ok(())
        }

        async fn login_all(&mut self) -> Result<()> {
            if self.state.login_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(Error::Timeout)
            } else {
                Ok(())
            }
        }

        async fn request_all(
            &mut self,
            _command: u32,
            _first: u32,
            _last: u32,
            _events: bool,
        ) -> Result<HashMap<u32, Vec<Vec<u8>>>> {
            self.state.requests.fetch_add(1, Ordering::SeqCst);
            Ok(HashMap::new())
        }

        async fn end(&mut self) {
            self.state.ended_sessions.fetch_add(1, Ordering::SeqCst);
        }

        async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
            Ok(SyncOutcome::Unsupported)
        }
    }

    #[tokio::test]
    async fn failed_cycle_is_cleaned_up_and_next_cycle_can_succeed() {
        let state = Arc::new(FlakyState::default());
        let connector = FlakyLoginConnector {
            state: state.clone(),
        };
        let mut collector = Collector::new(Box::new(connector), Tz::UTC, PollOptions::default());

        assert!(matches!(
            collector.cycle(false, None).await,
            Err(Error::Timeout)
        ));
        assert_eq!(state.ended_sessions.load(Ordering::SeqCst), 1);

        let inverters = collector
            .cycle(false, None)
            .await
            .expect("the next poll cycle should retry");
        assert_eq!(inverters.len(), 1);
        assert_eq!(state.ended_sessions.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn full_probe_runs_every_applicable_spot_query() {
        let state = Arc::new(FlakyState::default());
        state.login_attempts.store(1, Ordering::SeqCst);
        let connector = FlakyLoginConnector {
            state: state.clone(),
        };
        let mut collector = Collector::new(Box::new(connector), Tz::UTC, PollOptions::default());

        collector.probe_all().await.expect("full probe");

        assert_eq!(state.requests.load(Ordering::SeqCst), 13);
        assert_eq!(state.ended_sessions.load(Ordering::SeqCst), 1);
    }
}
