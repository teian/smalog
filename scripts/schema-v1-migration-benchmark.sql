-- Deterministic one-year SBFspot schema-v1 source for migration benchmarks.
-- Load with:
--   sqlite3 benchmark-source.db < scripts/schema-v1-migration-benchmark.sql
--
-- 2000 is a leap year: 366 * 24 * 12 = 105,408 five-minute samples.
PRAGMA journal_mode = OFF;
PRAGMA synchronous = OFF;
BEGIN;

CREATE TABLE Config ("Key", "Value");
CREATE TABLE Inverters (
    Serial, Name, Type, SW_Version, TimeStamp, TotalPac, EToday, ETotal,
    OperatingTime, FeedInTime, Status, GridRelay, Temperature
);
CREATE TABLE SpotData (
    TimeStamp, Serial, Pdc1, Pdc2, Idc1, Idc2, Udc1, Udc2,
    Pac1, Pac2, Pac3, Iac1, Iac2, Iac3, Uac1, Uac2, Uac3,
    EToday, ETotal, Frequency, OperatingTime, FeedInTime, BT_Signal,
    Status, GridRelay, Temperature
);
CREATE TABLE SpotDataX (TimeStamp, Serial, "Key", Value);
CREATE TABLE DayData (TimeStamp, Serial, TotalYield, Power, PVoutput);
CREATE TABLE MonthData (TimeStamp, Serial, TotalYield, DayYield);
CREATE TABLE EventData (
    EntryID, TimeStamp, Serial, SusyID, EventCode, EventType, Category,
    EventGroup, Tag, OldValue, NewValue, UserGroup
);
CREATE TABLE Consumption (TimeStamp, EnergyUsed, PowerUsed);

INSERT INTO Config VALUES
    ('SchemaVersion', '1'),
    ('Plantname', 'Deterministic UTF-8 benchmark – Süd');
INSERT INTO Inverters VALUES
    (42, 'Dach Süd', 'STP benchmark', '1.0', 978306900, 600, 1000,
     5000000, 105408, 105000, 'OK', 'Closed', 21);

WITH RECURSIVE samples(n) AS (
    VALUES(0)
    UNION ALL
    SELECT n + 1 FROM samples WHERE n + 1 < 105408
)
INSERT INTO SpotData
SELECT
    946684800 + n * 300, 42,
    100 + (n * 37) % 4900, 100 + (n * 53) % 4900,
    1000 + n % 500, 1100 + n % 500,
    350000 + n % 1000, 360000 + n % 1000,
    100 + n % 900, 100 + n % 900, 100 + n % 900,
    1000, 1000, 1000, 230000, 230000, 230000,
    (n % 288) * 100, 1000000 + n * 17, 50000,
    n, n, 100, 'OK', 'Closed', 20 + n % 10
FROM samples;

WITH RECURSIVE samples(n) AS (
    VALUES(0)
    UNION ALL
    SELECT n + 1 FROM samples WHERE n + 1 < 105408
), trackers(tracker, key_base) AS (
    VALUES(1, 2432512), (7, 2432512), (255, 2432512)
)
INSERT INTO SpotDataX
SELECT
    946684800 + n * 300,
    42,
    key_base | tracker,
    100 + (n * (31 + tracker)) % 4900
FROM samples CROSS JOIN trackers;

WITH RECURSIVE samples(n) AS (
    VALUES(0)
    UNION ALL
    SELECT n + 1 FROM samples WHERE n + 1 < 105408
)
INSERT INTO DayData
SELECT
    946684800 + n * 300, 42, 1000000 + n * 17,
    300 + n % 2700, n % 2
FROM samples;

WITH RECURSIVE days(n) AS (
    VALUES(0)
    UNION ALL
    SELECT n + 1 FROM days WHERE n + 1 < 366
)
INSERT INTO MonthData
SELECT
    946684800 + n * 86400, 42,
    1000000 + n * 4896, 4896
FROM days;

INSERT INTO EventData VALUES
    (1, 946684800, 42, 125, 1001, 'Info', 'Grid', 'Benchmark',
     'Start ✓', NULL, 'running', 'Installer');
INSERT INTO Consumption VALUES (946684800, 100, 300);

COMMIT;
ANALYZE;
