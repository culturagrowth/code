/**
 * Device request signing and verification.
 *
 * Every route except `POST /v1/devices` and `GET /v1/health` carries three headers:
 *
 * - `X-DC-Device`: the device id (lowercase uuid);
 * - `X-DC-Timestamp`: unix milliseconds, a decimal integer;
 * - `X-DC-Signature`: standard base64 of the 64-byte Ed25519 signature over the canonical string
 *
 *   `"DC1\n" + METHOD + "\n" + PATH_WITH_QUERY + "\n" + TIMESTAMP + "\n" + hex(sha256(body))`
 *
 * Requests are rejected when the timestamp is more than 5 minutes away from the Worker's clock,
 * and a signature seen before inside a 10 minute window is rejected as a replay.
 */

import { base64Decode, sha256Hex, utf8 } from "./encoding.js";
import { HttpError } from "./http.js";
import { isUuid } from "./keys.js";
import type { DeviceRow } from "./db.js";

/** First line of the canonical string; bumps if the signing scheme ever changes. */
export const SIGNATURE_VERSION = "DC1";

/** Largest accepted difference between the request timestamp and the Worker clock. */
export const MAX_SKEW_MS = 5 * 60 * 1000;

/** How long a signature is remembered to reject replays. */
export const REPLAY_WINDOW_MS = 10 * 60 * 1000;

/** Header carrying the device id. */
export const HEADER_DEVICE = "x-dc-device";
/** Header carrying the request timestamp (unix ms). */
export const HEADER_TIMESTAMP = "x-dc-timestamp";
/** Header carrying the base64 Ed25519 signature. */
export const HEADER_SIGNATURE = "x-dc-signature";

const TIMESTAMP_RE = /^[0-9]{1,16}$/;
const ED25519_SIGNATURE_BYTES = 64;
const ED25519_PUBLIC_KEY_BYTES = 32;
/** Order of the Ed25519 base point group, `2^252 + 27742317777372353535851937790883648493`. */
const ED25519_GROUP_ORDER = (1n << 252n) + 27742317777372353535851937790883648493n;

/**
 * True when the scalar half `S` of a 64-byte Ed25519 signature is below the group order
 * (RFC 8032 section 5.1.7).
 *
 * `(R, S + L)` verifies like `(R, S)` on a lax verifier, which would give a captured request a
 * second valid signature string and slip past the replay table (it keys on the signature text).
 * Workers' WebCrypto rejects such signatures, but the guarantee is cheap to enforce here so it
 * does not depend on the runtime.
 */
export function hasCanonicalScalar(signature: Uint8Array): boolean {
  if (signature.length !== ED25519_SIGNATURE_BYTES) {
    return false;
  }
  let scalar = 0n;
  for (let i = ED25519_SIGNATURE_BYTES - 1; i >= 32; i -= 1) {
    scalar = (scalar << 8n) | BigInt(signature[i] ?? 0);
  }
  return scalar < ED25519_GROUP_ORDER;
}

/** The slice of the database the authenticator needs. */
export interface AuthStore {
  getDevice(deviceId: string): Promise<DeviceRow | null>;
  /** Records a signature; resolves `false` when it was already seen inside the window. */
  recordSignature(sig: string, nowMs: number, windowMs: number): Promise<boolean>;
}

/** Dependencies of {@link authenticate}. */
export interface AuthDeps {
  db: AuthStore;
  /** Current unix time in milliseconds. */
  now(): number;
}

/**
 * Builds the exact string a device signs.
 *
 * @param method HTTP method (upper-cased here).
 * @param pathWithQuery Request path including the `?query` part, exactly as sent.
 * @param timestamp The value of `X-DC-Timestamp`.
 * @param bodySha256Hex Lowercase hex SHA-256 of the raw request body (of the empty string if none).
 */
export function canonicalString(
  method: string,
  pathWithQuery: string,
  timestamp: string | number,
  bodySha256Hex: string,
): string {
  return `${SIGNATURE_VERSION}\n${method.toUpperCase()}\n${pathWithQuery}\n${timestamp}\n${bodySha256Hex}`;
}

/** Imports a raw 32-byte Ed25519 public key for verification. Throws if the key is unusable. */
export async function importPublicKey(raw: Uint8Array): Promise<CryptoKey> {
  if (raw.length !== ED25519_PUBLIC_KEY_BYTES) {
    throw new RangeError("Ed25519 public keys are 32 bytes");
  }
  return crypto.subtle.importKey("raw", raw, { name: "Ed25519" }, false, ["verify"]);
}

/**
 * Verifies `signature` over `message` with the stored public key.
 * Returns `false` (never throws) for malformed keys or signatures.
 */
export async function verifySignature(
  publicKeyB64: string,
  signature: Uint8Array,
  message: string,
): Promise<boolean> {
  const raw = base64Decode(publicKeyB64);
  if (raw === null || !hasCanonicalScalar(signature)) {
    return false;
  }
  try {
    const key = await importPublicKey(raw);
    return await crypto.subtle.verify("Ed25519", key, signature, utf8(message));
  } catch {
    return false;
  }
}

function unauthorized(reason: string, extra: Record<string, unknown> = {}): HttpError {
  return new HttpError(401, "unauthorized", "request authentication failed", { reason, ...extra });
}

/**
 * Authenticates a signed request and returns the calling device id.
 *
 * @param deps Database access and clock.
 * @param request The incoming request (only headers and URL are read).
 * @param body The raw request body already read from `request`.
 * @throws {HttpError} 401 for any authentication failure.
 */
export async function authenticate(
  deps: AuthDeps,
  request: Request,
  body: Uint8Array,
): Promise<string> {
  const deviceId = request.headers.get(HEADER_DEVICE);
  const timestamp = request.headers.get(HEADER_TIMESTAMP);
  const signatureB64 = request.headers.get(HEADER_SIGNATURE);
  if (deviceId === null || timestamp === null || signatureB64 === null) {
    throw unauthorized("missing_headers");
  }
  if (!isUuid(deviceId) || !TIMESTAMP_RE.test(timestamp)) {
    throw unauthorized("bad_headers");
  }
  const signature = base64Decode(signatureB64);
  if (signature === null || signature.length !== ED25519_SIGNATURE_BYTES) {
    throw unauthorized("bad_headers");
  }

  const now = deps.now();
  if (Math.abs(now - Number(timestamp)) > MAX_SKEW_MS) {
    throw unauthorized("clock_skew", { server_time_ms: now });
  }

  const device = await deps.db.getDevice(deviceId);
  if (device === null) {
    throw unauthorized("unknown_device");
  }

  const url = new URL(request.url);
  const canonical = canonicalString(
    request.method,
    url.pathname + url.search,
    timestamp,
    await sha256Hex(body),
  );
  if (!(await verifySignature(device.public_key_b64, signature, canonical))) {
    throw unauthorized("bad_signature");
  }

  // Only verified signatures are remembered, so garbage cannot fill the table.
  if (!(await deps.db.recordSignature(signatureB64, now, REPLAY_WINDOW_MS))) {
    throw unauthorized("replay");
  }
  return deviceId;
}
