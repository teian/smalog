-- Which queries an inverter has answered "LRI not available" to (SMA error
-- 21). The answer depends on the model and its firmware, so repeating the
-- question every poll cycle buys a round trip for an answer that will not
-- change. Remembering it lets the collector skip the query.
--
-- Keyed by serial, not by model: two inverters of the same type can run
-- different firmware, and one refusing says nothing about the other. The
-- model is recorded for the operator, not as the key.
--
-- recorded_at makes the memory expire: an entry older than the collector's
-- recheck window is asked again, so a firmware update that adds a value is
-- picked up without anyone clearing a table.

CREATE TABLE inverter_query_support (
    serial_number BIGINT NOT NULL,
    -- Transmission kind identifier, e.g. 'spot.inverter_temperature'.
    query         TEXT NOT NULL,
    -- Device type as reported by the inverter, when it was known.
    model         TEXT,
    -- Unix epoch seconds of the most recent refusal.
    recorded_at   BIGINT NOT NULL,
    PRIMARY KEY (serial_number, query)
);
