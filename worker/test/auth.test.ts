import { describe, expect, it } from "vitest";
import {
  authenticate,
  canonicalString,
  MAX_SKEW_MS,
  REPLAY_WINDOW_MS,
  verifySignature,
} from "../src/auth.js";
import { createD1Db } from "../src/db.js";
import { sha256Hex, utf8 } from "../src/encoding.js";
import { HttpError } from "../src/http.js";
import { createHarness, json, ORIGIN, T0 } from "./helpers/harness.js";
import type { Harness, TestDevice } from "./helpers/harness.js";

describe("canonicalString", () => {
  it("joins version, method, path with query, timestamp and body hash with newlines", () => {
    expect(canonicalString("post", "/v1/crews?x=1", 1_700_000_000_000, "abc")).toBe(
      "DC1\nPOST\n/v1/crews?x=1\n1700000000000\nabc",
    );
    expect(canonicalString("GET", "/v1/health", "5", "e3b0")).toBe("DC1\nGET\n/v1/health\n5\ne3b0");
  });
});

describe("verifySignature", () => {
  it("accepts a good signature and rejects tampering and malformed inputs", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const message = canonicalString("GET", "/v1/crews/x/members", T0, await sha256Hex(utf8("")));
    const signature = Uint8Array.from(atob(await alice.signCanonical(message)), (c) => c.charCodeAt(0));

    expect(await verifySignature(alice.publicKeyB64, signature, message)).toBe(true);
    expect(await verifySignature(alice.publicKeyB64, signature, `${message}x`)).toBe(false);
    const flipped = signature.slice();
    flipped[0] = (flipped[0] ?? 0) ^ 1;
    expect(await verifySignature(alice.publicKeyB64, flipped, message)).toBe(false);
    expect(await verifySignature(alice.publicKeyB64, signature.slice(0, 63), message)).toBe(false);
    expect(await verifySignature("not-base64", signature, message)).toBe(false);
    expect(await verifySignature(btoa("short"), signature, message)).toBe(false);

    const bob = await h.device(2);
    expect(await verifySignature(bob.publicKeyB64, signature, message)).toBe(false);
  });
});

async function setup(): Promise<{ h: Harness; alice: TestDevice }> {
  const h = createHarness();
  const alice = await h.device(1);
  await alice.register();
  return { h, alice };
}

async function failure(h: Harness, request: Request): Promise<{ status: number; reason: unknown; body: Record<string, unknown> }> {
  const res = await h.send(request);
  const body = await json(res);
  return { status: res.status, reason: body["reason"], body };
}

describe("signed requests", () => {
  it("accepts a valid signature", async () => {
    const { alice } = await setup();
    const res = await alice.call("POST", "/v1/crews", { name: "Squad" });
    expect(res.status).toBe(201);
  });

  it("accepts a valid signature over a path with a query string", async () => {
    const { h, alice } = await setup();
    const crew = (await json<{ crew_id: string }>(await alice.call("POST", "/v1/crews", { name: "Squad" }))).crew_id;
    const res = await alice.call("GET", `/v1/crews/${crew}/members?page=1&x=%20y`);
    expect(res.status).toBe(200);
    expect(h.clock.now).toBeGreaterThan(T0);
  });

  it("rejects a modified body", async () => {
    const { h, alice } = await setup();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { signedBody: '{"name":"Other"}' });
    expect(await failure(h, request)).toMatchObject({ status: 401, reason: "bad_signature" });
  });

  it("rejects a modified path or query", async () => {
    const { h, alice } = await setup();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { signedPath: "/v1/crews?a=1" });
    expect(await failure(h, request)).toMatchObject({ status: 401, reason: "bad_signature" });
    const other = await alice.signed("GET", "/v1/crews/x/members?page=1", undefined, {
      signedPath: "/v1/crews/x/members?page=2",
    });
    expect(await failure(h, other)).toMatchObject({ status: 401, reason: "bad_signature" });
  });

  it("rejects a modified method", async () => {
    const { h, alice } = await setup();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { signedMethod: "PUT" });
    expect(await failure(h, request)).toMatchObject({ status: 401, reason: "bad_signature" });
  });

  it("rejects a modified timestamp", async () => {
    const { h, alice } = await setup();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" });
    const headers = new Headers(request.headers);
    headers.set("x-dc-timestamp", String(Number(headers.get("x-dc-timestamp")) + 1));
    const tampered = new Request(request.url, { method: "POST", headers, body: '{"name":"Squad"}' });
    expect(await failure(h, tampered)).toMatchObject({ status: 401, reason: "bad_signature" });
  });

  it("rejects a signature made by another device's key", async () => {
    const { h, alice } = await setup();
    const mallory = await h.device(9);
    const forged = await mallory.signed("POST", "/v1/crews", { name: "Squad" });
    const headers = new Headers(forged.headers);
    headers.set("x-dc-device", alice.id);
    const request = new Request(forged.url, { method: "POST", headers, body: '{"name":"Squad"}' });
    expect(await failure(h, request)).toMatchObject({ status: 401, reason: "bad_signature" });
  });

  it("rejects unknown devices", async () => {
    const { h } = await setup();
    const stranger = await h.device(9);
    const request = await stranger.signed("POST", "/v1/crews", { name: "Squad" });
    expect(await failure(h, request)).toMatchObject({ status: 401, reason: "unknown_device" });
  });

  it("rejects missing or malformed headers", async () => {
    const { h, alice } = await setup();
    const good = await alice.signed("POST", "/v1/crews", { name: "Squad" });
    const rebuild = (patch: (headers: Headers) => void) => {
      const headers = new Headers(good.headers);
      patch(headers);
      return new Request(good.url, { method: "POST", headers, body: '{"name":"Squad"}' });
    };
    expect(await failure(h, new Request(`${ORIGIN}/v1/crews`, { method: "POST", body: "{}" }))).toMatchObject({
      status: 401,
      reason: "missing_headers",
    });
    for (const header of ["x-dc-device", "x-dc-timestamp", "x-dc-signature"]) {
      expect(await failure(h, rebuild((headers) => headers.delete(header)))).toMatchObject({
        status: 401,
        reason: "missing_headers",
      });
    }
    const malformed: Array<[string, string]> = [
      ["x-dc-device", alice.id.toUpperCase()],
      ["x-dc-device", "not-a-uuid"],
      ["x-dc-timestamp", "12abc"],
      ["x-dc-timestamp", "-5"],
      ["x-dc-timestamp", "1.5e12"],
      ["x-dc-timestamp", ""],
      ["x-dc-signature", "AAAA"],
      ["x-dc-signature", "!!!!"],
    ];
    for (const [name, value] of malformed) {
      expect(await failure(h, rebuild((headers) => headers.set(name, value))), `${name}=${value}`).toMatchObject({
        status: 401,
        reason: "bad_headers",
      });
    }
  });

  it("never reveals secrets in error bodies", async () => {
    const { h, alice } = await setup();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { signedBody: "x" });
    const text = await (await h.send(request)).text();
    expect(text).not.toContain(alice.publicKeyB64);
  });
});

describe("clock skew", () => {
  it("accepts timestamps exactly 5 minutes away and rejects anything further", async () => {
    expect(MAX_SKEW_MS).toBe(300_000);
    const { h, alice } = await setup();
    // `signed()` advances the harness clock by 1 ms first, so aim relative to the new time.
    const skewed = (offset: number) => alice.signed("POST", "/v1/crews", { name: "Squad" }, { timestamp: h.clock.now + 1 + offset });
    for (const offset of [-MAX_SKEW_MS, MAX_SKEW_MS]) {
      const request = await skewed(offset);
      expect(Math.abs(Number(request.headers.get("x-dc-timestamp")) - h.clock.now)).toBe(MAX_SKEW_MS);
      expect((await h.send(request)).status).toBe(201);
    }
    for (const offset of [-MAX_SKEW_MS - 1, MAX_SKEW_MS + 1, -3_600_000, 86_400_000]) {
      const result = await failure(h, await skewed(offset));
      expect(result).toMatchObject({ status: 401, reason: "clock_skew" });
      expect(result.body["server_time_ms"]).toBe(h.clock.now);
    }
  });
});

describe("replay protection", () => {
  it("rejects the exact same request the second time", async () => {
    const { h, alice } = await setup();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" });
    const copy = request.clone();
    expect((await h.send(request)).status).toBe(201);
    expect(await failure(h, copy)).toMatchObject({ status: 401, reason: "replay" });
  });

  it("does not record signatures that failed verification", async () => {
    const { h, alice } = await setup();
    const bad = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { signedBody: "other" });
    await h.send(bad);
    expect(h.d1.query("SELECT sig FROM seen_signatures")).toHaveLength(0);
  });

  it("remembers a signature for exactly the replay window", async () => {
    expect(REPLAY_WINDOW_MS).toBe(600_000);
    const db = createD1Db(createHarness().d1);
    expect(await db.recordSignature("sig", 1_000_000, REPLAY_WINDOW_MS)).toBe(true);
    expect(await db.recordSignature("sig", 1_000_001, REPLAY_WINDOW_MS)).toBe(false);
    expect(await db.recordSignature("sig", 1_000_000 + REPLAY_WINDOW_MS - 1, REPLAY_WINDOW_MS)).toBe(false);
    expect(await db.recordSignature("sig", 1_000_000 + REPLAY_WINDOW_MS, REPLAY_WINDOW_MS)).toBe(false);
    // Past the window the entry no longer counts and is refreshed.
    expect(await db.recordSignature("sig", 1_000_000 + REPLAY_WINDOW_MS + 1, REPLAY_WINDOW_MS)).toBe(true);
    expect(await db.recordSignature("sig", 1_000_000 + REPLAY_WINDOW_MS + 2, REPLAY_WINDOW_MS)).toBe(false);
    expect(await db.recordSignature("other", 1_000_000, REPLAY_WINDOW_MS)).toBe(true);
  });

  it("allows identical requests that carry different timestamps", async () => {
    const { alice } = await setup();
    expect((await alice.call("POST", "/v1/crews", { name: "Squad" })).status).toBe(201);
    expect((await alice.call("POST", "/v1/crews", { name: "Squad" })).status).toBe(201);
  });
});

describe("authenticate", () => {
  it("throws HttpError(401) and returns the device id on success", async () => {
    const { h, alice } = await setup();
    const deps = { db: h.app.db, now: () => h.clock.now };
    const request = await alice.signed("GET", "/v1/crews/x/members");
    await expect(authenticate(deps, request, new Uint8Array(0))).resolves.toBe(alice.id);
    await expect(authenticate(deps, request, new Uint8Array(0))).rejects.toBeInstanceOf(HttpError);
  });
});
