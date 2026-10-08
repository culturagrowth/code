/**
 * Hand-written request body validators.
 *
 * Every function takes an already parsed JSON object and either returns a fully typed value or
 * throws an {@link HttpError} (400, 413 for oversized chunks). Unknown fields are ignored so
 * newer clients stay compatible; every known field is checked strictly.
 */

import { base64Decode } from "./encoding.js";
import { HttpError } from "./http.js";
import { INVITE_CODE_LENGTH } from "./invite.js";
import { isQuality, isUuid, MAX_CHUNK_INDEX } from "./keys.js";
import type { Quality } from "./keys.js";
import {
  MAX_CHUNK_BYTES,
  MAX_INDICES_PER_REQUEST,
  MAX_MANIFEST_BYTES,
  MAX_TTL_S,
} from "./quota.js";

/** Longest device display name, in Unicode code points. */
export const MAX_DISPLAY_NAME_CHARS = 32;

/** Longest crew name, in Unicode code points. */
export const MAX_CREW_NAME_CHARS = 48;

const INVITE_CODE_RE = /^[A-Z2-9]{10}$/;
const FORBIDDEN_TEXT_RE = /[\p{Cc}\p{Cs}‪-‮⁦-⁩]/u;

type JsonObject = Record<string, unknown>;

function invalid(message: string): HttpError {
  return new HttpError(400, "invalid_request", message);
}

function requireUuid(body: JsonObject, field: string): string {
  const value = body[field];
  if (!isUuid(value)) {
    throw invalid(`${field} must be a lowercase hyphenated uuid`);
  }
  return value;
}

function requireQuality(body: JsonObject): Quality {
  const value = body["quality"];
  if (!isQuality(value)) {
    throw invalid("quality must be 'proxy' or 'full'");
  }
  return value;
}

function optionalBoolean(body: JsonObject, field: string, fallback: boolean): boolean {
  const value = body[field];
  if (value === undefined) {
    return fallback;
  }
  if (typeof value !== "boolean") {
    throw invalid(`${field} must be a boolean`);
  }
  return value;
}

/** Validates a human-facing name: trimmed length of 1..`maxChars` code points, no control characters. */
function requireName(body: JsonObject, field: string, maxChars: number): string {
  const value = body[field];
  if (typeof value !== "string") {
    throw invalid(`${field} must be a string`);
  }
  const name = value.trim();
  const length = [...name].length;
  if (length < 1 || length > maxChars) {
    throw invalid(`${field} must have 1 to ${maxChars} characters`);
  }
  if (FORBIDDEN_TEXT_RE.test(name)) {
    throw invalid(`${field} contains forbidden characters`);
  }
  return name;
}

function requireIntegerArray(
  body: JsonObject,
  field: string,
  min: number,
  max: number,
  tooLarge?: () => HttpError,
): number[] {
  const value = body[field];
  if (!Array.isArray(value)) {
    throw invalid(`${field} must be an array`);
  }
  if (value.length > MAX_INDICES_PER_REQUEST) {
    throw new HttpError(
      400,
      "too_many_indices",
      `${field} has more than ${MAX_INDICES_PER_REQUEST} entries`,
    );
  }
  const out: number[] = [];
  for (const entry of value as unknown[]) {
    if (typeof entry !== "number" || !Number.isSafeInteger(entry) || entry < min) {
      throw invalid(`${field} entries must be integers >= ${min}`);
    }
    if (entry > max) {
      throw tooLarge ? tooLarge() : invalid(`${field} entries must be <= ${max}`);
    }
    out.push(entry);
  }
  return out;
}

function requireDistinctIndices(body: JsonObject): number[] {
  const indices = requireIntegerArray(body, "indices", 0, MAX_CHUNK_INDEX);
  if (new Set(indices).size !== indices.length) {
    throw invalid("indices must not contain duplicates");
  }
  return indices;
}

/** Body of `POST /v1/devices`. */
export interface DeviceRegistration {
  device_id: string;
  /** Standard base64 of the 32 raw Ed25519 public key bytes. */
  public_key_b64: string;
  display_name: string;
}

/** Validates the body of `POST /v1/devices`. */
export function parseDeviceRegistration(body: JsonObject): DeviceRegistration {
  const deviceId = requireUuid(body, "device_id");
  const key = body["public_key_b64"];
  if (typeof key !== "string") {
    throw invalid("public_key_b64 must be a string");
  }
  const raw = base64Decode(key);
  if (raw === null || raw.length !== 32) {
    throw invalid("public_key_b64 must be the standard base64 of exactly 32 bytes");
  }
  return {
    device_id: deviceId,
    public_key_b64: key,
    display_name: requireName(body, "display_name", MAX_DISPLAY_NAME_CHARS),
  };
}

/** Validates the body of `POST /v1/crews`; returns the crew name. */
export function parseCreateCrew(body: JsonObject): string {
  return requireName(body, "name", MAX_CREW_NAME_CHARS);
}

/** A session announcement; identity and receipt time are supplied by the Worker. */
export interface PresenceInput {
  game: string | null;
  active_crew: string | null;
  seq: number;
  online_since_ms: number;
}

/** Validates `POST /v1/presence`, preserving game ids exactly for equality matching. */
export function parsePresence(body: JsonObject): PresenceInput {
  const game = body["game"];
  if (game !== null && (
    typeof game !== "string" || game.trim().length === 0 ||
    new TextEncoder().encode(game).byteLength > 64 || FORBIDDEN_TEXT_RE.test(game)
  )) {
    throw invalid("game must be null or a nonempty game id of at most 64 UTF-8 bytes without forbidden characters");
  }
  const activeCrew = body["active_crew"];
  if (activeCrew !== null && !isUuid(activeCrew)) {
    throw invalid("active_crew must be null or a lowercase hyphenated uuid");
  }
  const nonnegativeInteger = (field: string): number => {
    const value = body[field];
    if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
      throw invalid(`${field} must be a nonnegative safe integer`);
    }
    return value;
  };
  return {
    game,
    active_crew: activeCrew,
    seq: nonnegativeInteger("seq"),
    online_since_ms: nonnegativeInteger("online_since_ms"),
  };
}

/** Validates the body of `POST /v1/crews/join`; returns the normalized (upper case) code. */
export function parseJoin(body: JsonObject): string {
  const value = body["code"];
  if (typeof value !== "string") {
    throw invalid("code must be a string");
  }
  // ASCII-only case folding: `String.prototype.toUpperCase` also maps "ı" (dotless i) to "I" and
  // "ſ" (long s) to "S", which would make several different strings spell the same code.
  const code = value.trim().replace(/[a-z]/g, (letter) => letter.toUpperCase());
  if (!INVITE_CODE_RE.test(code)) {
    throw invalid(`code must be ${INVITE_CODE_LENGTH} characters of A-Z and 2-9`);
  }
  return code;
}

/** Body of `POST /v1/clips`. */
export interface ClipRegistration {
  clip_id: string;
  crew_id: string;
  ttl_s: number;
}

/** Validates the body of `POST /v1/clips`. `ttl_s` defaults to (and is capped at) 72 h. */
export function parseClipRegistration(body: JsonObject): ClipRegistration {
  const ttl = body["ttl_s"];
  let ttlS = MAX_TTL_S;
  if (ttl !== undefined) {
    if (typeof ttl !== "number" || !Number.isSafeInteger(ttl) || ttl < 1 || ttl > MAX_TTL_S) {
      throw invalid(`ttl_s must be an integer between 1 and ${MAX_TTL_S}`);
    }
    ttlS = ttl;
  }
  return {
    clip_id: requireUuid(body, "clip_id"),
    crew_id: requireUuid(body, "crew_id"),
    ttl_s: ttlS,
  };
}

/** Body of `POST /v1/clips/:clip/upload-urls`. */
export interface UploadRequest {
  pov: string;
  quality: Quality;
  indices: number[];
  /** Announced ciphertext size of each chunk, parallel to `indices`. */
  sizes: number[];
  /** Also return a URL for the manifest object. */
  manifest: boolean;
  /** Exact ciphertext size of the manifest; present exactly when `manifest` is true. */
  manifest_size: number | null;
}

/** Validates the body of `POST /v1/clips/:clip/upload-urls`. */
export function parseUploadRequest(body: JsonObject): UploadRequest {
  const pov = requireUuid(body, "pov");
  const quality = requireQuality(body);
  const indices = requireDistinctIndices(body);
  const sizes = requireIntegerArray(
    body,
    "sizes",
    1,
    MAX_CHUNK_BYTES,
    () => new HttpError(413, "chunk_too_large", `each chunk must be at most ${MAX_CHUNK_BYTES} bytes`),
  );
  if (sizes.length !== indices.length) {
    throw invalid("sizes must have the same length as indices");
  }
  const manifest = optionalBoolean(body, "manifest", false);
  if (indices.length === 0 && !manifest) {
    throw invalid("request at least one chunk index or the manifest");
  }
  return { pov, quality, indices, sizes, manifest, manifest_size: manifestSize(body, manifest) };
}

/**
 * The manifest is uploaded through a URL whose `Content-Length` is signed, so its exact size must
 * be announced (and is charged to the quotas like a chunk). Ignored when no manifest is requested.
 */
function manifestSize(body: JsonObject, manifest: boolean): number | null {
  if (!manifest) {
    return null;
  }
  const size = body["manifest_size"];
  if (size === undefined) {
    throw invalid("manifest_size is required when manifest is true");
  }
  if (typeof size !== "number" || !Number.isSafeInteger(size) || size < 1) {
    throw invalid("manifest_size must be an integer >= 1");
  }
  if (size > MAX_MANIFEST_BYTES) {
    throw new HttpError(
      413,
      "manifest_too_large",
      `the manifest must be at most ${MAX_MANIFEST_BYTES} bytes`,
    );
  }
  return size;
}

/** Body of `POST /v1/clips/:clip/download-urls`. */
export interface DownloadRequest {
  pov: string;
  quality: Quality;
  indices: number[];
  manifest: boolean;
}

/** Validates the body of `POST /v1/clips/:clip/download-urls`. */
export function parseDownloadRequest(body: JsonObject): DownloadRequest {
  const pov = requireUuid(body, "pov");
  const quality = requireQuality(body);
  const indices = requireDistinctIndices(body);
  const manifest = optionalBoolean(body, "manifest", false);
  if (indices.length === 0 && !manifest) {
    throw invalid("request at least one chunk index or the manifest");
  }
  return { pov, quality, indices, manifest };
}
