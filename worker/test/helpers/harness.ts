/**
 * Test harness: a fully wired {@link App} with an in-memory SQLite (real SQL), an in-memory
 * object store, a controllable clock, seeded randomness, and signing test devices.
 */

import type { App } from "../../src/app.js";
import { canonicalString } from "../../src/auth.js";
import { createD1Db } from "../../src/db.js";
import { base64Encode, sha256Hex, utf8 } from "../../src/encoding.js";
import { createPresigner } from "../../src/presign.js";
import { DEFAULT_LIMITS } from "../../src/quota.js";
import type { Limits } from "../../src/quota.js";
import { handleRequest } from "../../src/routes.js";
import { MemoryObjectStore } from "./memory-store.js";
import { SqliteD1 } from "./sqlite-d1.js";

/** Fixed start of every test: 2026-10-08T12:00:00Z. */
export const T0 = Date.UTC(2026, 9, 8, 12, 0, 0);

export const ORIGIN = "https://worker.test";

/** Deterministic PRNG (mulberry32). */
export function seededRandom(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) >>> 0;
    let t = state;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** A UUID v4 look-alike derived from a seeded generator. */
function seededUuid(random: () => number): string {
  const bytes = Array.from({ length: 16 }, () => Math.floor(random() * 256));
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x40;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

/** The pieces a test usually needs. */
export interface Harness {
  app: App;
  d1: SqliteD1;
  store: MemoryObjectStore;
  /** Mutable clock; tests advance it by assigning `clock.now`. */
  clock: { now: number };
  /** Runs a request through the router. */
  send(request: Request): Promise<Response>;
  /** Creates a device with a deterministic key pair (does not register it). */
  device(seed: number, displayName?: string): Promise<TestDevice>;
}

/** Builds a fresh harness. */
export function createHarness(
  options: { seed?: number; storePageSize?: number; limits?: Partial<Limits> } = {},
): Harness {
  const random = seededRandom(options.seed ?? 1);
  const d1 = new SqliteD1();
  const store = new MemoryObjectStore(options.storePageSize ?? 1000);
  const clock = { now: T0 };
  const now = () => clock.now;
  const app: App = {
    db: createD1Db(d1),
    store,
    presigner: createPresigner(
      {
        accountId: "testaccount",
        bucketName: "test-bucket",
        accessKeyId: "AKIATESTKEY",
        secretAccessKey: "test-secret-key",
      },
      now,
    ),
    limits: { ...DEFAULT_LIMITS, ...options.limits },
    now,
    randomBytes: (length) => Uint8Array.from({ length }, () => Math.floor(random() * 256)),
    randomUuid: () => seededUuid(random),
  };
  const harness: Harness = {
    app,
    d1,
    store,
    clock,
    send: (request) => handleRequest(request, app),
    device: (seed, displayName) => TestDevice.fromSeed(harness, seed, displayName),
  };
  return harness;
}

const PKCS8_ED25519_PREFIX = Uint8Array.from([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
]);

function base64UrlToBytes(text: string): Uint8Array {
  const padded = text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "=");
  return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

/** A device with a deterministic Ed25519 key that can sign requests like the real app. */
export class TestDevice {
  private constructor(
    private readonly harness: Harness,
    readonly id: string,
    readonly displayName: string,
    readonly publicKeyB64: string,
    private readonly privateKey: CryptoKey,
  ) {}

  /** Derives a key pair and device id from `seed`. */
  static async fromSeed(harness: Harness, seed: number, displayName = `player${seed}`): Promise<TestDevice> {
    const random = seededRandom(1000 + seed);
    const secret = Uint8Array.from({ length: 32 }, () => Math.floor(random() * 256));
    const pkcs8 = new Uint8Array(PKCS8_ED25519_PREFIX.length + secret.length);
    pkcs8.set(PKCS8_ED25519_PREFIX);
    pkcs8.set(secret, PKCS8_ED25519_PREFIX.length);
    const privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "Ed25519" }, true, ["sign"]);
    const jwk = (await crypto.subtle.exportKey("jwk", privateKey)) as JsonWebKey;
    const publicKeyB64 = base64Encode(base64UrlToBytes(jwk.x ?? ""));
    return new TestDevice(harness, seededUuid(random), displayName, publicKeyB64, privateKey);
  }

  /** Body of `POST /v1/devices` for this device. */
  registrationBody(): Record<string, unknown> {
    return {
      device_id: this.id,
      public_key_b64: this.publicKeyB64,
      display_name: this.displayName,
    };
  }

  /** Registers the device (unsigned route) and asserts success. */
  async register(): Promise<void> {
    const res = await this.harness.send(
      new Request(`${ORIGIN}/v1/devices`, {
        method: "POST",
        body: JSON.stringify(this.registrationBody()),
      }),
    );
    if (res.status !== 201 && res.status !== 200) {
      throw new Error(`registration failed: ${res.status} ${await res.text()}`);
    }
  }

  /** Signs an arbitrary canonical string. */
  async signCanonical(canonical: string): Promise<string> {
    const signature = await crypto.subtle.sign("Ed25519", this.privateKey, utf8(canonical));
    return base64Encode(new Uint8Array(signature));
  }

  /**
   * Builds a correctly signed request. Every call advances the harness clock by 1 ms so that
   * two identical requests get different timestamps (and therefore different signatures).
   */
  async signed(
    method: string,
    pathWithQuery: string,
    body?: unknown,
    overrides: { timestamp?: number; signedBody?: string; signedPath?: string; signedMethod?: string } = {},
  ): Promise<Request> {
    this.harness.clock.now += 1;
    const rawBody = body === undefined ? "" : typeof body === "string" ? body : JSON.stringify(body);
    const timestamp = overrides.timestamp ?? this.harness.clock.now;
    const canonical = canonicalString(
      overrides.signedMethod ?? method,
      overrides.signedPath ?? pathWithQuery,
      timestamp,
      await sha256Hex(utf8(overrides.signedBody ?? rawBody)),
    );
    const init: RequestInit = {
      method,
      headers: {
        "x-dc-device": this.id,
        "x-dc-timestamp": String(timestamp),
        "x-dc-signature": await this.signCanonical(canonical),
      },
    };
    if (rawBody.length > 0) {
      init.body = rawBody;
    }
    return new Request(`${ORIGIN}${pathWithQuery}`, init);
  }

  /** Signs and sends a request. */
  async call(method: string, pathWithQuery: string, body?: unknown): Promise<Response> {
    return this.harness.send(await this.signed(method, pathWithQuery, body));
  }
}

/** Reads a JSON response body. */
export async function json<T = Record<string, unknown>>(response: Response): Promise<T> {
  return (await response.json()) as T;
}

/** Registers `device`, creates a crew with it, and returns the crew id. */
export async function newCrew(device: TestDevice, name = "Squad"): Promise<string> {
  await device.register();
  const res = await device.call("POST", "/v1/crews", { name });
  return (await json<{ crew_id: string }>(res)).crew_id;
}

/** Registers `joiner` and joins it to `crewId` through an invite created by `owner`. */
export async function joinCrew(owner: TestDevice, joiner: TestDevice, crewId: string): Promise<void> {
  await joiner.register();
  const invite = await json<{ code: string }>(await owner.call("POST", `/v1/crews/${crewId}/invites`));
  const res = await joiner.call("POST", "/v1/crews/join", { code: invite.code });
  if (res.status !== 200) {
    throw new Error(`join failed: ${res.status} ${await res.text()}`);
  }
}
