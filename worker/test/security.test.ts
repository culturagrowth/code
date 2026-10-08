/**
 * Regression tests for the adversarial security review: authentication edge cases, authorization
 * (IDOR) across every route, key and SQL injection, body limits and information leaks.
 */

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it, vi } from "vitest";
import {
  authenticate,
  canonicalString,
  hasCanonicalScalar,
  MAX_SKEW_MS,
  REPLAY_WINDOW_MS,
  verifySignature,
} from "../src/auth.js";
import { base64Decode, sha256Hex, utf8 } from "../src/encoding.js";
import { HttpError, MAX_BODY_BYTES, readBody } from "../src/http.js";
import { clipPrefix, validateObjectKey } from "../src/keys.js";
import { createHarness, joinCrew, json, newCrew, seededRandom, T0 } from "./helpers/harness.js";
import type { Harness, TestDevice } from "./helpers/harness.js";

// ---------------------------------------------------------------------------------------------
// Authentication
// ---------------------------------------------------------------------------------------------

/** Order of the Ed25519 group, as a 32-byte little-endian scalar. */
const L = (1n << 252n) + 27742317777372353535851937790883648493n;

function signatureWithScalar(scalar: bigint): Uint8Array {
  const signature = new Uint8Array(64).fill(0x11); // the R half is irrelevant to the check
  let rest = scalar;
  for (let i = 32; i < 64; i += 1) {
    signature[i] = Number(rest & 0xffn);
    rest >>= 8n;
  }
  return signature;
}

describe("Ed25519 signature malleability", () => {
  it("accepts only scalars below the group order", () => {
    expect(hasCanonicalScalar(signatureWithScalar(0n))).toBe(true);
    expect(hasCanonicalScalar(signatureWithScalar(L - 1n))).toBe(true);
    expect(hasCanonicalScalar(signatureWithScalar(L))).toBe(false);
    expect(hasCanonicalScalar(signatureWithScalar(L + 1n))).toBe(false);
    expect(hasCanonicalScalar(signatureWithScalar((1n << 256n) - 1n))).toBe(false);
  });

  it("rejects signatures of the wrong length", () => {
    expect(hasCanonicalScalar(new Uint8Array(0))).toBe(false);
    expect(hasCanonicalScalar(new Uint8Array(63))).toBe(false);
    expect(hasCanonicalScalar(new Uint8Array(65))).toBe(false);
  });

  it("rejects the second signature string (S + L) of an otherwise valid signature", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const message = canonicalString("GET", "/v1/health", T0, await sha256Hex(utf8("")));
    const signature = base64Decode(await alice.signCanonical(message));
    expect(signature).not.toBeNull();
    const good = signature ?? new Uint8Array(64);
    expect(hasCanonicalScalar(good)).toBe(true);
    expect(await verifySignature(alice.publicKeyB64, good, message)).toBe(true);

    let scalar = 0n;
    for (let i = 63; i >= 32; i -= 1) {
      scalar = (scalar << 8n) | BigInt(good[i] ?? 0);
    }
    const forged = new Uint8Array(good);
    forged.set(signatureWithScalar(scalar + L).slice(32), 32);
    expect(hasCanonicalScalar(forged)).toBe(false);
    expect(await verifySignature(alice.publicKeyB64, forged, message)).toBe(false);
  });

  it("does not depend on the runtime's verifier being strict", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const message = canonicalString("GET", "/v1/health", T0, await sha256Hex(utf8("")));
    const forged = signatureWithScalar(L + 5n);
    // A lax verifier that accepts everything, like a non-strict Ed25519 implementation.
    const lax = vi.spyOn(crypto.subtle, "verify").mockResolvedValue(true);
    try {
      expect(await verifySignature(alice.publicKeyB64, forged, message)).toBe(false);
      expect(lax).not.toHaveBeenCalled();
      expect(await verifySignature(alice.publicKeyB64, signatureWithScalar(5n), message)).toBe(true);
    } finally {
      lax.mockRestore();
    }
  });
});

describe("replay protection", () => {
  it("lets exactly one of two simultaneous identical requests through", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" });
    const results = await Promise.all([h.send(request.clone()), h.send(request.clone()), h.send(request)]);
    expect(results.map((r) => r.status).sort()).toEqual([201, 401, 401]);
    expect(h.d1.query("SELECT * FROM crews")).toHaveLength(1);
  });

  it("covers the whole time a signature is acceptable, with no gap at the edge", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    // A request dated as far in the future as the skew allows is first seen "now" ...
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { timestamp: h.clock.now + 1 + MAX_SKEW_MS });
    const replay = request.clone();
    const late = request.clone();
    expect((await h.send(request)).status).toBe(201);

    // ... and stays acceptable until timestamp + skew, which is exactly when the replay window ends.
    h.clock.now = Number(replay.headers.get("x-dc-timestamp")) + MAX_SKEW_MS;
    expect(h.clock.now - T0).toBeLessThanOrEqual(REPLAY_WINDOW_MS + 1);
    const atEdge = await h.send(replay);
    expect(atEdge.status).toBe(401);
    expect((await json(atEdge))["reason"]).toBe("replay");

    h.clock.now += 1;
    const pastEdge = await h.send(late);
    expect((await json(pastEdge))["reason"]).toBe("clock_skew");
    expect(h.d1.query("SELECT * FROM crews")).toHaveLength(1);
  });

  it("rejects a captured request replayed against another route, method, or query", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const captured = await alice.signed("GET", `/v1/crews/${crew}/members`);
    expect((await h.send(captured.clone())).status).toBe(200);
    for (const [method, path] of [
      ["GET", `/v1/crews/${crew}/members?x=1`],
      ["POST", `/v1/crews/${crew}/invites`],
      ["GET", `/v1/crews/${h.app.randomUuid()}/members`],
    ] as const) {
      const headers = new Headers(captured.headers);
      const res = await h.send(new Request(`https://worker.test${path}`, { method, headers }));
      expect(res.status, `${method} ${path}`).toBe(401);
      expect((await json(res))["reason"]).toBe("bad_signature");
    }
  });
});

describe("authenticate", () => {
  it("does not consume a replay slot for a request that failed any earlier check", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    const deps = { db: h.app.db, now: () => h.clock.now };
    const stale = await alice.signed("GET", "/v1/crews/x/members", undefined, { timestamp: h.clock.now - MAX_SKEW_MS - 10 });
    await expect(authenticate(deps, stale, new Uint8Array(0))).rejects.toBeInstanceOf(HttpError);
    expect(h.d1.query("SELECT * FROM seen_signatures")).toHaveLength(0);
  });
});

// ---------------------------------------------------------------------------------------------
// Authorization: no route may be usable by a device outside the crew
// ---------------------------------------------------------------------------------------------

interface World {
  h: Harness;
  alice: TestDevice;
  bob: TestDevice;
  mallory: TestDevice;
  eve: TestDevice;
  crew: string;
  otherCrew: string;
  clip: string;
}

/** Alice and Bob share a crew with a clip; Mallory has her own crew; Eve belongs to none. */
async function world(): Promise<World> {
  const h = createHarness();
  const alice = await h.device(1, "Alice");
  const bob = await h.device(2, "Bob");
  const mallory = await h.device(3, "Mallory");
  const eve = await h.device(4, "Eve");
  const crew = await newCrew(alice);
  await joinCrew(alice, bob, crew);
  const otherCrew = await newCrew(mallory);
  await eve.register();
  const clip = h.app.randomUuid();
  expect((await alice.call("POST", "/v1/clips", { clip_id: clip, crew_id: crew })).status).toBe(201);
  return { h, alice, bob, mallory, eve, crew, otherCrew, clip };
}

describe("authorization of every crew and clip route (IDOR)", () => {
  it("refuses outsiders and members of other crews, and changes nothing", async () => {
    const { h, alice, mallory, eve, crew, clip } = await world();
    const key = `clips/${crew}/${clip}/${alice.id}/full/000000.bin`;
    h.store.put(key);
    const snapshot = () =>
      JSON.stringify([
        h.d1.query("SELECT * FROM invites"),
        h.d1.query("SELECT * FROM clips ORDER BY clip_id"),
        h.d1.query("SELECT * FROM clip_chunks"),
        h.d1.query("SELECT * FROM usage"),
        h.d1.query("SELECT * FROM crew_members ORDER BY crew_id, device_id"),
        [...h.store.keys],
      ]);
    const before = snapshot();

    for (const intruder of [mallory, eve]) {
      const attempts: Array<[string, string, unknown]> = [
        ["POST", `/v1/crews/${crew}/invites`, undefined],
        ["GET", `/v1/crews/${crew}/members`, undefined],
        ["POST", "/v1/clips", { clip_id: h.app.randomUuid(), crew_id: crew }],
        ["POST", `/v1/clips/${clip}/upload-urls`, { pov: intruder.id, quality: "full", indices: [0], sizes: [1000] }],
        ["POST", `/v1/clips/${clip}/upload-urls`, { pov: intruder.id, quality: "full", indices: [], sizes: [], manifest: true, manifest_size: 10 }],
        ["POST", `/v1/clips/${clip}/download-urls`, { pov: alice.id, quality: "full", indices: [0] }],
        ["POST", `/v1/clips/${clip}/download-urls`, { pov: alice.id, quality: "full", indices: [], manifest: true }],
        ["DELETE", `/v1/clips/${clip}`, undefined],
      ];
      for (const [method, path, body] of attempts) {
        const res = await intruder.call(method, path, body);
        expect(res.status, `${intruder.displayName}: ${method} ${path}`).toBe(403);
        expect((await json(res))["error"]).toBe("not_a_member");
      }
    }
    expect(snapshot()).toBe(before);
  });

  it("never hands out URLs, even for a pov the caller forged, before checking membership", async () => {
    const { h, alice, mallory, clip } = await world();
    let presigned = 0;
    const real = h.app.presigner.presign.bind(h.app.presigner);
    h.app.presigner.presign = (...args) => {
      presigned += 1;
      return real(...args);
    };
    const res = await mallory.call("POST", `/v1/clips/${clip}/upload-urls`, {
      pov: alice.id,
      quality: "full",
      indices: [0],
      sizes: [10],
    });
    expect(res.status).toBe(403);
    expect(presigned).toBe(0);
  });

  it("does not let a member of crew B register or use a clip of crew A under crew B's id", async () => {
    const { h, mallory, otherCrew, clip } = await world();
    const taken = await mallory.call("POST", "/v1/clips", { clip_id: clip, crew_id: otherCrew });
    expect(taken.status).toBe(409);
    // The URLs she can request for her own crew never point into crew A's prefix.
    const own = h.app.randomUuid();
    await mallory.call("POST", "/v1/clips", { clip_id: own, crew_id: otherCrew });
    const res = await mallory.call("POST", `/v1/clips/${own}/upload-urls`, {
      pov: mallory.id,
      quality: "full",
      indices: [0],
      sizes: [10],
    });
    const body = await json<{ chunks: Array<{ key: string }> }>(res);
    expect(body.chunks[0]?.key.startsWith(clipPrefix(otherCrew, own))).toBe(true);
  });

  it("answers an unknown clip with 404 and a malformed id with 400 before anything else", async () => {
    const { alice, eve } = await world();
    const ghost = "00000000-0000-0000-0000-00000000dead";
    expect((await alice.call("DELETE", `/v1/clips/${ghost}`)).status).toBe(404);
    expect((await eve.call("DELETE", `/v1/clips/${ghost}`)).status).toBe(404);
    expect((await alice.call("DELETE", "/v1/clips/..%2F..%2Fcrews")).status).toBe(400);
  });
});

// ---------------------------------------------------------------------------------------------
// Key injection and hostile input
// ---------------------------------------------------------------------------------------------

const NASTY = [
  "../",
  "..",
  "../../etc/passwd",
  "%2e%2e%2f",
  "a/b",
  "a\\b",
  "\n",
  "\u0000",
  "' OR '1'='1",
  "'); DROP TABLE clips;--",
  "${1+1}",
  "full/../proxy",
  "FULL",
  "proxy\n",
  "clips/",
  "x".repeat(600),
  "‮",
  "ı",
];

describe("object key injection", () => {
  it("rejects hostile ids, povs and qualities with 400 and signs nothing", async () => {
    const { h, alice, crew, clip } = await world();
    let presigned = 0;
    const real = h.app.presigner.presign.bind(h.app.presigner);
    h.app.presigner.presign = (...args) => {
      presigned += 1;
      return real(...args);
    };
    const random = seededRandom(99);
    const pick = () => NASTY[Math.floor(random() * NASTY.length)] ?? "";
    for (let i = 0; i < 120; i += 1) {
      const povValid = random() < 0.3;
      const body = {
        pov: povValid ? alice.id : `${pick()}${random() < 0.5 ? alice.id : ""}${pick()}`,
        quality: povValid ? pick() : random() < 0.5 ? "full" : pick(),
        indices: random() < 0.5 ? [0] : [pick()],
        sizes: [10],
      };
      for (const path of [`/v1/clips/${clip}/upload-urls`, `/v1/clips/${clip}/download-urls`]) {
        const res = await alice.call("POST", path, body);
        expect(res.status, JSON.stringify(body)).toBeGreaterThanOrEqual(400);
        expect(res.status).toBeLessThan(500);
      }
      const reg = await alice.call("POST", "/v1/clips", { clip_id: pick(), crew_id: pick() });
      expect(reg.status).toBe(400);
      const cropped = await alice.call("POST", `/v1/clips/${encodeURIComponent(`${clip}${pick()}`)}/upload-urls`, body);
      expect(cropped.status).toBe(400);
    }
    expect(presigned).toBe(0);
    expect(crew).toBeTruthy();
  });

  it("rejects an id that only looks like a uuid because of a trailing newline", async () => {
    const { alice, clip } = await world();
    const withNewline = await alice.call("POST", `/v1/clips/${clip}%0A/upload-urls`, {
      pov: alice.id,
      quality: "full",
      indices: [0],
      sizes: [10],
    });
    expect(withNewline.status).toBe(400);
    const body = await alice.call("POST", `/v1/clips/${clip}/upload-urls`, {
      pov: `${alice.id}\n`,
      quality: "full",
      indices: [0],
      sizes: [10],
    });
    expect(body.status).toBe(400);
  });

  it("only ever issues keys that pass the structural key rules and sit under the clip prefix", async () => {
    const { alice, crew, clip } = await world();
    const res = await alice.call("POST", `/v1/clips/${clip}/upload-urls`, {
      pov: alice.id,
      quality: "proxy",
      indices: [0, 999_999],
      sizes: [10, 10],
      manifest: true,
      manifest_size: 10,
    });
    const body = await json<{ chunks: Array<{ key: string }>; manifest: { key: string } }>(res);
    for (const { key } of [...body.chunks, body.manifest]) {
      expect(validateObjectKey(key)).toBeNull();
      expect(key.startsWith(clipPrefix(crew, clip))).toBe(true);
    }
    expect(body.chunks[1]?.key.endsWith("/proxy/999999.bin")).toBe(true);
    // One past the last index that fits the six-digit key format is refused.
    const over = await alice.call("POST", `/v1/clips/${clip}/upload-urls`, {
      pov: alice.id,
      quality: "proxy",
      indices: [1_000_000],
      sizes: [10],
    });
    expect(over.status).toBe(400);
  });
});

describe("SQL injection", () => {
  it("stores hostile text verbatim and leaves every table intact", async () => {
    const h = createHarness();
    const alice = await h.device(1, "'); DROP TABLE devices;--");
    await alice.register();
    const evil = "Robert'); DROP TABLE crews;-- \" OR 1=1";
    const res = await alice.call("POST", "/v1/crews", { name: evil });
    expect(res.status).toBe(201);
    const { crew_id } = await json<{ crew_id: string }>(res);
    expect(h.d1.query<{ name: string }>("SELECT name FROM crews WHERE crew_id = ?", crew_id)[0]?.name).toBe(evil);
    expect(h.d1.query<{ display_name: string }>("SELECT display_name FROM devices")[0]?.display_name).toBe(
      "'); DROP TABLE devices;--",
    );
    for (const code of ["' OR '1'='1", "A' OR 1=1--", "ABCDEFGHIJ'; --"]) {
      expect((await alice.call("POST", "/v1/crews/join", { code })).status).toBe(400);
    }
    const members = await alice.call("GET", `/v1/crews/${crew_id}/members?x=%27%20OR%201%3D1`);
    expect(members.status).toBe(200);
    expect(h.d1.query("SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('crews', 'devices')")).toHaveLength(2);
  });

  it("builds no SQL from request data: the only interpolated names are constant column lists", () => {
    const source = readFileSync(join(import.meta.dirname, "../src/db.ts"), "utf8");
    const interpolations = [...source.matchAll(/\$\{([^}]*)\}/g)].map((match) => match[1]);
    // `${DEVICE_COLUMNS}` / `${CLIP_COLUMNS}` and the template of `joinAttemptsScope`-style helpers.
    const allowed = new Set(["DEVICE_COLUMNS", "CLIP_COLUMNS", "deviceId"]);
    for (const expression of interpolations) {
      expect(allowed.has(expression ?? ""), `unexpected \${${expression}} in src/db.ts`).toBe(true);
    }
    // And every statement goes through bind(): no string concatenation into prepare().
    expect(source).not.toMatch(/prepare\([^)]*\+/);
  });
});

// ---------------------------------------------------------------------------------------------
// Body limits and information leaks
// ---------------------------------------------------------------------------------------------

function streamOf(...sizes: number[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const size of sizes) {
        controller.enqueue(new Uint8Array(size).fill(0x61));
      }
      controller.close();
    },
  });
}

function fakeRequest(headers: Record<string, string>, body: ReadableStream<Uint8Array> | null): Request {
  return { headers: new Headers(headers), body } as unknown as Request;
}

describe("readBody", () => {
  it("refuses a declared length above the limit without reading", async () => {
    const request = fakeRequest({ "content-length": String(MAX_BODY_BYTES + 1) }, streamOf(1));
    await expect(readBody(request, MAX_BODY_BYTES)).rejects.toMatchObject({ status: 413 });
  });

  it("counts the stream when the header is absent or lies", async () => {
    const variants: Array<Record<string, string>> = [{}, { "content-length": "10" }, { "content-length": "abc" }];
    for (const headers of variants) {
      const request = fakeRequest(headers, streamOf(8 * 1024, 8 * 1024, 1));
      await expect(readBody(request, MAX_BODY_BYTES), JSON.stringify(headers)).rejects.toMatchObject({
        status: 413,
        code: "payload_too_large",
      });
    }
  });

  it("stops reading as soon as the limit is crossed", async () => {
    let pulled = 0;
    const endless = new ReadableStream<Uint8Array>({
      pull(controller) {
        pulled += 1;
        controller.enqueue(new Uint8Array(4096));
      },
    });
    await expect(readBody(fakeRequest({}, endless), MAX_BODY_BYTES)).rejects.toMatchObject({ status: 413 });
    expect(pulled).toBeLessThan(10);
  });

  it("accepts a body of exactly the limit and an empty body", async () => {
    const exact = await readBody(fakeRequest({}, streamOf(MAX_BODY_BYTES)), MAX_BODY_BYTES);
    expect(exact.byteLength).toBe(MAX_BODY_BYTES);
    expect((await readBody(fakeRequest({}, null), MAX_BODY_BYTES)).byteLength).toBe(0);
  });
});

describe("information leaks", () => {
  it("keeps internal error details out of responses for every failure mode", async () => {
    const { h, alice, clip } = await world();
    const secret = "SECRET-r2-key-and-sql-text";
    h.app.db.getClip = () => Promise.reject(new Error(`D1_ERROR: ${secret}`));
    const errors: string[] = [];
    const original = console.error;
    console.error = (line: string) => void errors.push(line);
    try {
      const res = await alice.call("DELETE", `/v1/clips/${clip}`);
      expect(res.status).toBe(500);
      const text = await res.text();
      expect(text).not.toContain(secret);
      expect(JSON.parse(text)).toEqual({ error: "internal_error", message: "internal server error" });
    } finally {
      console.error = original;
    }
    // The operator still gets the message, but never a stack trace.
    expect(errors.join("\n")).toContain(secret);
    expect(errors.join("\n")).not.toMatch(/\bat \S+ \(/);
  });

  it("never echoes the submitted signature, key or body in a 401", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    const request = await alice.signed("POST", "/v1/crews", { name: "Squad" }, { signedBody: "other" });
    const signature = request.headers.get("x-dc-signature") ?? "";
    const text = await (await h.send(request)).text();
    expect(text).not.toContain(signature);
    expect(text).not.toContain(alice.publicKeyB64);
    expect(text).not.toContain("Squad");
  });

  it("marks every response as uncacheable and non-sniffable", async () => {
    const { alice, clip } = await world();
    for (const res of [
      await alice.call("POST", `/v1/clips/${clip}/download-urls`, { pov: alice.id, quality: "full", indices: [0] }),
      await alice.call("GET", "/v1/nope"),
    ]) {
      expect(res.headers.get("cache-control")).toBe("no-store");
      expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    }
  });
});
