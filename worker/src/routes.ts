/**
 * Router and route handlers.
 *
 * `handleRequest` is the single entry point: it matches the route, reads the (size-limited) body,
 * authenticates the device, then runs the handler. Handlers throw {@link HttpError} for every
 * expected failure; anything else becomes an opaque 500.
 */

import type { App } from "./app.js";
import { authenticate, importPublicKey } from "./auth.js";
import {
  clipRegistrationsScope,
  joinAttemptsScope,
  NEW_DEVICES_SCOPE,
} from "./db.js";
import type { ClipRow } from "./db.js";
import { base64Decode, timingSafeEqual } from "./encoding.js";
import {
  errorResponse,
  HttpError,
  jsonResponse,
  MAX_BODY_BYTES,
  parseJsonObject,
  readBody,
} from "./http.js";
import { generateInviteCode, INVITE_TTL_MS, INVITE_USES } from "./invite.js";
import { isUuid, manifestKey, objectKey } from "./keys.js";
import type { Quality } from "./keys.js";
import { PRESIGN_EXPIRES_S, UPLOAD_CONTENT_TYPE } from "./presign.js";
import { PRESENCE_HEARTBEAT_MS, PRESENCE_TTL_MS } from "./presence.js";
import type { PresignMethod } from "./presign.js";
import {
  additionalBytes,
  chargedBytes,
  evaluateQuota,
  MANIFEST_INDEX,
  MAX_CLIP_BYTES,
  MAX_DAILY_BYTES,
  secondsUntilNextUtcDay,
  utcDay,
} from "./quota.js";
import { deleteClipObjects } from "./store.js";
import {
  parseClipRegistration,
  parseCreateCrew,
  parseDeviceRegistration,
  parseDownloadRequest,
  parseJoin,
  parsePresence,
  parseUploadRequest,
} from "./validate.js";

/** Everything a handler receives. */
interface RouteContext {
  app: App;
  /** Raw (already size-checked) request body. */
  body: Uint8Array;
  /** Authenticated device id; empty string on unauthenticated routes. */
  deviceId: string;
  /** Values of the `:param` segments, in order. */
  params: string[];
}

type Handler = (ctx: RouteContext) => Promise<Response>;

interface Route {
  method: string;
  /** Path segments; a leading `:` marks a parameter. */
  segments: readonly string[];
  /** Whether the request must carry a valid device signature. */
  auth: boolean;
  handler: Handler;
}

const INVITE_CREATE_ATTEMPTS = 5;

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

function notAMember(): HttpError {
  return new HttpError(403, "not_a_member", "the device is not a member of this crew");
}

function requireUuidParam(value: string | undefined, name: string): string {
  if (!isUuid(value)) {
    throw new HttpError(400, "invalid_request", `${name} must be a lowercase hyphenated uuid`);
  }
  return value;
}

async function requireMember(app: App, crewId: string, deviceId: string): Promise<void> {
  if (!(await app.db.isMember(crewId, deviceId))) {
    throw notAMember();
  }
}

/** Loads a clip and checks that the caller belongs to its crew. */
async function loadClipForMember(
  app: App,
  clipParam: string | undefined,
  deviceId: string,
): Promise<ClipRow> {
  const clipId = requireUuidParam(clipParam, "clip id");
  const clip = await app.db.getClip(clipId);
  if (clip === null) {
    throw new HttpError(404, "clip_not_found", "unknown clip");
  }
  if (clip.owner !== deviceId) {
    await requireMember(app, clip.crew_id, deviceId);
  }
  return clip;
}

/** Throws 410 when the clip was deleted or has expired. */
function requireLiveClip(clip: ClipRow, nowMs: number): void {
  if (clip.deleted_at !== null) {
    throw new HttpError(410, "clip_gone", "the clip was deleted");
  }
  if (clip.expires_at <= nowMs) {
    throw new HttpError(410, "clip_expired", "the clip has expired");
  }
}

/** A 429 that clears at the next UTC midnight, when the daily counters restart. */
function dailyLimit(code: string, message: string, nowMs: number): HttpError {
  return new HttpError(429, code, message, {}, {
    "retry-after": String(secondsUntilNextUtcDay(nowMs)),
  });
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

const health: Handler = () => Promise.resolve(jsonResponse(200, { ok: true }));

const heartbeat: Handler = async ({ app, body, deviceId }) => {
  const input = parsePresence(parseJsonObject(body));
  if (input.active_crew !== null) {
    await requireMember(app, input.active_crew, deviceId);
  }
  const now = app.now();
  const seenAt = await app.db.updatePresence(deviceId, input, now, PRESENCE_TTL_MS);
  if (seenAt === null) {
    throw new HttpError(409, "stale_presence", "the run/sequence pair must increase while the previous presence is fresh");
  }
  const crews = (await app.db.listCrewPresence(deviceId, now, PRESENCE_TTL_MS)).map((snapshot) => ({
    crew_id: snapshot.crew_id,
    members: snapshot.members.map((row) => ({
      ...row,
      expires_at: row.seen_at_ms + PRESENCE_TTL_MS,
    })),
  }));
  return jsonResponse(200, {
    ok: true,
    seen_at_ms: seenAt,
    expires_at: seenAt + PRESENCE_TTL_MS,
    heartbeat_interval_ms: PRESENCE_HEARTBEAT_MS,
    crews,
  });
};

const listPresence: Handler = async ({ app, deviceId, params }) => {
  const crewId = requireUuidParam(params[0], "crew id");
  await requireMember(app, crewId, deviceId);
  const rows = await app.db.listPresence(crewId, app.now(), PRESENCE_TTL_MS);
  return jsonResponse(200, rows.map((row) => ({
    ...row,
    expires_at: row.seen_at_ms + PRESENCE_TTL_MS,
  })));
};

const registerDevice: Handler = async ({ app, body }) => {
  const input = parseDeviceRegistration(parseJsonObject(body));
  const rawKey = base64Decode(input.public_key_b64);
  if (rawKey === null) {
    throw new HttpError(400, "invalid_request", "public_key_b64 is not valid base64");
  }
  try {
    await importPublicKey(rawKey);
  } catch {
    throw new HttpError(400, "invalid_request", "public_key_b64 is not a valid Ed25519 key");
  }

  const sameKey = (stored: string): boolean => {
    const storedRaw = base64Decode(stored);
    return storedRaw !== null && timingSafeEqual(storedRaw, rawKey);
  };
  const conflict = () =>
    new HttpError(409, "device_exists", "this device id is registered with a different key");

  const existing = await app.db.getDevice(input.device_id);
  if (existing !== null) {
    if (!sameKey(existing.public_key_b64)) {
      throw conflict();
    }
    return jsonResponse(200, { device_id: input.device_id });
  }

  // Registration is open, so the number of new devices per day is capped service wide. Only
  // brand-new devices count; repeating a registration (above) is free.
  const now = app.now();
  const day = utcDay(now);
  if (!(await app.db.reserveCounter(NEW_DEVICES_SCOPE, day, 1, app.limits.newDevicesPerDay))) {
    throw dailyLimit(
      "registration_limited",
      "too many new devices were registered today; try again tomorrow",
      now,
    );
  }
  const inserted = await app.db.insertDeviceIfAbsent({
    ...input,
    created_at: now,
  });
  if (!inserted) {
    await app.db.releaseCounter(NEW_DEVICES_SCOPE, day, 1);
    // Lost a race with a concurrent registration of the same id.
    const winner = await app.db.getDevice(input.device_id);
    if (winner === null || !sameKey(winner.public_key_b64)) {
      throw conflict();
    }
    return jsonResponse(200, { device_id: input.device_id });
  }
  return jsonResponse(201, { device_id: input.device_id });
};

const createCrew: Handler = async ({ app, body, deviceId }) => {
  const name = parseCreateCrew(parseJsonObject(body));
  const crewId = app.randomUuid();
  const created = await app.db.createCrew(
    crewId,
    name,
    deviceId,
    app.now(),
    app.limits.maxCrewsPerDevice,
  );
  if (!created) {
    throw new HttpError(
      409,
      "crew_limit",
      `a device can create at most ${app.limits.maxCrewsPerDevice} crews`,
    );
  }
  return jsonResponse(201, { crew_id: crewId });
};

const createInvite: Handler = async ({ app, deviceId, params }) => {
  const crewId = requireUuidParam(params[0], "crew id");
  await requireMember(app, crewId, deviceId);

  const now = app.now();
  if ((await app.db.countActiveInvites(crewId, now)) >= app.limits.maxActiveInvitesPerCrew) {
    throw new HttpError(
      409,
      "invite_limit",
      `a crew can have at most ${app.limits.maxActiveInvitesPerCrew} valid invites at a time`,
    );
  }
  const expiresAt = now + INVITE_TTL_MS;
  for (let attempt = 0; attempt < INVITE_CREATE_ATTEMPTS; attempt += 1) {
    const code = generateInviteCode(app.randomBytes);
    const inserted = await app.db.insertInvite({
      code,
      crew_id: crewId,
      created_by: deviceId,
      expires_at: expiresAt,
      uses_left: INVITE_USES,
    });
    if (inserted) {
      return jsonResponse(201, { code, expires_at: expiresAt });
    }
  }
  throw new Error("could not allocate a unique invite code");
};

const joinCrew: Handler = async ({ app, body, deviceId }) => {
  const code = parseJoin(parseJsonObject(body));
  const invalid = () =>
    new HttpError(404, "invalid_invite", "the invite code is invalid, expired or used up");

  // Every attempt is counted before the code is looked at, so parallel guesses cannot slip past
  // the limit. A successful join gives its attempt back; only failures add up.
  const now = app.now();
  const day = utcDay(now);
  const scope = joinAttemptsScope(deviceId);
  if (!(await app.db.reserveCounter(scope, day, 1, app.limits.maxJoinAttemptsPerDay))) {
    throw dailyLimit(
      "too_many_attempts",
      "too many failed attempts to join with an invite code today",
      now,
    );
  }

  const invite = await app.db.getInvite(code);
  if (invite === null || invite.expires_at <= now) {
    throw invalid();
  }
  // Joining again with the same device must not burn another use of the invite.
  if (await app.db.isMember(invite.crew_id, deviceId)) {
    await app.db.releaseCounter(scope, day, 1);
    return jsonResponse(200, { crew_id: invite.crew_id });
  }
  const crewId = await app.db.consumeInvite(code, now);
  if (crewId === null) {
    throw invalid();
  }
  await app.db.addMember(crewId, deviceId, now);
  await app.db.releaseCounter(scope, day, 1);
  return jsonResponse(200, { crew_id: crewId });
};

const listMembers: Handler = async ({ app, deviceId, params }) => {
  const crewId = requireUuidParam(params[0], "crew id");
  await requireMember(app, crewId, deviceId);
  return jsonResponse(200, await app.db.listMembers(crewId));
};

const registerClip: Handler = async ({ app, body, deviceId }) => {
  const input = parseClipRegistration(parseJsonObject(body));
  await requireMember(app, input.crew_id, deviceId);

  const now = app.now();
  const respondExisting = (clip: ClipRow): Response => {
    if (clip.owner !== deviceId || clip.crew_id !== input.crew_id) {
      throw new HttpError(409, "clip_exists", "this clip id is already registered");
    }
    requireLiveClip(clip, now);
    return jsonResponse(200, { clip_id: clip.clip_id, expires_at: clip.expires_at });
  };

  const existing = await app.db.getClip(input.clip_id);
  if (existing !== null) {
    return respondExisting(existing);
  }
  // Only new clips count towards the daily limit; repeating a registration is free.
  const day = utcDay(now);
  const scope = clipRegistrationsScope(deviceId);
  if (!(await app.db.reserveCounter(scope, day, 1, app.limits.maxClipsPerDevicePerDay))) {
    throw dailyLimit(
      "clip_registration_limited",
      "too many clips were registered by this device today",
      now,
    );
  }
  const expiresAt = now + input.ttl_s * 1000;
  const inserted = await app.db.insertClip({
    clip_id: input.clip_id,
    crew_id: input.crew_id,
    owner: deviceId,
    created_at: now,
    expires_at: expiresAt,
  });
  if (!inserted) {
    await app.db.releaseCounter(scope, day, 1);
    // Lost a race with a concurrent registration of the same id.
    const winner = await app.db.getClip(input.clip_id);
    if (winner === null) {
      throw new Error("clip vanished after a failed insert");
    }
    return respondExisting(winner);
  }
  return jsonResponse(201, { clip_id: input.clip_id, expires_at: expiresAt });
};

/** One presigned URL of an upload or download response. */
interface SignedObject {
  key: string;
  url: string;
  /** Uploads only: the exact `Content-Length` the PUT must send (it is part of the signature). */
  content_length?: number;
}

/**
 * Presigns the chunk keys (and optionally the manifest) of one `(clip, pov, quality)`.
 *
 * For PUT, `sizes` (parallel to `indices`) and `manifestSize` are signed into the URLs as the
 * exact `Content-Length`, so nothing larger than what the quotas were charged for can be stored.
 */
async function presignObjects(
  app: App,
  method: PresignMethod,
  clip: ClipRow,
  request: {
    pov: string;
    quality: Quality;
    indices: number[];
    manifest: boolean;
    sizes?: number[];
    manifestSize?: number | null;
  },
): Promise<{
  chunks: Array<SignedObject & { index: number }>;
  manifest: SignedObject | null;
}> {
  const chunks = await Promise.all(
    request.indices.map(async (index, i) => {
      const key = objectKey(clip.crew_id, clip.clip_id, request.pov, request.quality, index);
      const contentLength = method === "PUT" ? request.sizes?.[i] : undefined;
      const url = await app.presigner.presign(method, key, { contentLength });
      return contentLength === undefined
        ? { index, key, url }
        : { index, key, url, content_length: contentLength };
    }),
  );
  let manifest: SignedObject | null = null;
  if (request.manifest) {
    const key = manifestKey(clip.crew_id, clip.clip_id, request.pov, request.quality);
    const contentLength = method === "PUT" ? (request.manifestSize ?? undefined) : undefined;
    const url = await app.presigner.presign(method, key, { contentLength });
    manifest =
      contentLength === undefined ? { key, url } : { key, url, content_length: contentLength };
  }
  return { chunks, manifest };
}

const uploadUrls: Handler = async ({ app, body, deviceId, params }) => {
  const clip = await loadClipForMember(app, params[0], deviceId);
  const now = app.now();
  requireLiveClip(clip, now);
  const input = parseUploadRequest(parseJsonObject(body));
  if (input.pov !== deviceId) {
    throw new HttpError(403, "pov_mismatch", "a device can only upload its own POV");
  }

  // The manifest is accounted like a chunk (under a reserved index), so it cannot be used to
  // store unmetered data and asking for its URL again is free.
  const accounted =
    input.manifest_size === null
      ? { indices: input.indices, sizes: input.sizes }
      : {
          indices: [...input.indices, MANIFEST_INDEX],
          sizes: [...input.sizes, input.manifest_size],
        };
  const existing = await app.db.getChunkSizes(
    clip.clip_id,
    input.pov,
    input.quality,
    accounted.indices,
  );
  const delta = additionalBytes(existing, accounted.indices, accounted.sizes);
  if (evaluateQuota(clip.bytes, 0, delta) === "clip_limit") {
    throw clipQuotaExceeded();
  }

  // Presigning has no side effects, so do it before anything is reserved.
  const signed = await presignObjects(app, "PUT", clip, {
    ...input,
    manifestSize: input.manifest_size,
  });

  const verdict = await app.db.reserveBytes({
    clipId: clip.clip_id,
    deviceId,
    day: utcDay(now),
    delta,
    clipLimit: MAX_CLIP_BYTES,
    dayLimit: MAX_DAILY_BYTES,
    globalDayLimit: app.limits.globalDailyBytes,
  });
  if (verdict === "clip_limit") {
    throw clipQuotaExceeded();
  }
  if (verdict === "day_limit") {
    throw dailyLimit("daily_quota_exceeded", "the daily upload quota was reached", now);
  }
  if (verdict === "global_limit") {
    throw dailyLimit(
      "global_quota_exceeded",
      "the service-wide daily upload budget was reached",
      now,
    );
  }
  await app.db.recordChunks(
    clip.clip_id,
    input.pov,
    input.quality,
    accounted.indices.map((index, i) => [index, chargedBytes(accounted.sizes[i] ?? 0)] as const),
  );

  return jsonResponse(200, {
    expires_in: PRESIGN_EXPIRES_S,
    expires_at: now + PRESIGN_EXPIRES_S * 1000,
    headers: { "Content-Type": UPLOAD_CONTENT_TYPE },
    chunks: signed.chunks,
    manifest: signed.manifest,
  });
};

function clipQuotaExceeded(): HttpError {
  return new HttpError(413, "clip_quota_exceeded", "the clip would exceed its total size quota");
}

const downloadUrls: Handler = async ({ app, body, deviceId, params }) => {
  const clip = await loadClipForMember(app, params[0], deviceId);
  const now = app.now();
  requireLiveClip(clip, now);
  const input = parseDownloadRequest(parseJsonObject(body));
  const signed = await presignObjects(app, "GET", clip, input);
  return jsonResponse(200, {
    expires_in: PRESIGN_EXPIRES_S,
    expires_at: now + PRESIGN_EXPIRES_S * 1000,
    chunks: signed.chunks,
    manifest: signed.manifest,
  });
};

const deleteClip: Handler = async ({ app, deviceId, params }) => {
  const clip = await loadClipForMember(app, params[0], deviceId);
  // Deleting again is allowed and re-runs the object deletion: it removes uploads that landed
  // through a still-valid presigned URL after the first delete, or finishes a cut-short one.
  const result = await deleteClipObjects(app.store, clip.crew_id, clip.clip_id);
  await app.db.markClipDeleted(clip.clip_id, app.now());
  return jsonResponse(200, {
    ok: true,
    deleted_objects: result.deleted,
    complete: result.complete,
  });
};

// ---------------------------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------------------------

const ROUTES: readonly Route[] = [
  { method: "GET", segments: ["v1", "health"], auth: false, handler: health },
  { method: "POST", segments: ["v1", "devices"], auth: false, handler: registerDevice },
  { method: "POST", segments: ["v1", "presence"], auth: true, handler: heartbeat },
  { method: "POST", segments: ["v1", "crews"], auth: true, handler: createCrew },
  { method: "POST", segments: ["v1", "crews", "join"], auth: true, handler: joinCrew },
  {
    method: "POST",
    segments: ["v1", "crews", ":crew", "invites"],
    auth: true,
    handler: createInvite,
  },
  {
    method: "GET",
    segments: ["v1", "crews", ":crew", "members"],
    auth: true,
    handler: listMembers,
  },
  {
    method: "GET",
    segments: ["v1", "crews", ":crew", "presence"],
    auth: true,
    handler: listPresence,
  },
  { method: "POST", segments: ["v1", "clips"], auth: true, handler: registerClip },
  {
    method: "POST",
    segments: ["v1", "clips", ":clip", "upload-urls"],
    auth: true,
    handler: uploadUrls,
  },
  {
    method: "POST",
    segments: ["v1", "clips", ":clip", "download-urls"],
    auth: true,
    handler: downloadUrls,
  },
  { method: "DELETE", segments: ["v1", "clips", ":clip"], auth: true, handler: deleteClip },
];

/** Returns the parameter values if `path` matches `segments`, otherwise `null`. */
function matchSegments(segments: readonly string[], path: readonly string[]): string[] | null {
  if (segments.length !== path.length) {
    return null;
  }
  const params: string[] = [];
  for (let i = 0; i < segments.length; i += 1) {
    const pattern = segments[i];
    const actual = path[i];
    if (pattern === undefined || actual === undefined) {
      return null;
    }
    if (pattern.startsWith(":")) {
      if (actual.length === 0) {
        return null;
      }
      params.push(actual);
    } else if (pattern !== actual) {
      return null;
    }
  }
  return params;
}

async function dispatch(request: Request, app: App): Promise<Response> {
  const url = new URL(request.url);
  const path = url.pathname.split("/").slice(1);

  let pathMatched = false;
  let selected: { route: Route; params: string[] } | null = null;
  const allowed: string[] = [];
  for (const route of ROUTES) {
    const params = matchSegments(route.segments, path);
    if (params === null) {
      continue;
    }
    pathMatched = true;
    allowed.push(route.method);
    if (route.method === request.method) {
      selected = { route, params };
    }
  }
  if (selected === null) {
    if (pathMatched) {
      throw new HttpError(405, "method_not_allowed", "method not allowed for this path", {}, {
        allow: allowed.join(", "),
      });
    }
    throw new HttpError(404, "not_found", "no such route");
  }

  const body = await readBody(request, MAX_BODY_BYTES);
  const deviceId = selected.route.auth
    ? await authenticate({ db: app.db, now: () => app.now() }, request, body)
    : "";
  return selected.route.handler({ app, body, deviceId, params: selected.params });
}

/**
 * Handles one HTTP request. Never throws: failures become JSON error responses.
 *
 * @param request The incoming request.
 * @param app Injected dependencies (database, object store, presigner, clock, randomness).
 */
export async function handleRequest(request: Request, app: App): Promise<Response> {
  try {
    return await dispatch(request, app);
  } catch (error) {
    if (error instanceof HttpError) {
      return errorResponse(error);
    }
    // Log only the message: stack traces and causes can embed URLs or key material.
    console.error(
      JSON.stringify({
        event: "unhandled_error",
        message: error instanceof Error ? error.message : "unknown error",
      }),
    );
    return errorResponse(new HttpError(500, "internal_error", "internal server error"));
  }
}
