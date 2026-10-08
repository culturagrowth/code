/** Exercise the friends/crew flow with two temporary signing identities. No capture or R2. */
import { randomUUID } from "node:crypto";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { canonicalString } from "../src/auth.js";
import { base64Encode, sha256Hex, utf8 } from "../src/encoding.js";
import { isUuid } from "../src/keys.js";

type HttpFetch = (url: string, init: RequestInit) => Promise<Response>;
interface ProbeDevice { id: string; name: string; key: CryptoKey; publicKey: string }
interface Member { device_id: string; game: string | null; active_crew: string | null }
interface Heartbeat { crews: Array<{ crew_id: string; members: Member[] }> }

export interface CrewSmokeResult {
  ok: boolean;
  checks: Array<{ step: string; status: number }>;
  cleanup: "idle" | "not_completed";
  failure?: { step: string; code: string };
}

class ProbeError extends Error {}

/** Require a Worker origin, allowing cleartext HTTP only on loopback. */
export function workerOrigin(input: string): string {
  const url = new URL(input);
  const loopback = ["localhost", "127.0.0.1", "[::1]"].includes(url.hostname);
  if ((url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) ||
      url.username || url.password || url.pathname !== "/" || url.search || url.hash) {
    throw new ProbeError("invalid_worker_origin");
  }
  return url.origin;
}

async function device(name: string): Promise<ProbeDevice> {
  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, false, ["sign", "verify"]) as CryptoKeyPair;
  const raw = await crypto.subtle.exportKey("raw", keys.publicKey) as ArrayBuffer;
  const publicKey = base64Encode(new Uint8Array(raw));
  return { id: randomUUID(), name, key: keys.privateKey, publicKey };
}

/** Creates two test devices and a crew; keys remain only in memory. Prints no invite or signature. */
export async function runCrewSmoke(
  baseUrl: string,
  httpFetch: HttpFetch = (url, init) => fetch(url, init),
  now: () => number = Date.now,
): Promise<CrewSmokeResult> {
  const result: CrewSmokeResult = { ok: false, checks: [], cleanup: "not_completed" };
  let step = "configuration";
  try {
    const origin = workerOrigin(baseUrl);
    // Increasing timestamps avoid identical signatures when sequential calls share a millisecond.
    let lastTimestamp = 0;
    const call = async <T>(label: string, method: string, path: string, body?: unknown, who?: ProbeDevice): Promise<T> => {
      step = label;
      const url = new URL(path, origin);
      const text = body === undefined ? "" : JSON.stringify(body);
      const headers: Record<string, string> = { "content-type": "application/json" };
      if (who) {
        const timestamp = Math.max(now(), lastTimestamp + 1);
        lastTimestamp = timestamp;
        const canonical = canonicalString(method, url.pathname + url.search, timestamp, await sha256Hex(utf8(text)));
        headers["x-dc-device"] = who.id;
        headers["x-dc-timestamp"] = String(timestamp);
        headers["x-dc-signature"] = base64Encode(new Uint8Array(await crypto.subtle.sign("Ed25519", who.key, utf8(canonical))));
      }
      const response = await httpFetch(url.href, {
        method, headers, ...(text ? { body: text } : {}),
        signal: AbortSignal.timeout(15_000), redirect: "error",
      });
      result.checks.push({ step: label, status: response.status });
      if (!response.ok) throw new ProbeError(`http_${response.status}`);
      try {
        return await response.json() as T;
      } catch {
        throw new ProbeError("invalid_json");
      }
    };
    const health = await call<{ ok: boolean }>("health", "GET", "/v1/health");
    if (health?.ok !== true) throw new ProbeError("health_mismatch");
    step = "generate_devices";
    const alice = await device("DuoClip teste A");
    const bob = await device("DuoClip teste B");
    for (const [label, who] of [["register_a", alice], ["register_b", bob]] as const) {
      const registered = await call<{ device_id: string }>(label, "POST", "/v1/devices", {
        device_id: who.id, public_key_b64: who.publicKey, display_name: who.name,
      });
      if (registered?.device_id !== who.id) throw new ProbeError("registration_mismatch");
    }
    const crew = await call<{ crew_id: string }>("create_crew", "POST", "/v1/crews", { name: "DuoClip teste de amigos" }, alice);
    if (!isUuid(crew?.crew_id)) throw new ProbeError("crew_mismatch");
    const path = `/v1/crews/${crew.crew_id}`;
    const invite = await call<{ code: string }>("create_invite", "POST", `${path}/invites`, undefined, alice);
    if (typeof invite?.code !== "string") throw new ProbeError("invite_mismatch");
    const joined = await call<{ crew_id: string }>("join_crew", "POST", "/v1/crews/join", { code: invite.code }, bob);
    if (joined?.crew_id !== crew.crew_id) throw new ProbeError("join_mismatch");
    const members = await call<Member[]>("members", "GET", `${path}/members`, undefined, alice);
    const both = (rows: Member[]): boolean => Array.isArray(rows) && rows.length === 2 &&
      [alice, bob].every((who) => rows.some((row) => row.device_id === who.id));
    if (!both(members)) throw new ProbeError("members_mismatch");

    // Synthetic presence time is deliberate: this diagnostic does not replace the app's AppClock.
    const presence = { game: "cs2", active_crew: crew.crew_id, seated_since_ms: 0, online_since_ms: 0 };
    const heartbeat = (label: string, who: ProbeDevice, seq: number, idle = false): Promise<Heartbeat> =>
      call(label, "POST", "/v1/presence", {
        ...presence, seq, ...(idle ? { game: null, active_crew: null, seated_since_ms: null } : {}),
      }, who);
    await heartbeat("heartbeat_a", alice, 0);
    await heartbeat("heartbeat_b", bob, 0);
    for (const [label, who] of [["snapshot_a", alice], ["snapshot_b", bob]] as const) {
      const snapshot = await heartbeat(label, who, 1);
      const rows = snapshot?.crews?.find((c) => c.crew_id === crew.crew_id)?.members;
      if (!rows || !both(rows) || rows.some((row) => row.game !== presence.game || row.active_crew !== crew.crew_id)) {
        throw new ProbeError("presence_mismatch");
      }
    }
    await heartbeat("idle_a", alice, 2, true);
    const idle = await heartbeat("idle_b", bob, 2, true);
    const idleMembers = idle?.crews?.find((c) => c.crew_id === crew.crew_id)?.members;
    if (!idleMembers || !both(idleMembers) || idleMembers.some((row) => row.game !== null || row.active_crew !== null)) {
      throw new ProbeError("idle_mismatch");
    }
    result.cleanup = "idle";
    result.ok = true;
  } catch (error) {
    result.failure = { step, code: error instanceof ProbeError ? error.message : "request_or_response_failed" };
  }
  return result;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.length === 1 && args[0] === "--help") {
    console.log("Usage: npm run test:crew -- [--url http://127.0.0.1:8787]");
    console.log("Creates two temporary test devices and one crew; finishes with both players idle. No capture or R2.");
    return;
  }
  if (args.length !== 0 && !(args.length === 2 && args[0] === "--url")) {
    console.error("Usage: npm run test:crew -- [--url http://127.0.0.1:8787]");
    process.exitCode = 1;
    return;
  }
  const result = await runCrewSmoke(args[1] ?? "http://127.0.0.1:8787");
  console.log(JSON.stringify(result, null, 2));
  if (!result.ok) process.exitCode = 1;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  await main();
}
