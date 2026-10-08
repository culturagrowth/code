-- DuoClip Worker: initial schema (Cloudflare D1 / SQLite).
-- All timestamps are unix milliseconds unless stated otherwise.

-- One row per app installation. The private key never leaves the PC.
CREATE TABLE devices (
  device_id      TEXT PRIMARY KEY,           -- lowercase hyphenated uuid
  public_key_b64 TEXT NOT NULL,              -- standard base64 of the 32 raw Ed25519 bytes
  display_name   TEXT NOT NULL,
  created_at     INTEGER NOT NULL
);

-- A crew is a private group of friends who exchange clips.
CREATE TABLE crews (
  crew_id    TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE crew_members (
  crew_id   TEXT NOT NULL,
  device_id TEXT NOT NULL,
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (crew_id, device_id)
);
CREATE INDEX idx_crew_members_device ON crew_members (device_id);

-- Short-lived invite codes (10 chars of [A-Z2-9]).
CREATE TABLE invites (
  code       TEXT PRIMARY KEY,
  crew_id    TEXT NOT NULL,
  created_by TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  uses_left  INTEGER NOT NULL
);
CREATE INDEX idx_invites_expires_at ON invites (expires_at);

-- Registry of clips; the objects themselves live in R2 under clips/{crew}/{clip}/.
-- `bytes` is the total announced upload size, used for the per-clip quota.
CREATE TABLE clips (
  clip_id    TEXT PRIMARY KEY,
  crew_id    TEXT NOT NULL,
  owner      TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  deleted_at INTEGER,
  bytes      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_clips_expires_at ON clips (expires_at);

-- Announced size of each chunk, so re-requesting a URL for the same chunk is not double counted.
CREATE TABLE clip_chunks (
  clip_id TEXT NOT NULL,
  pov     TEXT NOT NULL,
  quality TEXT NOT NULL,
  idx     INTEGER NOT NULL,
  bytes   INTEGER NOT NULL,
  PRIMARY KEY (clip_id, pov, quality, idx)
) WITHOUT ROWID;

-- Bytes announced per device per UTC day ("YYYY-MM-DD").
CREATE TABLE usage (
  device_id TEXT NOT NULL,
  day       TEXT NOT NULL,
  bytes     INTEGER NOT NULL,
  PRIMARY KEY (device_id, day)
);

-- Replay protection: request signatures seen recently.
CREATE TABLE seen_signatures (
  sig     TEXT PRIMARY KEY,
  seen_at INTEGER NOT NULL
);
CREATE INDEX idx_seen_signatures_seen_at ON seen_signatures (seen_at);
