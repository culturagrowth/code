/**
 * Typed D1 queries.
 *
 * Handlers only see the {@link Db} interface, so tests can run them against any implementation
 * (the test-suite runs the real SQL in this file on an in-memory SQLite database).
 *
 * Every write that has to be race-free is a single conditional SQL statement, because D1 has no
 * interactive transactions: the statement itself decides, and `meta.changes` reports the outcome.
 */

import type { Quality } from "./keys.js";
import type { QuotaVerdict } from "./quota.js";
import type { PresenceInput } from "./validate.js";

/** A registered device. */
export interface DeviceRow {
  device_id: string;
  public_key_b64: string;
  display_name: string;
  created_at: number;
}

/** A crew member as returned by `GET /v1/crews/:crew/members`. */
export interface MemberRow {
  device_id: string;
  display_name: string;
}

/** Available online crew member, before adding the derived expiry to the API response. */
export interface PresenceRow extends MemberRow, PresenceInput {
  seen_at_ms: number;
}

/** An invite code. */
export interface InviteRow {
  code: string;
  crew_id: string;
  created_by: string;
  expires_at: number;
  uses_left: number;
}

/** A registered clip. */
export interface ClipRow {
  clip_id: string;
  crew_id: string;
  owner: string;
  created_at: number;
  expires_at: number;
  deleted_at: number | null;
  /** Total announced upload bytes (per-clip quota accounting). */
  bytes: number;
}

/** Arguments of {@link Db.reserveBytes}. */
export interface ReserveBytesArgs {
  clipId: string;
  deviceId: string;
  /** UTC day (`YYYY-MM-DD`) the bytes are charged to. */
  day: string;
  /** Bytes to add; a non-negative integer. */
  delta: number;
  clipLimit: number;
  dayLimit: number;
  /**
   * Service-wide bytes allowed per UTC day, across all devices. When omitted the global counter
   * is neither checked nor updated.
   */
  globalDayLimit?: number;
}

/** Every database operation the Worker performs. */
export interface Db {
  getDevice(deviceId: string): Promise<DeviceRow | null>;
  /** Inserts a device; resolves `false` when the id already exists. */
  insertDeviceIfAbsent(row: DeviceRow): Promise<boolean>;

  /**
   * Creates a crew and its first member atomically, unless `createdBy` already created
   * `maxCrews` crews. Resolves `false` (and creates nothing) when that limit is reached.
   */
  createCrew(
    crewId: string,
    name: string,
    createdBy: string,
    nowMs: number,
    maxCrews: number,
  ): Promise<boolean>;
  isMember(crewId: string, deviceId: string): Promise<boolean>;
  listMembers(crewId: string): Promise<MemberRow[]>;
  addMember(crewId: string, deviceId: string, nowMs: number): Promise<void>;

  /** Atomically accepts an increasing sequence or replaces an expired announcement; null means stale. */
  updatePresence(deviceId: string, input: PresenceInput, nowMs: number, ttlMs: number): Promise<number | null>;
  /** Fresh members who are idle or active in this crew, sorted by device id. */
  listPresence(crewId: string, nowMs: number, ttlMs: number): Promise<PresenceRow[]>;
  /** Deletes announcements strictly older than the freshness cutoff. */
  purgePresence(olderThanMs: number): Promise<number>;

  /** Inserts an invite; resolves `false` when the code already exists. */
  insertInvite(row: InviteRow): Promise<boolean>;
  /** Number of invites of the crew that are unexpired and have uses left. */
  countActiveInvites(crewId: string, nowMs: number): Promise<number>;
  getInvite(code: string): Promise<InviteRow | null>;
  /**
   * Atomically spends one use of a valid (unexpired, not exhausted) invite.
   * Resolves the crew id, or `null` when the invite is unknown, expired or exhausted.
   */
  consumeInvite(code: string, nowMs: number): Promise<string | null>;

  getClip(clipId: string): Promise<ClipRow | null>;
  /** Inserts a clip; resolves `false` when the id already exists. */
  insertClip(row: Omit<ClipRow, "deleted_at" | "bytes">): Promise<boolean>;
  markClipDeleted(clipId: string, nowMs: number): Promise<void>;

  /** Announced sizes of the given chunk indices, keyed by index (missing = never announced). */
  getChunkSizes(
    clipId: string,
    pov: string,
    quality: Quality,
    indices: readonly number[],
  ): Promise<Map<number, number>>;
  /**
   * Atomically reserves `delta` bytes against the clip and the device's daily quota.
   * Nothing stays reserved unless the verdict is `"ok"`.
   */
  reserveBytes(args: ReserveBytesArgs): Promise<QuotaVerdict>;
  /** Remembers the announced size of chunks (the larger value wins on repeat). */
  recordChunks(
    clipId: string,
    pov: string,
    quality: Quality,
    entries: ReadonlyArray<readonly [index: number, size: number]>,
  ): Promise<void>;

  /**
   * Atomically adds `amount` to the counter `(scope, day)` unless that would exceed `limit`.
   * Resolves `false` (and changes nothing) when the limit would be exceeded.
   */
  reserveCounter(scope: string, day: string, amount: number, limit: number): Promise<boolean>;
  /** Gives back `amount` previously reserved with {@link Db.reserveCounter} (never below zero). */
  releaseCounter(scope: string, day: string, amount: number): Promise<void>;

  /** Records a request signature; resolves `false` if it was seen within `windowMs`. */
  recordSignature(sig: string, nowMs: number, windowMs: number): Promise<boolean>;

  /** Clips with `expires_at < beforeMs`, oldest first. */
  listExpiredClips(beforeMs: number, limit: number): Promise<ClipRow[]>;
  /** Deletes clip rows (and their chunk accounting); resolves how many clips were removed. */
  deleteClips(clipIds: readonly string[]): Promise<number>;
  purgeSignatures(olderThanMs: number): Promise<number>;
  /** Deletes invites that expired or ran out of uses. */
  purgeInvites(nowMs: number): Promise<number>;
  /** Deletes usage rows for days strictly before `beforeDay` (`YYYY-MM-DD`). */
  purgeUsage(beforeDay: string): Promise<number>;
  /** Deletes per-day counters for days strictly before `beforeDay` (`YYYY-MM-DD`). */
  purgeCounters(beforeDay: string): Promise<number>;
}

/** The subset of a D1 prepared statement used here (a real `D1PreparedStatement` fits). */
export interface D1StatementLike {
  bind(...values: unknown[]): D1StatementLike;
  first<T = unknown>(): Promise<T | null>;
  all<T = unknown>(): Promise<{ results: T[] }>;
  run(): Promise<{ meta: { changes?: number } }>;
}

/** The subset of a D1 database used here (a real `D1Database` fits). */
export interface D1Like {
  prepare(sql: string): D1StatementLike;
  batch(statements: D1StatementLike[]): Promise<Array<{ meta: { changes?: number } }>>;
}

/** Counter scope of the service-wide daily byte budget. */
export const GLOBAL_BYTES_SCOPE = "bytes";
/** Counter scope of the service-wide daily count of new devices. */
export const NEW_DEVICES_SCOPE = "devices";
/** Counter scope of the join attempts of one device. */
export const joinAttemptsScope = (deviceId: string): string => `join:${deviceId}`;
/** Counter scope of the clips registered by one device. */
export const clipRegistrationsScope = (deviceId: string): string => `clips:${deviceId}`;

const DEVICE_COLUMNS = "device_id, public_key_b64, display_name, created_at";
const CLIP_COLUMNS = "clip_id, crew_id, owner, created_at, expires_at, deleted_at, bytes";

/** Creates the D1-backed implementation of {@link Db}. */
export function createD1Db(d1: D1Like): Db {
  const run = async (sql: string, ...params: unknown[]): Promise<number> => {
    const result = await d1
      .prepare(sql)
      .bind(...params)
      .run();
    return result.meta.changes ?? 0;
  };
  const first = <T>(sql: string, ...params: unknown[]): Promise<T | null> =>
    d1
      .prepare(sql)
      .bind(...params)
      .first<T>();
  const all = async <T>(sql: string, ...params: unknown[]): Promise<T[]> =>
    (
      await d1
        .prepare(sql)
        .bind(...params)
        .all<T>()
    ).results;

  const reserveCounter = async (
    scope: string,
    day: string,
    amount: number,
    limit: number,
  ): Promise<boolean> => {
    if (amount > limit) {
      return false; // also keeps the first insert of a day below the limit
    }
    const changes = await run(
      `INSERT INTO counters (scope, day, n) VALUES (?1, ?2, ?3)
       ON CONFLICT (scope, day) DO UPDATE SET n = n + excluded.n
         WHERE n + excluded.n <= ?4`,
      scope,
      day,
      amount,
      limit,
    );
    return changes > 0;
  };

  return {
    getDevice: (deviceId) =>
      first<DeviceRow>(`SELECT ${DEVICE_COLUMNS} FROM devices WHERE device_id = ?1`, deviceId),

    async insertDeviceIfAbsent(row) {
      const changes = await run(
        `INSERT INTO devices (${DEVICE_COLUMNS}) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (device_id) DO NOTHING`,
        row.device_id,
        row.public_key_b64,
        row.display_name,
        row.created_at,
      );
      return changes > 0;
    },

    async createCrew(crewId, name, createdBy, nowMs, maxCrews) {
      // Both statements run in one transaction. The first one is conditional on the creator's
      // crew count, so the limit holds even for concurrent requests; the second only runs when
      // the crew row exists.
      const results = await d1.batch([
        d1
          .prepare(
            `INSERT INTO crews (crew_id, name, created_by, created_at)
             SELECT ?1, ?2, ?3, ?4
              WHERE (SELECT COUNT(*) FROM crews WHERE created_by = ?3) < ?5`,
          )
          .bind(crewId, name, createdBy, nowMs, maxCrews),
        d1
          .prepare(
            `INSERT INTO crew_members (crew_id, device_id, joined_at)
             SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM crews WHERE crew_id = ?1)`,
          )
          .bind(crewId, createdBy, nowMs),
      ]);
      return (results[0]?.meta.changes ?? 0) > 0;
    },

    async isMember(crewId, deviceId) {
      const row = await first<{ ok: number }>(
        "SELECT 1 AS ok FROM crew_members WHERE crew_id = ?1 AND device_id = ?2",
        crewId,
        deviceId,
      );
      return row !== null;
    },

    listMembers: (crewId) =>
      all<MemberRow>(
        `SELECT d.device_id AS device_id, d.display_name AS display_name
           FROM crew_members m JOIN devices d ON d.device_id = m.device_id
          WHERE m.crew_id = ?1
          ORDER BY m.joined_at, d.device_id`,
        crewId,
      ),

    async addMember(crewId, deviceId, nowMs) {
      await run(
        `INSERT INTO crew_members (crew_id, device_id, joined_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (crew_id, device_id) DO NOTHING`,
        crewId,
        deviceId,
        nowMs,
      );
    },

    async updatePresence(deviceId, input, nowMs, ttlMs) {
      const rows = await all<{ seen_at_ms: number }>(
        `INSERT INTO device_presence (device_id, game, active_crew, seq, online_since_ms, seen_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (device_id) DO UPDATE SET
           game = excluded.game, active_crew = excluded.active_crew, seq = excluded.seq,
           online_since_ms = excluded.online_since_ms,
           seen_at_ms = MAX(device_presence.seen_at_ms, excluded.seen_at_ms)
         WHERE device_presence.seen_at_ms < ?7 OR excluded.seq > device_presence.seq
         RETURNING seen_at_ms`,
        deviceId,
        input.game,
        input.active_crew,
        input.seq,
        input.online_since_ms,
        nowMs,
        nowMs - ttlMs,
      );
      return rows[0]?.seen_at_ms ?? null;
    },

    listPresence: (crewId, nowMs, ttlMs) =>
      all<PresenceRow>(
        `SELECT p.device_id, d.display_name, p.game, p.active_crew, p.seq,
                p.online_since_ms, p.seen_at_ms
           FROM crew_members m
           JOIN device_presence p ON p.device_id = m.device_id
           JOIN devices d ON d.device_id = p.device_id
          WHERE m.crew_id = ?1 AND p.seen_at_ms >= ?2
            AND (p.active_crew IS NULL OR p.active_crew = ?1)
          ORDER BY p.device_id`,
        crewId,
        nowMs - ttlMs,
      ),

    purgePresence: (olderThanMs) =>
      run("DELETE FROM device_presence WHERE seen_at_ms < ?1", olderThanMs),

    async insertInvite(row) {
      const changes = await run(
        `INSERT INTO invites (code, crew_id, created_by, expires_at, uses_left)
         VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (code) DO NOTHING`,
        row.code,
        row.crew_id,
        row.created_by,
        row.expires_at,
        row.uses_left,
      );
      return changes > 0;
    },

    async countActiveInvites(crewId, nowMs) {
      const row = await first<{ n: number }>(
        "SELECT COUNT(*) AS n FROM invites WHERE crew_id = ?1 AND expires_at > ?2 AND uses_left > 0",
        crewId,
        nowMs,
      );
      return row?.n ?? 0;
    },

    getInvite: (code) =>
      first<InviteRow>(
        "SELECT code, crew_id, created_by, expires_at, uses_left FROM invites WHERE code = ?1",
        code,
      ),

    async consumeInvite(code, nowMs) {
      const rows = await all<{ crew_id: string }>(
        `UPDATE invites SET uses_left = uses_left - 1
          WHERE code = ?1 AND uses_left > 0 AND expires_at > ?2
          RETURNING crew_id`,
        code,
        nowMs,
      );
      return rows[0]?.crew_id ?? null;
    },

    getClip: (clipId) =>
      first<ClipRow>(`SELECT ${CLIP_COLUMNS} FROM clips WHERE clip_id = ?1`, clipId),

    async insertClip(row) {
      const changes = await run(
        `INSERT INTO clips (clip_id, crew_id, owner, created_at, expires_at, deleted_at, bytes)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL, 0) ON CONFLICT (clip_id) DO NOTHING`,
        row.clip_id,
        row.crew_id,
        row.owner,
        row.created_at,
        row.expires_at,
      );
      return changes > 0;
    },

    async markClipDeleted(clipId, nowMs) {
      await run(
        "UPDATE clips SET deleted_at = COALESCE(deleted_at, ?2) WHERE clip_id = ?1",
        clipId,
        nowMs,
      );
    },

    async getChunkSizes(clipId, pov, quality, indices) {
      const rows = await all<{ idx: number; bytes: number }>(
        `SELECT idx, bytes FROM clip_chunks
          WHERE clip_id = ?1 AND pov = ?2 AND quality = ?3
            AND idx IN (SELECT value FROM json_each(?4))`,
        clipId,
        pov,
        quality,
        JSON.stringify(indices),
      );
      return new Map(rows.map((row) => [row.idx, row.bytes] as const));
    },

    async reserveBytes({ clipId, deviceId, day, delta, clipLimit, dayLimit, globalDayLimit }) {
      if (delta <= 0) {
        return "ok";
      }
      if (delta > clipLimit) {
        return "clip_limit";
      }
      if (delta > dayLimit) {
        return "day_limit";
      }
      const clipChanges = await run(
        "UPDATE clips SET bytes = bytes + ?1 WHERE clip_id = ?2 AND bytes + ?1 <= ?3",
        delta,
        clipId,
        clipLimit,
      );
      if (clipChanges === 0) {
        return "clip_limit";
      }
      // The first write of a day cannot overflow because delta <= dayLimit was checked above.
      const dayChanges = await run(
        `INSERT INTO usage (device_id, day, bytes) VALUES (?1, ?2, ?3)
         ON CONFLICT (device_id, day) DO UPDATE SET bytes = bytes + excluded.bytes
           WHERE bytes + excluded.bytes <= ?4`,
        deviceId,
        day,
        delta,
        dayLimit,
      );
      if (dayChanges === 0) {
        // Give the clip reservation back so a refused request leaves no trace.
        await run("UPDATE clips SET bytes = MAX(0, bytes - ?1) WHERE clip_id = ?2", delta, clipId);
        return "day_limit";
      }
      const globalOk =
        globalDayLimit === undefined ||
        (await reserveCounter(GLOBAL_BYTES_SCOPE, day, delta, globalDayLimit));
      if (!globalOk) {
        // Undo both earlier reservations so a refused request leaves no trace.
        await run("UPDATE clips SET bytes = MAX(0, bytes - ?1) WHERE clip_id = ?2", delta, clipId);
        await run(
          "UPDATE usage SET bytes = MAX(0, bytes - ?1) WHERE device_id = ?2 AND day = ?3",
          delta,
          deviceId,
          day,
        );
        return "global_limit";
      }
      return "ok";
    },

    reserveCounter,

    async releaseCounter(scope, day, amount) {
      await run(
        "UPDATE counters SET n = MAX(0, n - ?3) WHERE scope = ?1 AND day = ?2",
        scope,
        day,
        amount,
      );
    },

    async recordChunks(clipId, pov, quality, entries) {
      if (entries.length === 0) {
        return;
      }
      await run(
        `INSERT INTO clip_chunks (clip_id, pov, quality, idx, bytes)
         SELECT ?1, ?2, ?3, json_extract(value, '$[0]'), json_extract(value, '$[1]')
           FROM json_each(?4) WHERE true
         ON CONFLICT (clip_id, pov, quality, idx) DO UPDATE SET bytes = MAX(bytes, excluded.bytes)`,
        clipId,
        pov,
        quality,
        JSON.stringify(entries),
      );
    },

    async recordSignature(sig, nowMs, windowMs) {
      const changes = await run(
        `INSERT INTO seen_signatures (sig, seen_at) VALUES (?1, ?2)
         ON CONFLICT (sig) DO UPDATE SET seen_at = excluded.seen_at
           WHERE seen_signatures.seen_at < ?3`,
        sig,
        nowMs,
        nowMs - windowMs,
      );
      return changes > 0;
    },

    listExpiredClips: (beforeMs, limit) =>
      all<ClipRow>(
        `SELECT ${CLIP_COLUMNS} FROM clips WHERE expires_at < ?1 ORDER BY expires_at LIMIT ?2`,
        beforeMs,
        limit,
      ),

    async deleteClips(clipIds) {
      if (clipIds.length === 0) {
        return 0;
      }
      const ids = JSON.stringify(clipIds);
      const results = await d1.batch([
        d1
          .prepare("DELETE FROM clip_chunks WHERE clip_id IN (SELECT value FROM json_each(?1))")
          .bind(ids),
        d1
          .prepare("DELETE FROM clips WHERE clip_id IN (SELECT value FROM json_each(?1))")
          .bind(ids),
      ]);
      return results[1]?.meta.changes ?? 0;
    },

    purgeSignatures: (olderThanMs) =>
      run("DELETE FROM seen_signatures WHERE seen_at < ?1", olderThanMs),

    purgeInvites: (nowMs) =>
      run("DELETE FROM invites WHERE expires_at < ?1 OR uses_left <= 0", nowMs),

    purgeUsage: (beforeDay) => run("DELETE FROM usage WHERE day < ?1", beforeDay),

    purgeCounters: (beforeDay) => run("DELETE FROM counters WHERE day < ?1", beforeDay),
  };
}
