-- DuoClip Worker: abuse limits (security review).
-- All days are UTC calendar days formatted "YYYY-MM-DD".

-- Atomic per-day counters for the service-wide and per-device circuit breakers.
-- `scope` is one of:
--   'devices'            new devices registered today (service wide)
--   'bytes'              bytes announced for upload today (service wide)
--   'join:<device_id>'   join attempts of one device today
--   'clips:<device_id>'  clips registered by one device today
-- Device ids are validated uuids, so a scope can never be forged from user input.
CREATE TABLE counters (
  scope TEXT NOT NULL,
  day   TEXT NOT NULL,
  n     INTEGER NOT NULL,
  PRIMARY KEY (scope, day)
) WITHOUT ROWID;
CREATE INDEX idx_counters_day ON counters (day);

-- Lookups for the per-device crew limit and the per-crew active invite limit.
CREATE INDEX idx_crews_created_by ON crews (created_by);
CREATE INDEX idx_invites_crew ON invites (crew_id);
