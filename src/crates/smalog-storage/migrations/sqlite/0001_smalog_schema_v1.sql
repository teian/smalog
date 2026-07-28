CREATE TABLE schema_metadata (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT INTO schema_metadata (key, value) VALUES
    ('schema_version', '1'),
    ('created_by', 'smalog'),
    ('implementation_version', '1');

CREATE TABLE inverters (
    inverter_id      INTEGER PRIMARY KEY AUTOINCREMENT,
    serial_number    INTEGER NOT NULL UNIQUE
        CHECK (serial_number BETWEEN 0 AND 4294967295),
    susy_id          INTEGER,
    configured_name  TEXT,
    device_name      TEXT,
    model            TEXT,
    firmware_version TEXT,
    transport        TEXT
        CHECK (transport IS NULL OR transport IN ('ethernet', 'bluetooth')),
    first_seen_at    INTEGER,
    last_seen_at     INTEGER
);

CREATE TABLE inverter_measurements (
    measurement_id               INTEGER PRIMARY KEY AUTOINCREMENT,
    inverter_id                  INTEGER NOT NULL
        REFERENCES inverters(inverter_id) ON DELETE RESTRICT,
    measured_at                  INTEGER NOT NULL,
    ac_power_l1_w                INTEGER,
    ac_power_l2_w                INTEGER,
    ac_power_l3_w                INTEGER,
    ac_current_l1_ma             INTEGER,
    ac_current_l2_ma             INTEGER,
    ac_current_l3_ma             INTEGER,
    ac_voltage_l1_mv             INTEGER,
    ac_voltage_l2_mv             INTEGER,
    ac_voltage_l3_mv             INTEGER,
    grid_frequency_mhz           INTEGER,
    grid_import_power_w          INTEGER,
    grid_export_power_w          INTEGER,
    energy_today_wh              INTEGER,
    energy_total_wh              INTEGER,
    operating_time_s             INTEGER,
    feed_in_time_s               INTEGER,
    device_status_code           INTEGER,
    grid_relay_status_code       INTEGER,
    temperature_millicelsius     INTEGER,
    bluetooth_signal_permille    INTEGER
        CHECK (
            bluetooth_signal_permille IS NULL
            OR bluetooth_signal_permille BETWEEN 0 AND 1000
        )
);

CREATE UNIQUE INDEX inverter_measurements_inverter_time_uq
    ON inverter_measurements (inverter_id, measured_at);

CREATE INDEX inverter_measurements_time_inverter_idx
    ON inverter_measurements (measured_at, inverter_id);

CREATE TABLE mppt_measurements (
    measurement_id INTEGER NOT NULL
        REFERENCES inverter_measurements(measurement_id) ON DELETE CASCADE,
    tracker_number INTEGER NOT NULL CHECK (tracker_number BETWEEN 1 AND 255),
    dc_power_w     INTEGER,
    dc_current_ma  INTEGER,
    dc_voltage_mv  INTEGER,
    PRIMARY KEY (measurement_id, tracker_number)
);

CREATE TABLE battery_measurements (
    measurement_id           INTEGER PRIMARY KEY
        REFERENCES inverter_measurements(measurement_id) ON DELETE CASCADE,
    state_of_charge_permille INTEGER
        CHECK (
            state_of_charge_permille IS NULL
            OR state_of_charge_permille BETWEEN 0 AND 1000
        ),
    voltage_mv               INTEGER,
    current_ma               INTEGER,
    temperature_millicelsius INTEGER
);

CREATE TABLE inverter_energy_samples (
    inverter_id     INTEGER NOT NULL
        REFERENCES inverters(inverter_id) ON DELETE RESTRICT,
    measured_at     INTEGER NOT NULL,
    total_energy_wh INTEGER,
    power_w         INTEGER,
    PRIMARY KEY (inverter_id, measured_at)
);

CREATE INDEX inverter_energy_samples_time_inverter_idx
    ON inverter_energy_samples (measured_at, inverter_id);

CREATE TABLE inverter_daily_yields (
    inverter_id     INTEGER NOT NULL
        REFERENCES inverters(inverter_id) ON DELETE RESTRICT,
    yield_date      TEXT NOT NULL
        CHECK (
            yield_date GLOB
                '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(yield_date, '+0 days') = yield_date
        ),
    total_energy_wh INTEGER,
    daily_energy_wh INTEGER,
    is_complete     INTEGER NOT NULL DEFAULT 0 CHECK (is_complete IN (0, 1)),
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (inverter_id, yield_date)
);

CREATE INDEX inverter_daily_yields_date_inverter_idx
    ON inverter_daily_yields (yield_date, inverter_id);

CREATE TABLE inverter_events (
    inverter_id     INTEGER NOT NULL
        REFERENCES inverters(inverter_id) ON DELETE RESTRICT,
    device_event_id INTEGER NOT NULL,
    occurred_at     INTEGER NOT NULL,
    event_code      INTEGER,
    event_type      TEXT,
    category        TEXT,
    event_group     TEXT,
    tag             TEXT,
    old_value       TEXT,
    new_value       TEXT,
    user_group      TEXT,
    PRIMARY KEY (inverter_id, device_event_id)
);

CREATE INDEX inverter_events_inverter_time_idx
    ON inverter_events (inverter_id, occurred_at DESC);

CREATE TABLE site_consumption_measurements (
    measured_at        INTEGER PRIMARY KEY,
    consumed_energy_wh INTEGER,
    consumed_power_w   INTEGER
);

CREATE TABLE migration_runs (
    migration_run_id    INTEGER PRIMARY KEY AUTOINCREMENT,
    source_fingerprint  TEXT NOT NULL,
    source_identity     TEXT NOT NULL,
    source_schema       TEXT NOT NULL,
    timezone            TEXT NOT NULL,
    started_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    completed_at        INTEGER,
    status              TEXT NOT NULL
        CHECK (status IN ('running', 'interrupted', 'completed', 'failed')),
    rows_processed      INTEGER NOT NULL DEFAULT 0 CHECK (rows_processed >= 0),
    categories_completed INTEGER NOT NULL DEFAULT 0
        CHECK (categories_completed >= 0),
    last_error          TEXT,
    report_metadata     TEXT
);

CREATE TABLE migration_checkpoints (
    migration_run_id  INTEGER NOT NULL
        REFERENCES migration_runs(migration_run_id) ON DELETE CASCADE,
    category          TEXT NOT NULL,
    source_table      TEXT NOT NULL,
    last_key          TEXT,
    rows_processed    INTEGER NOT NULL DEFAULT 0 CHECK (rows_processed >= 0),
    batches_processed INTEGER NOT NULL DEFAULT 0 CHECK (batches_processed >= 0),
    status            TEXT NOT NULL
        CHECK (status IN ('pending', 'running', 'completed')),
    started_at        INTEGER,
    updated_at        INTEGER NOT NULL,
    completed_at      INTEGER,
    report_metadata   TEXT,
    PRIMARY KEY (migration_run_id, category)
);

-- Categories not yet mapped to canonical tables retain bounded resumable
-- source keys until their Phase-4 mapper replaces the staging write.
CREATE TABLE migration_staged_rows (
    migration_run_id INTEGER NOT NULL
        REFERENCES migration_runs(migration_run_id) ON DELETE CASCADE,
    category         TEXT NOT NULL,
    source_key       TEXT NOT NULL,
    payload          TEXT NOT NULL,
    PRIMARY KEY (migration_run_id, category, source_key)
);
