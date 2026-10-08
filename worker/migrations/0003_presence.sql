-- One current announcement per device, shared across its crews.
-- Receipt time is Unix milliseconds; online_since_ms is opaque client monotonic time.
CREATE TABLE device_presence (
  device_id       TEXT PRIMARY KEY REFERENCES devices(device_id) ON DELETE CASCADE,
  game            TEXT,
  active_crew     TEXT,
  seq             INTEGER NOT NULL CHECK (seq >= 0),
  online_since_ms INTEGER NOT NULL CHECK (online_since_ms >= 0),
  seen_at_ms      INTEGER NOT NULL
);
CREATE INDEX idx_device_presence_seen_at ON device_presence (seen_at_ms);
