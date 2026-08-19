-- Optional runtime diagnostics: the Poll Cycle transmission ring. Pruned by
-- age and by row count, and not part of the canonical schema-v1 model — no
-- canonical read or write depends on it.
--
-- The captured application log is not stored here. It lives in a
-- process-memory buffer: persisting a log line would put a database write
-- behind every tracing call, for data that is disposable by nature.
--
-- AUTOINCREMENT is required, not decorative: pruning deletes rows constantly,
-- and plain rowids would be reused, breaking the monotonic cursor the read
-- endpoints page with.

CREATE TABLE IF NOT EXISTS poll_transmissions (
    transmission_id INTEGER PRIMARY KEY AUTOINCREMENT,
    -- Unix epoch milliseconds; the exchange's start.
    occurred_at     INTEGER NOT NULL,
    target          TEXT NOT NULL,
    transport       TEXT NOT NULL,
    protocol        TEXT NOT NULL,
    request_kind    TEXT NOT NULL,
    command         INTEGER,
    first_lri       INTEGER,
    last_lri        INTEGER,
    duration_ms     INTEGER NOT NULL CHECK (duration_ms >= 0),
    total_frames    INTEGER NOT NULL CHECK (total_frames >= 0),
    outcome         TEXT NOT NULL CHECK (outcome IN ('ok', 'empty', 'unsupported', 'failed')),
    -- Set only when outcome = 'failed'.
    error           TEXT,
    -- Note for a successful exchange, such as a clock-sync skip reason.
    detail          TEXT
);

-- One row per serial the exchange addressed or that answered it, so the
-- serial filter is an indexed join and pruning a parent prunes its children.
CREATE TABLE IF NOT EXISTS poll_transmission_devices (
    transmission_id INTEGER NOT NULL
        REFERENCES poll_transmissions(transmission_id) ON DELETE CASCADE,
    serial_number   INTEGER NOT NULL,
    frame_count     INTEGER NOT NULL DEFAULT 0 CHECK (frame_count >= 0),
    addressed       INTEGER NOT NULL DEFAULT 1 CHECK (addressed IN (0, 1)),
    PRIMARY KEY (transmission_id, serial_number)
);

-- Pruning by age.
CREATE INDEX IF NOT EXISTS poll_transmissions_occurred_at_idx
    ON poll_transmissions (occurred_at);

-- Keyset paging under each supported filter. The descending id keeps a
-- selective filter an index seek instead of a backwards scan of the ring.
CREATE INDEX IF NOT EXISTS poll_transmissions_outcome_id_idx
    ON poll_transmissions (outcome, transmission_id DESC);
CREATE INDEX IF NOT EXISTS poll_transmissions_target_id_idx
    ON poll_transmissions (target, transmission_id DESC);
CREATE INDEX IF NOT EXISTS poll_transmission_devices_serial_id_idx
    ON poll_transmission_devices (serial_number, transmission_id DESC);

INSERT INTO schema_metadata (key, value)
VALUES ('diagnostics_version', '2')
ON CONFLICT (key) DO UPDATE SET value = excluded.value;
