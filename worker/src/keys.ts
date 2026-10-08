/**
 * Object key naming and validation.
 *
 * The layout MUST stay byte-for-byte identical to `object_key`, `manifest_key` and `clip_prefix`
 * in `crates/duoclip-proto` (Rust):
 *
 * - `clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin`
 * - `clips/{crew}/{clip}/{pov}/{proxy|full}/manifest.bin`
 * - `clips/{crew}/{clip}/`
 *
 * UUIDs are lowercase and hyphenated. The builders throw on malformed input as a defense in
 * depth; request handlers validate every identifier before calling them.
 */

/** Every object key starts with this prefix. */
export const OBJECT_KEY_PREFIX = "clips/";

/** Longest accepted object key, in bytes (same limit as the Rust crate). */
export const MAX_OBJECT_KEY_BYTES = 512;

/** Largest chunk index the Worker hands out (six digits, as in the `{index:06}` format). */
export const MAX_CHUNK_INDEX = 999_999;

/** Quality tier of an uploaded POV. */
export type Quality = "proxy" | "full";

/** All valid qualities, in wire spelling. */
export const QUALITIES: readonly Quality[] = ["proxy", "full"];

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const KEY_CHARS_RE = /^[A-Za-z0-9/_.-]*$/;

/** True for a lowercase, hyphenated 8-4-4-4-12 UUID string (any version). */
export function isUuid(value: unknown): value is string {
  return typeof value === "string" && UUID_RE.test(value);
}

/** True when `value` is `"proxy"` or `"full"`. */
export function isQuality(value: unknown): value is Quality {
  return value === "proxy" || value === "full";
}

function assertUuid(name: string, value: string): void {
  if (!isUuid(value)) {
    throw new RangeError(`${name} must be a lowercase hyphenated uuid`);
  }
}

function assertQuality(value: string): void {
  if (!isQuality(value)) {
    throw new RangeError("quality must be 'proxy' or 'full'");
  }
}

/** Prefix shared by every object of a clip: `clips/{crew}/{clip}/`. */
export function clipPrefix(crew: string, clip: string): string {
  assertUuid("crew", crew);
  assertUuid("clip", clip);
  return `${OBJECT_KEY_PREFIX}${crew}/${clip}/`;
}

/** Key of one encrypted chunk: `clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin`. */
export function objectKey(
  crew: string,
  clip: string,
  pov: string,
  quality: Quality,
  index: number,
): string {
  assertUuid("pov", pov);
  assertQuality(quality);
  if (!Number.isInteger(index) || index < 0 || index > 0xffff_ffff) {
    throw new RangeError("index must be an integer in 0..=4294967295");
  }
  return `${clipPrefix(crew, clip)}${pov}/${quality}/${String(index).padStart(6, "0")}.bin`;
}

/** Key of the encrypted manifest of one `(clip, pov, quality)`. */
export function manifestKey(crew: string, clip: string, pov: string, quality: Quality): string {
  assertUuid("pov", pov);
  assertQuality(quality);
  return `${clipPrefix(crew, clip)}${pov}/${quality}/manifest.bin`;
}

/**
 * Returns `null` when `key` obeys the structural rules shared with the Rust crate (starts with
 * `clips/`, at most 512 bytes, only `[A-Za-z0-9/_.-]`, no `..`, no `//`), otherwise a reason.
 */
export function validateObjectKey(key: string): string | null {
  if (new TextEncoder().encode(key).length > MAX_OBJECT_KEY_BYTES) {
    return "object key longer than 512 bytes";
  }
  if (!key.startsWith(OBJECT_KEY_PREFIX)) {
    return 'object key must start with "clips/"';
  }
  if (!KEY_CHARS_RE.test(key)) {
    return "object key contains a forbidden character";
  }
  if (key.includes("..")) {
    return 'object key contains ".."';
  }
  if (key.includes("//")) {
    return 'object key contains "//"';
  }
  return null;
}
