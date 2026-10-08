/**
 * Quota constants and the pure math behind them.
 *
 * Quotas are enforced on the sizes announced by the uploader when it asks for presigned URLs.
 * The authoritative (atomic) accounting happens in SQL, see `Db.reserveBytes`; the helpers here
 * give early, cheap answers and make the rules testable in isolation.
 */

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;

/** Most chunk indices accepted in one upload-urls / download-urls request. */
export const MAX_INDICES_PER_REQUEST = 64;

/** Largest single chunk (ciphertext), in bytes. */
export const MAX_CHUNK_BYTES = 64 * MIB;

/** Total bytes one clip may hold across every POV and quality (1.5 GiB). */
export const MAX_CLIP_BYTES = 1.5 * GIB;

/** Bytes one device may announce per UTC day (10 GiB). */
export const MAX_DAILY_BYTES = 10 * GIB;

/** Longest clip lifetime, in seconds (72 h). Also the default. */
export const MAX_TTL_S = 72 * 60 * 60;

/** Largest manifest object (ciphertext), in bytes (1 MiB). */
export const MAX_MANIFEST_BYTES = MIB;

/**
 * Every object is charged at least this many bytes against the quotas, whatever its real size.
 *
 * Without a floor a hostile member could create millions of 1-byte objects: they would cost
 * almost no quota but a lot of R2 operations and sweep work. With the floor a clip holds at most
 * `MAX_CLIP_BYTES / MIN_CHARGED_BYTES` = 6144 objects. Real chunks are 4-8 MiB, so honest
 * clients never notice.
 */
export const MIN_CHARGED_BYTES = 256 * 1024;

/**
 * Pseudo chunk index under which the manifest is tracked in `clip_chunks`.
 * Real chunk indices are never negative, so it cannot collide with one.
 */
export const MANIFEST_INDEX = -1;

/** Outcome of a quota evaluation. */
export type QuotaVerdict = "ok" | "clip_limit" | "day_limit" | "global_limit";

/**
 * Abuse limits. `POST /v1/devices` is open by design, so besides the per-device quotas the
 * service has a few global circuit breakers. Two of them can be tuned with Worker variables
 * (see {@link limitsFromEnv}).
 */
export interface Limits {
  /** New devices that may register per UTC day, service wide. `0` closes registration. */
  newDevicesPerDay: number;
  /** Bytes (announced) all devices together may upload per UTC day. */
  globalDailyBytes: number;
  /** Crews one device may create. */
  maxCrewsPerDevice: number;
  /** Invites of one crew that may be valid at the same time. */
  maxActiveInvitesPerCrew: number;
  /** Join attempts per device per UTC day; an attempt that succeeds is not counted. */
  maxJoinAttemptsPerDay: number;
  /** New clips one device may register per UTC day. */
  maxClipsPerDevicePerDay: number;
}

/** Limits used when nothing is configured. */
export const DEFAULT_LIMITS: Readonly<Limits> = {
  newDevicesPerDay: 50,
  globalDailyBytes: 200 * GIB,
  maxCrewsPerDevice: 20,
  maxActiveInvitesPerCrew: 10,
  maxJoinAttemptsPerDay: 20,
  maxClipsPerDevicePerDay: 200,
};

/** The optional Worker variables that tune {@link Limits}. */
export interface LimitsEnv {
  MAX_NEW_DEVICES_PER_DAY?: unknown;
  MAX_GLOBAL_DAILY_BYTES?: unknown;
}

function nonNegativeInteger(value: unknown, fallback: number): number {
  const text = typeof value === "number" ? String(value) : value;
  if (typeof text !== "string" || !/^[0-9]{1,15}$/.test(text.trim())) {
    return fallback;
  }
  return Number(text.trim());
}

/**
 * Builds {@link Limits} from the Worker variables. Missing, empty or malformed values (anything
 * but a non-negative integer) fall back to the defaults, so a typo can never silently disable a
 * limit.
 */
export function limitsFromEnv(env: LimitsEnv): Limits {
  return {
    ...DEFAULT_LIMITS,
    newDevicesPerDay: nonNegativeInteger(env.MAX_NEW_DEVICES_PER_DAY, DEFAULT_LIMITS.newDevicesPerDay),
    globalDailyBytes: nonNegativeInteger(env.MAX_GLOBAL_DAILY_BYTES, DEFAULT_LIMITS.globalDailyBytes),
  };
}

/** Bytes charged for an object of `size` bytes: the size, but at least {@link MIN_CHARGED_BYTES}. */
export function chargedBytes(size: number): number {
  return Math.max(size, MIN_CHARGED_BYTES);
}

/** UTC calendar day of a unix-millisecond timestamp, formatted `YYYY-MM-DD`. */
export function utcDay(unixMs: number): string {
  return new Date(unixMs).toISOString().slice(0, 10);
}

/** Whole seconds from `unixMs` until the next UTC midnight, when daily counters reset (at least 1). */
export function secondsUntilNextUtcDay(unixMs: number): number {
  const dayMs = 24 * 60 * 60 * 1000;
  return Math.max(1, Math.ceil((dayMs - (unixMs % dayMs)) / 1000));
}

/**
 * Extra bytes a request adds, given what was already charged for the same chunks.
 *
 * Each chunk is charged `max(size, MIN_CHARGED_BYTES)`. Asking again for a URL for a chunk with
 * the same (or a smaller) size costs nothing, so a retry of a failed upload never eats quota.
 * Growing a chunk only costs the difference.
 *
 * @param existing Previously charged bytes per chunk index (see {@link chargedBytes}).
 * @param indices Chunk indices of the request (the manifest uses {@link MANIFEST_INDEX}).
 * @param sizes Announced size for each index (same length as `indices`).
 */
export function additionalBytes(
  existing: ReadonlyMap<number, number>,
  indices: readonly number[],
  sizes: readonly number[],
): number {
  let delta = 0;
  for (let i = 0; i < indices.length; i += 1) {
    const index = indices[i];
    const size = sizes[i];
    if (index === undefined || size === undefined) {
      continue;
    }
    delta += Math.max(0, chargedBytes(size) - (existing.get(index) ?? 0));
  }
  return delta;
}

/**
 * Evaluates both quotas for a request that adds `delta` bytes.
 *
 * @param clipBytes Bytes already announced for the clip.
 * @param dayBytes Bytes already announced today by the calling device.
 */
export function evaluateQuota(clipBytes: number, dayBytes: number, delta: number): QuotaVerdict {
  if (clipBytes + delta > MAX_CLIP_BYTES) {
    return "clip_limit";
  }
  if (dayBytes + delta > MAX_DAILY_BYTES) {
    return "day_limit";
  }
  return "ok";
}
