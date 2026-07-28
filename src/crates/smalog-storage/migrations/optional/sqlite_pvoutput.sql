CREATE TABLE IF NOT EXISTS pvoutput_exports (
    inverter_id INTEGER NOT NULL
        REFERENCES inverters(inverter_id) ON DELETE CASCADE,
    measured_at INTEGER NOT NULL,
    exported_at INTEGER,
    attempts    INTEGER NOT NULL DEFAULT 0,
    last_error  TEXT,
    PRIMARY KEY (inverter_id, measured_at)
);

INSERT INTO schema_metadata (key, value)
VALUES ('pvoutput_exports_version', '1')
ON CONFLICT (key) DO UPDATE SET value = excluded.value;
