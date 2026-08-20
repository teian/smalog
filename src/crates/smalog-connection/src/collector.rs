//! One poll loop for any [`Connection`].
//!
//! The collector owns the per-inverter [`InverterData`] state and runs the
//! SBFspot query sequence + archive fetches through the connector's
//! request primitive, decoding with the shared protocol layer. It is the
//! single, transport-agnostic replacement for SBFspot's per-transport
//! `Inverter::process()` loops.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Instant;

use chrono::{Datelike, Utc};
use chrono_tz::Tz;
use tracing::{debug, info, warn};

use crate::connection::{ClockMode, Connection, RequestReply, SyncOutcome, UserGroup};
use crate::error::Result;
use crate::query_support::QuerySupportStore;
use crate::smadata2::archive::{
    day_request_window, event_command, event_request_window, month_request_window,
    process_day_frames, process_event_frames, process_month_frames,
};
use crate::smadata2::commands::{QueryKind, CMD_ARCHIVE_DAY, CMD_ARCHIVE_MONTH};
use crate::smadata2::decode::decode_spot_records;
use crate::smadata2::inverter::InverterData;
use crate::speedwire::packet::Datagram;
use crate::transmission::{
    PollTransmission, TransmissionKind, TransmissionOutcome, TransmissionSink,
};
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
    /// Optional diagnostics channel; `None` means nothing is recorded.
    sink: Option<Arc<dyn TransmissionSink>>,
    /// Optional memory of the queries a device has refused; `None` means
    /// every query is asked every cycle.
    support: Option<Arc<dyn QuerySupportStore>>,
    /// This session's refusals, loaded from `support` once the devices are
    /// known: serial → query identifiers.
    refused: HashMap<u32, BTreeSet<String>>,
    /// Serials already told about their missing DC power, so the hint is
    /// logged once per process instead of once per poll cycle.
    dc_power_hinted: BTreeSet<u32>,
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
            sink: None,
            support: None,
            refused: HashMap::new(),
            dc_power_hinted: BTreeSet::new(),
        }
    }

    /// Stop asking for values the inverters have already refused, remembering
    /// them in `store` across restarts. Without it every query is sent every
    /// cycle, as before.
    #[must_use]
    pub fn with_query_support(mut self, store: Arc<dyn QuerySupportStore>) -> Collector {
        self.support = Some(store);
        self
    }

    /// Build a collector that also reports every exchange to `sink`.
    ///
    /// Recording is purely additive: the poll sequence, its results and its
    /// error handling are identical to [`Collector::new`].
    pub fn with_sink(
        connector: Box<dyn Connection>,
        tz: Tz,
        opts: PollOptions,
        sink: Arc<dyn TransmissionSink>,
    ) -> Collector {
        Collector {
            sink: Some(sink),
            ..Collector::new(connector, tz, opts)
        }
    }

    /// Time one connector request and record it as a transmission.
    ///
    /// A query every addressed device has already refused is not sent at all:
    /// it is reported as [`TransmissionOutcome::Unsupported`] and answered
    /// with an empty reply. Otherwise the request's result is returned
    /// unchanged, and with no sink configured nothing is recorded — including
    /// the device lookup and the clock reads.
    async fn request_recorded(
        &mut self,
        kind: TransmissionKind,
        command: u32,
        first: u32,
        last: u32,
        events: bool,
    ) -> Result<RequestReply> {
        let mut addressed: Vec<u32> = self.connector.devices().iter().map(|d| d.serial).collect();
        addressed.sort_unstable();

        if self.every_device_refused(kind, &addressed) {
            debug!(
                query = kind.as_str(),
                "skipping a query every device has refused"
            );
            self.record_skipped(kind, command, first, last, addressed);
            return Ok(RequestReply::default());
        }

        if self.sink.is_none() {
            let reply = self
                .connector
                .request_all(command, first, last, events)
                .await;
            if let Ok(reply) = &reply {
                self.remember_refusals(kind, reply);
            }
            return reply;
        }

        let started_at_ms = Utc::now().timestamp_millis();
        let clock = Instant::now();
        let result = self
            .connector
            .request_all(command, first, last, events)
            .await;
        let duration_ms = elapsed_ms(clock);
        let (protocol, transport) = self.connector.communication();
        let mut transmission = PollTransmission::step(
            started_at_ms,
            protocol,
            transport,
            kind,
            duration_ms,
            TransmissionOutcome::Ok,
        );
        transmission.command = Some(command);
        transmission.first_lri = Some(first);
        transmission.last_lri = Some(last);
        transmission.addressed_serials = addressed.clone();
        match &result {
            Ok(reply) => {
                for (serial, frames) in &reply.frames {
                    let count = u32::try_from(frames.len()).unwrap_or(u32::MAX);
                    if count > 0 {
                        transmission.frames_by_serial.insert(*serial, count);
                    }
                }
                transmission.total_frames = transmission
                    .frames_by_serial
                    .values()
                    .fold(0u32, |sum, count| sum.saturating_add(*count));
                if transmission.total_frames == 0 {
                    // Every device refusing is an answer; anything else is
                    // silence.
                    transmission.outcome = if !addressed.is_empty()
                        && addressed.iter().all(|s| reply.unsupported.contains(s))
                    {
                        TransmissionOutcome::Unsupported
                    } else {
                        TransmissionOutcome::Empty
                    };
                }
                self.remember_refusals(kind, reply);
            }
            Err(error) => {
                transmission.outcome = TransmissionOutcome::Failed;
                transmission.error = Some(error.to_string());
            }
        }
        if let Some(sink) = &self.sink {
            sink.record(transmission);
        }
        result
    }

    /// True when every addressed device is known to lack `kind`, so asking
    /// again would only cost a round trip.
    fn every_device_refused(&self, kind: TransmissionKind, addressed: &[u32]) -> bool {
        if self.support.is_none() || addressed.is_empty() {
            return false;
        }
        let query = kind.as_str();
        addressed.iter().all(|serial| {
            self.refused
                .get(serial)
                .is_some_and(|queries| queries.contains(query))
        })
    }

    /// Persist the refusals in `reply` and keep this session's set in step.
    fn remember_refusals(&mut self, kind: TransmissionKind, reply: &RequestReply) {
        let Some(store) = self.support.as_ref() else {
            return;
        };
        let query = kind.as_str();
        for serial in &reply.unsupported {
            let model = self
                .inverters
                .iter()
                .find(|inv| inv.serial == *serial)
                .map(|inv| inv.device_type.clone())
                .filter(|model| !model.is_empty());
            store.remember(*serial, query, model.as_deref());
            self.refused
                .entry(*serial)
                .or_default()
                .insert(query.to_owned());
        }
    }

    /// Record a query that was not sent because every device had refused it.
    fn record_skipped(
        &self,
        kind: TransmissionKind,
        command: u32,
        first: u32,
        last: u32,
        addressed: Vec<u32>,
    ) {
        let Some(sink) = self.sink.as_ref() else {
            return;
        };
        let (protocol, transport) = self.connector.communication();
        let mut transmission = PollTransmission::step(
            Utc::now().timestamp_millis(),
            protocol,
            transport,
            kind,
            0,
            TransmissionOutcome::Unsupported,
        );
        transmission.command = Some(command);
        transmission.first_lri = Some(first);
        transmission.last_lri = Some(last);
        transmission.addressed_serials = addressed;
        sink.record(transmission);
    }

    /// Load the remembered refusals for the devices of this session.
    fn load_refusals(&mut self) {
        self.refused.clear();
        let Some(store) = self.support.as_ref() else {
            return;
        };
        for device in self.connector.devices() {
            let queries = store.unsupported(device.serial);
            if !queries.is_empty() {
                debug!(
                    serial = device.serial,
                    queries = queries.len(),
                    "skipping queries this inverter has refused before"
                );
                self.refused.insert(device.serial, queries);
            }
        }
    }

    /// Record one session step (begin, login, clock sync, end).
    fn record_session_step(
        &self,
        kind: TransmissionKind,
        started_at_ms: i64,
        clock: Instant,
        outcome: TransmissionOutcome,
        error: Option<String>,
        detail: Option<String>,
    ) {
        let Some(sink) = &self.sink else {
            return;
        };
        let (protocol, transport) = self.connector.communication();
        let mut transmission = PollTransmission::step(
            started_at_ms,
            protocol,
            transport,
            kind,
            elapsed_ms(clock),
            outcome,
        );
        transmission.error = error;
        transmission.detail = detail;
        sink.record(transmission);
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
        self.begin_recorded().await?;
        self.rebuild_inverters();
        self.load_refusals();

        // Once begin() succeeds, always tear the session down. In particular,
        // a failed login must not leave Bluetooth or a partially logged-in
        // multi-inverter connection behind for the next poll cycle.
        let res = match self.login_recorded().await {
            Ok(()) => self.run(fetch_day, daily).await,
            Err(error) => Err(error),
        };
        self.end_recorded().await;

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
        self.begin_recorded().await?;
        self.rebuild_inverters();
        self.load_refusals();

        let result = match self.login_recorded().await {
            Ok(()) if all => Ok(self.query_sequence(false).await),
            Ok(()) => Ok(self.probe_sequence().await),
            Err(error) => Err(error),
        };
        self.end_recorded().await;

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
        let started_at_ms = Utc::now().timestamp_millis();
        let clock = Instant::now();
        let sync = self.connector.set_clock(ClockMode::Auto).await;
        match &sync {
            Ok(SyncOutcome::Set) => info!("inverter clock synchronised"),
            Ok(SyncOutcome::Skipped(r)) => debug!(reason = r, "clock sync skipped"),
            Ok(SyncOutcome::VerifyFailed { drift }) => warn!(drift, "clock sync not confirmed"),
            Ok(SyncOutcome::Unsupported) => {}
            Err(e) => warn!(error = %e, "clock sync failed"),
        }
        // A gated or unsupported clock sync is not a failure: the cycle does
        // not treat it as one, so neither does its transmission entry.
        let (outcome, error, detail) = match &sync {
            Ok(SyncOutcome::Set) => (TransmissionOutcome::Ok, None, None),
            Ok(SyncOutcome::Skipped(reason)) => {
                (TransmissionOutcome::Ok, None, Some((*reason).to_owned()))
            }
            Ok(SyncOutcome::Unsupported) => (
                TransmissionOutcome::Ok,
                None,
                Some("transport cannot set the inverter clock".to_owned()),
            ),
            Ok(SyncOutcome::VerifyFailed { drift }) => (
                TransmissionOutcome::Failed,
                Some(format!("clock written, {drift} s drift remaining")),
                None,
            ),
            Err(error) => (TransmissionOutcome::Failed, Some(error.to_string()), None),
        };
        self.record_session_step(
            TransmissionKind::ClockSync,
            started_at_ms,
            clock,
            outcome,
            error,
            detail,
        );
    }

    /// `Connection::begin`, recorded as its own transmission.
    async fn begin_recorded(&mut self) -> Result<()> {
        let started_at_ms = Utc::now().timestamp_millis();
        let clock = Instant::now();
        let begun = self.connector.begin().await;
        self.record_session_step(
            TransmissionKind::SessionBegin,
            started_at_ms,
            clock,
            outcome_of(&begun),
            begun.as_ref().err().map(|error| error.to_string()),
            None,
        );
        begun
    }

    /// `Connection::login_all`, recorded as its own transmission.
    async fn login_recorded(&mut self) -> Result<()> {
        let started_at_ms = Utc::now().timestamp_millis();
        let clock = Instant::now();
        let login = self.connector.login_all().await;
        self.record_session_step(
            TransmissionKind::Login,
            started_at_ms,
            clock,
            outcome_of(&login),
            login.as_ref().err().map(|error| error.to_string()),
            None,
        );
        login
    }

    /// `Connection::end`, recorded as its own transmission. Teardown is best
    /// effort and cannot fail, so its outcome is always `ok`.
    async fn end_recorded(&mut self) {
        let started_at_ms = Utc::now().timestamp_millis();
        let clock = Instant::now();
        self.connector.end().await;
        self.record_session_step(
            TransmissionKind::SessionEnd,
            started_at_ms,
            clock,
            TransmissionOutcome::Ok,
            None,
            None,
        );
    }

    fn index_of(&self, serial: u32) -> Option<usize> {
        self.inverters.iter().position(|i| i.serial == serial)
    }

    /// Run one spot query against all devices and decode replies into the
    /// matching inverter's state.
    async fn query(&mut self, kind: QueryKind) -> usize {
        let q = kind.query();
        let map = match self
            .request_recorded(
                TransmissionKind::Spot(kind),
                q.command,
                q.first,
                q.last,
                false,
            )
            .await
        {
            Ok(m) => m,
            Err(e) => {
                warn!(?kind, error = %e, "query failed");
                return 0;
            }
        };
        let received_frames = map.total_frames();
        for (serial, frames) in map.frames {
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
            } else if inv.dc_power_missing() && self.dc_power_hinted.insert(inv.serial) {
                // Silently storing zero would look like a dead string, and
                // the number to fix it is one the operator has to set.
                warn!(
                    serial = inv.serial,
                    "inverter reports DC voltage and current but no DC power; \
                     set service.calc_missing_spot = true to derive it (SBFspot \
                     CalculateMissingSpotValues)"
                );
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
            .request_recorded(
                TransmissionKind::ETodayFallback,
                CMD_ARCHIVE_DAY,
                first,
                last,
                false,
            )
            .await
        else {
            return;
        };
        for (serial, frames) in map.frames {
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
            .request_recorded(
                TransmissionKind::DayArchive,
                CMD_ARCHIVE_DAY,
                first,
                last,
                false,
            )
            .await
        {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "day archive failed");
                return;
            }
        };
        for (serial, frames) in map.frames {
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
            .request_recorded(
                TransmissionKind::MonthOffsetProbe,
                CMD_ARCHIVE_MONTH,
                first,
                last,
                false,
            )
            .await
        else {
            return;
        };
        for (serial, frames) in map.frames {
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
                .request_recorded(
                    TransmissionKind::MonthArchive,
                    CMD_ARCHIVE_MONTH,
                    first,
                    last,
                    false,
                )
                .await
            {
                for (serial, frames) in map.frames {
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
                    .request_recorded(
                        TransmissionKind::EventArchive,
                        event_command(group),
                        first,
                        last,
                        true,
                    )
                    .await
                {
                    for (serial, frames) in map.frames {
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

/// Outcome of a session step that either succeeded or returned an error.
fn outcome_of<T>(result: &Result<T>) -> TransmissionOutcome {
    match result {
        Ok(_) => TransmissionOutcome::Ok,
        Err(_) => TransmissionOutcome::Failed,
    }
}

/// Elapsed milliseconds, saturating rather than wrapping on a stalled link.
fn elapsed_ms(clock: Instant) -> u32 {
    u32::try_from(clock.elapsed().as_millis()).unwrap_or(u32::MAX)
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
        ) -> Result<RequestReply> {
            self.state.requests.fetch_add(1, Ordering::SeqCst);
            Ok(RequestReply::default())
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

    #[derive(Default)]
    pub(super) struct RecordingSink {
        recorded: std::sync::Mutex<Vec<PollTransmission>>,
    }

    impl RecordingSink {
        fn all(&self) -> Vec<PollTransmission> {
            self.recorded.lock().expect("sink lock").clone()
        }

        fn kinds(&self) -> Vec<&'static str> {
            self.all()
                .iter()
                .map(|transmission| transmission.kind.as_str())
                .collect()
        }

        pub(super) fn of_kind(&self, kind: &str) -> Vec<PollTransmission> {
            self.all()
                .into_iter()
                .filter(|transmission| transmission.kind.as_str() == kind)
                .collect()
        }
    }

    impl TransmissionSink for RecordingSink {
        fn record(&self, transmission: PollTransmission) {
            self.recorded.lock().expect("sink lock").push(transmission);
        }
    }

    /// Two devices behind one collector; the first answers with two frames,
    /// the second with one, so per-serial counts are distinguishable.
    struct TwoInverterConnector {
        fail_requests: bool,
    }

    #[async_trait]
    impl Connection for TwoInverterConnector {
        fn communication(
            &self,
        ) -> (
            smalog_observation::ProtocolFamily,
            smalog_observation::Transport,
        ) {
            (
                smalog_observation::ProtocolFamily::SmaData2Plus,
                smalog_observation::Transport::Bluetooth,
            )
        }

        fn devices(&self) -> Vec<DeviceId> {
            vec![
                DeviceId {
                    susy_id: 1,
                    serial: 22,
                    address: "b".into(),
                },
                DeviceId {
                    susy_id: 1,
                    serial: 11,
                    address: "a".into(),
                },
            ]
        }

        fn user_group(&self) -> UserGroup {
            UserGroup::User
        }

        async fn begin(&mut self) -> Result<()> {
            Ok(())
        }

        async fn login_all(&mut self) -> Result<()> {
            Ok(())
        }

        async fn request_all(
            &mut self,
            _command: u32,
            _first: u32,
            _last: u32,
            _events: bool,
        ) -> Result<RequestReply> {
            if self.fail_requests {
                return Err(Error::Timeout);
            }
            Ok(RequestReply::from_frames(HashMap::from([
                (11u32, vec![vec![0u8; 8], vec![0u8; 8]]),
                (22u32, vec![vec![0u8; 8]]),
            ])))
        }

        async fn end(&mut self) {}

        async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
            Ok(SyncOutcome::Skipped("disabled in test"))
        }
    }

    fn collector_with(connector: TwoInverterConnector) -> (Collector, Arc<RecordingSink>) {
        let sink = Arc::new(RecordingSink::default());
        let collector = Collector::with_sink(
            Box::new(connector),
            Tz::UTC,
            PollOptions::default(),
            sink.clone(),
        );
        (collector, sink)
    }

    #[tokio::test]
    async fn cycle_records_every_session_step_once() {
        let (mut collector, sink) = collector_with(TwoInverterConnector {
            fail_requests: false,
        });

        collector.cycle(false, None).await.expect("cycle");

        let kinds = sink.kinds();
        for step in [
            "session.begin",
            "session.login",
            "session.clock_sync",
            "session.end",
        ] {
            assert_eq!(
                kinds.iter().filter(|kind| **kind == step).count(),
                1,
                "expected exactly one {step} entry in {kinds:?}"
            );
        }
        assert_eq!(kinds.first().copied(), Some("session.begin"));
        assert_eq!(kinds.last().copied(), Some("session.end"));
    }

    #[tokio::test]
    async fn skipped_clock_sync_is_recorded_as_ok_with_its_reason() {
        let (mut collector, sink) = collector_with(TwoInverterConnector {
            fail_requests: false,
        });

        collector.cycle(false, None).await.expect("cycle");

        let recorded = sink.of_kind("session.clock_sync");
        let sync = recorded.first().expect("clock sync entry");
        assert_eq!(sync.outcome, TransmissionOutcome::Ok);
        assert_eq!(sync.error, None);
        assert_eq!(sync.detail.as_deref(), Some("disabled in test"));
    }

    #[tokio::test]
    async fn request_records_per_serial_frame_counts() {
        let (mut collector, sink) = collector_with(TwoInverterConnector {
            fail_requests: false,
        });

        collector.cycle(false, None).await.expect("cycle");

        let recorded = sink.of_kind("spot.ac_total_power");
        let request = recorded.first().expect("ac total power entry");
        assert_eq!(request.outcome, TransmissionOutcome::Ok);
        assert_eq!(request.total_frames, 3);
        assert_eq!(request.frames_by_serial.get(&11), Some(&2));
        assert_eq!(request.frames_by_serial.get(&22), Some(&1));
        assert_eq!(request.addressed_serials, vec![11, 22]);
        assert_eq!(
            request.command,
            Some(QueryKind::SpotAcTotalPower.query().command)
        );
        assert_eq!(
            request.first_lri,
            Some(QueryKind::SpotAcTotalPower.query().first)
        );
        assert_eq!(request.error, None);
    }

    #[tokio::test]
    async fn failing_request_is_recorded_as_failed_with_its_error() {
        let (mut collector, sink) = collector_with(TwoInverterConnector {
            fail_requests: true,
        });

        collector.cycle(false, None).await.expect("cycle");

        let recorded = sink.of_kind("spot.ac_total_power");
        let request = recorded.first().expect("ac total power entry");
        assert_eq!(request.outcome, TransmissionOutcome::Failed);
        assert_eq!(request.total_frames, 0);
        assert!(request.frames_by_serial.is_empty());
        assert_eq!(request.error, Some(Error::Timeout.to_string()));
    }

    #[tokio::test]
    async fn request_answered_by_nobody_is_recorded_as_empty() {
        let sink = Arc::new(RecordingSink::default());
        let state = Arc::new(FlakyState::default());
        state.login_attempts.store(1, Ordering::SeqCst);
        let mut collector = Collector::with_sink(
            Box::new(FlakyLoginConnector { state }),
            Tz::UTC,
            PollOptions::default(),
            sink.clone(),
        );

        collector.cycle(false, None).await.expect("cycle");

        let recorded = sink.of_kind("spot.ac_total_power");
        let request = recorded.first().expect("ac total power entry");
        assert_eq!(request.outcome, TransmissionOutcome::Empty);
        assert_eq!(request.total_frames, 0);
        assert_eq!(request.error, None);
    }

    #[tokio::test]
    async fn failed_login_records_the_login_step_and_no_data_request() {
        let sink = Arc::new(RecordingSink::default());
        let mut collector = Collector::with_sink(
            Box::new(FlakyLoginConnector {
                state: Arc::new(FlakyState::default()),
            }),
            Tz::UTC,
            PollOptions::default(),
            sink.clone(),
        );

        assert!(matches!(
            collector.cycle(false, None).await,
            Err(Error::Timeout)
        ));

        let kinds = sink.kinds();
        assert_eq!(kinds, vec!["session.begin", "session.login", "session.end"]);
        let recorded = sink.of_kind("session.login");
        let login = recorded.first().expect("login entry");
        assert_eq!(login.outcome, TransmissionOutcome::Failed);
        assert_eq!(login.error, Some(Error::Timeout.to_string()));
    }

    #[tokio::test]
    async fn no_sink_records_nothing_and_keeps_the_cycle_intact() {
        let state = Arc::new(FlakyState::default());
        state.login_attempts.store(1, Ordering::SeqCst);
        let mut collector = Collector::new(
            Box::new(FlakyLoginConnector {
                state: state.clone(),
            }),
            Tz::UTC,
            PollOptions::default(),
        );

        let inverters = collector.cycle(false, None).await.expect("cycle");

        assert_eq!(inverters.len(), 1);
        assert_eq!(state.requests.load(Ordering::SeqCst), 13);
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

/// Skipping the queries an inverter has refused, and telling that refusal
/// apart from silence.
#[cfg(test)]
mod query_support_tests {
    use super::tests::RecordingSink;
    use super::*;
    use crate::connection::DeviceId;
    use crate::connection::SyncOutcome;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    const SERIAL: u32 = 2_100_123_456;

    /// Answers every request with "LRI not available", counting the
    /// requests that actually reached it.
    struct RefusingConnector {
        requests: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Connection for RefusingConnector {
        fn communication(
            &self,
        ) -> (
            smalog_observation::ProtocolFamily,
            smalog_observation::Transport,
        ) {
            (
                smalog_observation::ProtocolFamily::SmaData2Plus,
                smalog_observation::Transport::Bluetooth,
            )
        }

        fn devices(&self) -> Vec<DeviceId> {
            vec![DeviceId {
                susy_id: 123,
                serial: SERIAL,
                address: "00:80:25:2E:45:D6".into(),
            }]
        }

        fn user_group(&self) -> UserGroup {
            UserGroup::User
        }

        async fn begin(&mut self) -> Result<()> {
            Ok(())
        }

        async fn login_all(&mut self) -> Result<()> {
            Ok(())
        }

        async fn request_all(
            &mut self,
            _command: u32,
            _first: u32,
            _last: u32,
            _events: bool,
        ) -> Result<RequestReply> {
            self.requests.fetch_add(1, Ordering::SeqCst);
            let mut reply = RequestReply::default();
            reply.unsupported.insert(SERIAL);
            Ok(reply)
        }

        async fn end(&mut self) {}

        async fn set_clock(&mut self, _mode: ClockMode) -> Result<SyncOutcome> {
            Ok(SyncOutcome::Unsupported)
        }
    }

    /// Remembers refusals in memory, and counts what it was told.
    #[derive(Default)]
    struct MemoryStore {
        refused: Mutex<HashMap<u32, BTreeSet<String>>>,
        writes: AtomicUsize,
    }

    impl QuerySupportStore for MemoryStore {
        fn unsupported(&self, serial: u32) -> BTreeSet<String> {
            self.refused
                .lock()
                .expect("store lock")
                .get(&serial)
                .cloned()
                .unwrap_or_default()
        }

        fn remember(&self, serial: u32, query: &str, _model: Option<&str>) {
            self.writes.fetch_add(1, Ordering::SeqCst);
            self.refused
                .lock()
                .expect("store lock")
                .entry(serial)
                .or_default()
                .insert(query.to_owned());
        }
    }

    fn collector(
        requests: Arc<AtomicUsize>,
        sink: Arc<RecordingSink>,
        store: Arc<MemoryStore>,
    ) -> Collector {
        Collector::with_sink(
            Box::new(RefusingConnector { requests }),
            Tz::UTC,
            PollOptions::default(),
            sink,
        )
        .with_query_support(store)
    }

    #[tokio::test]
    async fn a_refused_query_is_reported_as_unsupported_not_as_silence() {
        let sink = Arc::new(RecordingSink::default());
        let store = Arc::new(MemoryStore::default());
        let mut collector = collector(Arc::new(AtomicUsize::new(0)), sink.clone(), store);

        collector.cycle(false, None).await.expect("cycle");

        let spot = sink.of_kind("spot.ac_total_power");
        let first = spot.first().expect("the query was recorded");
        assert_eq!(first.outcome, TransmissionOutcome::Unsupported);
        assert_eq!(first.total_frames, 0);
    }

    #[tokio::test]
    async fn a_remembered_refusal_is_not_asked_again() {
        let requests = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(MemoryStore::default());

        let mut first = collector(
            requests.clone(),
            Arc::new(RecordingSink::default()),
            store.clone(),
        );
        first.cycle(false, None).await.expect("first cycle");
        let asked_once = requests.load(Ordering::SeqCst);
        assert!(asked_once > 0, "the first cycle has to ask");
        let stored_writes = store.writes.load(Ordering::SeqCst);
        assert!(stored_writes > 0, "refusals have to be remembered");

        // A second collector over the same store: every query it would send
        // is already known to be refused.
        let sink = Arc::new(RecordingSink::default());
        let mut second = collector(requests.clone(), sink.clone(), store.clone());
        second.cycle(false, None).await.expect("second cycle");

        assert_eq!(
            requests.load(Ordering::SeqCst),
            asked_once,
            "no request may reach an inverter that already refused"
        );
        assert_eq!(
            store.writes.load(Ordering::SeqCst),
            stored_writes,
            "a skipped query has nothing new to store"
        );
        let spot = sink.of_kind("spot.ac_total_power");
        let skipped = spot.first().expect("the skipped query is still recorded");
        assert_eq!(skipped.outcome, TransmissionOutcome::Unsupported);
        assert_eq!(skipped.duration_ms, 0, "a skipped query costs no time");
    }

    #[tokio::test]
    async fn without_a_store_every_query_is_asked_every_cycle() {
        let requests = Arc::new(AtomicUsize::new(0));
        let mut collector = Collector::new(
            Box::new(RefusingConnector {
                requests: requests.clone(),
            }),
            Tz::UTC,
            PollOptions::default(),
        );

        collector.cycle(false, None).await.expect("first cycle");
        let first = requests.load(Ordering::SeqCst);
        collector.cycle(false, None).await.expect("second cycle");

        assert_eq!(
            requests.load(Ordering::SeqCst),
            first * 2,
            "with no memory configured the poll sequence is unchanged"
        );
    }
}
