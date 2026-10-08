-- One current announcement per device, shared across its crews.
-- Receipt time is Unix milliseconds; client run and seat times use the DuoClip global clock.
CREATE TABLE device_presence (
  device_id       TEXT PRIMARY KEY REFERENCES devices(device_id) ON DELETE CASCADE,
  game            TEXT,
  active_crew     TEXT,
  seated_since_ms INTEGER CHECK (seated_since_ms >= 0),
  seq             INTEGER NOT NULL CHECK (seq >= 0),
  online_since_ms INTEGER NOT NULL CHECK (online_since_ms >= 0),
  seen_at_ms      INTEGER NOT NULL
);
-- No receipt-time index: the private-group table is small, and each index adds heartbeat writes.
