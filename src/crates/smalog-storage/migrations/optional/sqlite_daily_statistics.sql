CREATE TABLE IF NOT EXISTS inverter_daily_statistics (
    inverter_id                INTEGER NOT NULL
        REFERENCES inverters(inverter_id) ON DELETE CASCADE,
    statistics_date            TEXT NOT NULL
        CHECK (
            statistics_date GLOB
                '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(statistics_date, '+0 days') = statistics_date
        ),
    peak_ac_power_w            INTEGER,
    peak_dc_power_w            INTEGER,
    mean_ac_power_w            INTEGER,
    mean_dc_power_w            INTEGER,
    measurement_count          INTEGER NOT NULL CHECK (measurement_count >= 0),
    expected_measurement_count INTEGER
        CHECK (
            expected_measurement_count IS NULL
            OR expected_measurement_count >= 0
        ),
    first_measurement_at       INTEGER,
    last_measurement_at        INTEGER,
    is_complete                INTEGER NOT NULL DEFAULT 0
        CHECK (is_complete IN (0, 1)),
    calculated_at              INTEGER NOT NULL,
    source_max_measured_at     INTEGER,
    PRIMARY KEY (inverter_id, statistics_date)
);

CREATE INDEX IF NOT EXISTS inverter_daily_statistics_date_inverter_idx
    ON inverter_daily_statistics (statistics_date, inverter_id);

INSERT INTO schema_metadata (key, value)
VALUES ('daily_statistics_version', '1')
ON CONFLICT (key) DO UPDATE SET value = excluded.value;
