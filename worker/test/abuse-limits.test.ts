/**
 * Regression tests for the quota and abuse limits added by the security review:
 * unmetered manifests, tiny-object floods, signed upload sizes, open-registration (sybil) caps,
 * invite brute force and unbounded crew, invite and clip creation.
 */

import { describe, expect, it } from "vitest";
import worker from "../src/index.js";
import type { Env } from "../src/env.js";
import {
  clipRegistrationsScope,
  createD1Db,
  GLOBAL_BYTES_SCOPE,
  joinAttemptsScope,
  NEW_DEVICES_SCOPE,
} from "../src/db.js";
import {
  DEFAULT_LIMITS,
  MAX_CLIP_BYTES,
  MAX_MANIFEST_BYTES,
  MIN_CHARGED_BYTES,
  secondsUntilNextUtcDay,
  utcDay,
} from "../src/quota.js";
import { createHarness, joinCrew, json, newCrew, ORIGIN, T0 } from "./helpers/harness.js";
import type { Harness, TestDevice } from "./helpers/harness.js";
import { SqliteD1 } from "./helpers/sqlite-d1.js";

const KIB = 1024;
const MIB = 1024 * KIB;
const GIB = 1024 * MIB;
const DAY_MS = 24 * 60 * 60 * 1000;

async function clipFor(h: Harness, owner: TestDevice, crew: string): Promise<string> {
  const id = h.app.randomUuid();
  const res = await owner.call("POST", "/v1/clips", { clip_id: id, crew_id: crew });
  expect(res.status).toBe(201);
  return id;
}

function clipBytes(h: Harness, clip: string): number {
  return h.d1.query<{ bytes: number }>("SELECT bytes FROM clips WHERE clip_id = ?", clip)[0]?.bytes ?? -1;
}

function counter(h: Harness, scope: string, day = utcDay(h.clock.now)): number | undefined {
  return h.d1.query<{ n: number }>("SELECT n FROM counters WHERE scope = ? AND day = ?", scope, day)[0]?.n;
}

function upload(owner: TestDevice, clip: string, body: Record<string, unknown>): Promise<Response> {
  return owner.call("POST", `/v1/clips/${clip}/upload-urls`, { pov: owner.id, quality: "full", ...body });
}

// ---------------------------------------------------------------------------------------------
// The manifest is metered like a chunk
// ---------------------------------------------------------------------------------------------

describe("manifest uploads", () => {
  async function setup() {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    return { h, alice, crew, clip: await clipFor(h, alice, crew) };
  }

  it("need an exact manifest_size, so no upload URL is ever minted without a size bound", async () => {
    const { h, alice, clip } = await setup();
    const missing = await upload(alice, clip, { indices: [], sizes: [], manifest: true });
    expect(missing.status).toBe(400);
    expect(await json(missing)).toMatchObject({ error: "invalid_request" });
    for (const manifest_size of [0, -5, 2.5, "10", null, 1e300]) {
      expect((await upload(alice, clip, { indices: [], sizes: [], manifest: true, manifest_size })).status).toBe(400);
    }
    expect(clipBytes(h, clip)).toBe(0);
    expect(h.d1.query("SELECT * FROM usage")).toHaveLength(0);
  });

  it("are capped at 1 MiB and signed with their exact size", async () => {
    const { alice, clip } = await setup();
    const big = await upload(alice, clip, { indices: [], sizes: [], manifest: true, manifest_size: MAX_MANIFEST_BYTES + 1 });
    expect(big.status).toBe(413);
    expect(await json(big)).toMatchObject({ error: "manifest_too_large" });
    const ok = await upload(alice, clip, { indices: [], sizes: [], manifest: true, manifest_size: MAX_MANIFEST_BYTES });
    expect(ok.status).toBe(200);
    const body = await json<{ manifest: { url: string; content_length: number } }>(ok);
    expect(body.manifest.content_length).toBe(MAX_MANIFEST_BYTES);
    expect(new URL(body.manifest.url).searchParams.get("X-Amz-SignedHeaders")).toBe("content-length;content-type;host");
  });

  it("count against the clip and the daily quota, once per (pov, quality)", async () => {
    const { h, alice, clip } = await setup();
    const request = { indices: [], sizes: [], manifest: true, manifest_size: 600 * KIB };
    expect((await upload(alice, clip, request)).status).toBe(200);
    expect(clipBytes(h, clip)).toBe(600 * KIB);
    expect(h.d1.query("SELECT bytes FROM usage")).toEqual([{ bytes: 600 * KIB }]);
    // Asking again, or for a smaller manifest, is free ...
    expect((await upload(alice, clip, request)).status).toBe(200);
    expect((await upload(alice, clip, { ...request, manifest_size: 10 })).status).toBe(200);
    expect(clipBytes(h, clip)).toBe(600 * KIB);
    // ... a bigger one pays the difference, and the other quality is a separate object.
    expect((await upload(alice, clip, { ...request, manifest_size: 700 * KIB })).status).toBe(200);
    expect(clipBytes(h, clip)).toBe(700 * KIB);
    expect((await upload(alice, clip, { ...request, quality: "proxy" })).status).toBe(200);
    expect(clipBytes(h, clip)).toBe(1300 * KIB);
    expect(
      h.d1.query<{ quality: string; idx: number; bytes: number }>("SELECT quality, idx, bytes FROM clip_chunks ORDER BY quality"),
    ).toEqual([
      { quality: "full", idx: -1, bytes: 700 * KIB },
      { quality: "proxy", idx: -1, bytes: 600 * KIB },
    ]);
  });

  it("cannot be used to push a clip past its 1.5 GiB limit", async () => {
    const { h, alice, clip } = await setup();
    h.d1.database.prepare("UPDATE clips SET bytes = ?").run(MAX_CLIP_BYTES - 10 * KIB);
    const res = await upload(alice, clip, { indices: [], sizes: [], manifest: true, manifest_size: 10 });
    expect(res.status).toBe(413);
    expect(await json(res)).toMatchObject({ error: "clip_quota_exceeded" });
    expect(clipBytes(h, clip)).toBe(MAX_CLIP_BYTES - 10 * KIB);
  });
});

// ---------------------------------------------------------------------------------------------
// Sizes are bounded, floored and signed
// ---------------------------------------------------------------------------------------------

describe("announced sizes", () => {
  async function setup() {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    return { h, alice, crew, clip: await clipFor(h, alice, crew) };
  }

  it("sign the exact Content-Length of every chunk into its URL", async () => {
    const { alice, clip } = await setup();
    const res = await upload(alice, clip, { indices: [4, 5], sizes: [4 * MIB, 123_456] });
    const body = await json<{ chunks: Array<{ url: string; content_length: number }> }>(res);
    expect(body.chunks.map((c) => c.content_length)).toEqual([4 * MIB, 123_456]);
    for (const chunk of body.chunks) {
      expect(new URL(chunk.url).searchParams.get("X-Amz-SignedHeaders")).toBe("content-length;content-type;host");
    }
    // Different sizes produce different signatures, so a URL cannot be reused for a bigger body.
    const [a, b] = body.chunks.map((c) => new URL(c.url).searchParams.get("X-Amz-Signature"));
    expect(a).not.toBe(b);
  });

  it("never reveal a size on download URLs", async () => {
    const { alice, clip } = await setup();
    const res = await alice.call("POST", `/v1/clips/${clip}/download-urls`, { pov: alice.id, quality: "full", indices: [0] });
    const body = await json<{ chunks: Array<Record<string, unknown>> }>(res);
    expect(body.chunks[0]).not.toHaveProperty("content_length");
  });

  it("are charged at least 256 KiB per object", async () => {
    const { h, alice, clip } = await setup();
    const indices = Array.from({ length: 64 }, (_, i) => i);
    const res = await upload(alice, clip, { indices, sizes: indices.map(() => 1) });
    expect(res.status).toBe(200);
    expect(clipBytes(h, clip)).toBe(64 * MIN_CHARGED_BYTES);
    // Retrying with the same tiny sizes stays free; growing to the floor costs nothing either.
    await upload(alice, clip, { indices, sizes: indices.map(() => MIN_CHARGED_BYTES) });
    expect(clipBytes(h, clip)).toBe(64 * MIN_CHARGED_BYTES);
  });

  it("limit a clip to 6144 objects however small they are", async () => {
    const { h, alice, clip } = await setup();
    h.d1.database.prepare("UPDATE clips SET bytes = ?").run(MAX_CLIP_BYTES - MIN_CHARGED_BYTES);
    expect((await upload(alice, clip, { indices: [0], sizes: [1] })).status).toBe(200);
    expect(clipBytes(h, clip)).toBe(MAX_CLIP_BYTES);
    const next = await upload(alice, clip, { indices: [1], sizes: [1] });
    expect(next.status).toBe(413);
    expect(await json(next)).toMatchObject({ error: "clip_quota_exceeded" });
  });

  it("reject hostile numbers without reserving anything", async () => {
    const { h, alice, clip } = await setup();
    const hostile: unknown[] = [
      Number.MAX_SAFE_INTEGER,
      2 ** 53,
      2 ** 63,
      1e21,
      64 * MIB + 1,
      -1,
      0,
      0.5,
      "1000",
      null,
      true,
      [1],
    ];
    for (const size of hostile) {
      const res = await upload(alice, clip, { indices: [0], sizes: [size] });
      expect([400, 413], `size ${JSON.stringify(size)}`).toContain(res.status);
    }
    // 1e400 is not valid JSON-safe: it parses to Infinity and must not slip through either.
    const raw = '{"pov":"' + alice.id + '","quality":"full","indices":[0],"sizes":[1e400]}';
    expect((await alice.call("POST", `/v1/clips/${clip}/upload-urls`, raw)).status).toBe(400);
    expect(clipBytes(h, clip)).toBe(0);
    expect(h.d1.query("SELECT * FROM usage")).toHaveLength(0);
    expect(h.d1.query("SELECT * FROM clip_chunks")).toHaveLength(0);
    expect(counter(h, GLOBAL_BYTES_SCOPE)).toBeUndefined();
  });

  it("cannot be inflated after the fact: a retry with a larger size is charged the growth", async () => {
    const { h, alice, clip } = await setup();
    await upload(alice, clip, { indices: [0], sizes: [MIB] });
    await upload(alice, clip, { indices: [0], sizes: [3 * MIB] });
    expect(clipBytes(h, clip)).toBe(3 * MIB);
    await upload(alice, clip, { indices: [0], sizes: [MIB] });
    expect(clipBytes(h, clip)).toBe(3 * MIB);
  });
});

// ---------------------------------------------------------------------------------------------
// Service-wide circuit breakers (registration is open)
// ---------------------------------------------------------------------------------------------

describe("daily registration limit for new devices", () => {
  it("stops the (N+1)th new device of the day, with Retry-After", async () => {
    const h = createHarness({ limits: { newDevicesPerDay: 3 } });
    const devices = await Promise.all([1, 2, 3, 4].map((seed) => h.device(seed)));
    for (const device of devices.slice(0, 3)) {
      await device.register();
    }
    const res = await h.send(
      new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(devices[3]?.registrationBody()) }),
    );
    expect(res.status).toBe(429);
    expect(await json(res)).toMatchObject({ error: "registration_limited" });
    expect(res.headers.get("retry-after")).toBe(String(secondsUntilNextUtcDay(h.clock.now)));
    expect(h.d1.query("SELECT * FROM devices")).toHaveLength(3);
    expect(counter(h, NEW_DEVICES_SCOPE)).toBe(3);
  });

  it("does not count repeats of an existing device or a rejected key conflict", async () => {
    const h = createHarness({ limits: { newDevicesPerDay: 1 } });
    const alice = await h.device(1);
    const mallory = await h.device(2);
    await alice.register();
    await alice.register();
    await alice.register();
    const clash = await h.send(
      new Request(`${ORIGIN}/v1/devices`, {
        method: "POST",
        body: JSON.stringify({ ...mallory.registrationBody(), device_id: alice.id }),
      }),
    );
    expect(clash.status).toBe(409);
    const malformed = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: "{}" }));
    expect(malformed.status).toBe(400);
    expect(counter(h, NEW_DEVICES_SCOPE)).toBe(1);
  });

  it("does not let existing devices keep working be affected, and resets on the next UTC day", async () => {
    const h = createHarness({ limits: { newDevicesPerDay: 1 } });
    const alice = await h.device(1);
    const bob = await h.device(2);
    await alice.register();
    const blocked = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(bob.registrationBody()) }));
    expect(blocked.status).toBe(429);
    expect((await alice.call("POST", "/v1/crews", { name: "Still works" })).status).toBe(201);
    h.clock.now = T0 + DAY_MS;
    await bob.register();
    expect(h.d1.query("SELECT * FROM devices")).toHaveLength(2);
  });

  it("closes registration entirely when set to 0 and gives the slot back after a lost race", async () => {
    const closed = createHarness({ limits: { newDevicesPerDay: 0 } });
    const alice = await closed.device(1);
    const res = await closed.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(alice.registrationBody()) }));
    expect(res.status).toBe(429);

    const h = createHarness({ limits: { newDevicesPerDay: 5 } });
    const bob = await h.device(2);
    const real = h.app.db.getDevice.bind(h.app.db);
    let first = true;
    h.app.db.getDevice = async (id) => {
      if (first) {
        first = false;
        return null; // pretend the device is new ...
      }
      return real(id);
    };
    await h.app.db.insertDeviceIfAbsent({
      device_id: bob.id,
      public_key_b64: bob.publicKeyB64,
      display_name: "Bob",
      created_at: 1,
    }); // ... while a concurrent request already inserted it
    const raced = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(bob.registrationBody()) }));
    expect(raced.status).toBe(200);
    expect(counter(h, NEW_DEVICES_SCOPE) ?? 0).toBe(0);
  });
});

describe("service-wide daily upload budget", () => {
  async function setup(globalDailyBytes: number) {
    const h = createHarness({ limits: { globalDailyBytes } });
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    return { h, alice, bob, crew, clip: await clipFor(h, alice, crew) };
  }

  it("refuses uploads once all devices together reached it, and leaves no reservation behind", async () => {
    const { h, alice, bob, clip } = await setup(10 * MIB);
    expect((await upload(alice, clip, { indices: [0], sizes: [6 * MIB] })).status).toBe(200);
    const refused = await upload(bob, clip, { indices: [0], sizes: [5 * MIB] });
    expect(refused.status).toBe(429);
    expect(await json(refused)).toMatchObject({ error: "global_quota_exceeded" });
    expect(refused.headers.get("retry-after")).toBe(String(secondsUntilNextUtcDay(h.clock.now)));
    // Nothing of the refused request stays reserved: not on the clip, the device, or the budget.
    expect(clipBytes(h, clip)).toBe(6 * MIB);
    expect(h.d1.query("SELECT device_id, bytes FROM usage WHERE bytes > 0")).toEqual([
      { device_id: alice.id, bytes: 6 * MIB },
    ]);
    expect(counter(h, GLOBAL_BYTES_SCOPE)).toBe(6 * MIB);
    // What still fits is accepted, up to the byte.
    expect((await upload(bob, clip, { indices: [0], sizes: [4 * MIB] })).status).toBe(200);
    expect(counter(h, GLOBAL_BYTES_SCOPE)).toBe(10 * MIB);
    expect((await upload(bob, clip, { indices: [1], sizes: [1] })).status).toBe(429);
  });

  it("restarts every UTC day", async () => {
    const { h, alice, clip } = await setup(MIB);
    expect((await upload(alice, clip, { indices: [0], sizes: [MIB] })).status).toBe(200);
    expect((await upload(alice, clip, { indices: [1], sizes: [MIB] })).status).toBe(429);
    h.clock.now = T0 + DAY_MS;
    expect((await upload(alice, clip, { indices: [1], sizes: [MIB] })).status).toBe(200);
  });

  it("has a default far above the per-device quota", () => {
    expect(DEFAULT_LIMITS.globalDailyBytes).toBeGreaterThan(10 * GIB);
  });

  it("keeps counters atomic under concurrency", async () => {
    const h = createHarness();
    const db = createD1Db(h.d1);
    const results = await Promise.all(
      Array.from({ length: 20 }, () => db.reserveCounter("s", "d", 1, 7)),
    );
    expect(results.filter(Boolean)).toHaveLength(7);
    expect(counter(h, "s", "d")).toBe(7);
    expect(await db.reserveCounter("s", "d", 8, 7)).toBe(false);
    expect(await db.reserveCounter("fresh", "d", 8, 7)).toBe(false);
    expect(counter(h, "fresh", "d")).toBeUndefined();
    expect(await db.reserveCounter("zero", "d", 1, 0)).toBe(false);
  });
});

// ---------------------------------------------------------------------------------------------
// Invite brute force
// ---------------------------------------------------------------------------------------------

describe("invite code guessing", () => {
  async function setup(maxJoinAttemptsPerDay = 5) {
    const h = createHarness({ limits: { maxJoinAttemptsPerDay } });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const invite = await json<{ code: string }>(await alice.call("POST", `/v1/crews/${crew}/invites`));
    const guesser = await h.device(2);
    await guesser.register();
    return { h, alice, crew, code: invite.code, guesser };
  }

  const wrong = (i: number) => `AAAAAAAA${String(2 + (i % 8))}${String(2 + Math.floor(i / 8))}`;

  it("locks a device out after N failed attempts a day, even for the right code", async () => {
    const { h, code, guesser } = await setup(5);
    for (let i = 0; i < 5; i += 1) {
      const res = await guesser.call("POST", "/v1/crews/join", { code: wrong(i) });
      expect(res.status).toBe(404);
    }
    const locked = await guesser.call("POST", "/v1/crews/join", { code });
    expect(locked.status).toBe(429);
    expect(await json(locked)).toMatchObject({ error: "too_many_attempts" });
    expect(locked.headers.get("retry-after")).toBe(String(secondsUntilNextUtcDay(h.clock.now)));
    expect(h.d1.query("SELECT uses_left FROM invites")).toEqual([{ uses_left: 5 }]);
    expect(h.d1.query("SELECT * FROM crew_members WHERE device_id = ?", guesser.id)).toHaveLength(0);

    h.clock.now = T0 + DAY_MS;
    expect((await guesser.call("POST", "/v1/crews/join", { code })).status).toBe(200);
  });

  it("does not let parallel guesses slip past the limit", async () => {
    const { h, guesser } = await setup(5);
    const requests = await Promise.all(Array.from({ length: 25 }, (_, i) => guesser.signed("POST", "/v1/crews/join", { code: wrong(i) })));
    const lookups: string[] = [];
    const real = h.app.db.getInvite.bind(h.app.db);
    h.app.db.getInvite = (code) => {
      lookups.push(code);
      return real(code);
    };
    const statuses = (await Promise.all(requests.map((r) => h.send(r)))).map((r) => r.status);
    expect(statuses.filter((s) => s === 404)).toHaveLength(5);
    expect(statuses.filter((s) => s === 429)).toHaveLength(20);
    expect(lookups).toHaveLength(5);
  });

  it("does not count successful joins, repeated joins, or malformed codes", async () => {
    const { h, code, guesser, crew } = await setup(2);
    expect((await guesser.call("POST", "/v1/crews/join", { code })).status).toBe(200);
    for (let i = 0; i < 6; i += 1) {
      expect((await guesser.call("POST", "/v1/crews/join", { code })).status).toBe(200);
    }
    for (let i = 0; i < 6; i += 1) {
      expect((await guesser.call("POST", "/v1/crews/join", { code: "bad" })).status).toBe(400);
    }
    expect(counter(h, joinAttemptsScope(guesser.id)) ?? 0).toBe(0);
    expect(h.d1.query("SELECT uses_left FROM invites")).toEqual([{ uses_left: 4 }]);
    expect(crew).toBeTruthy();
  });

  it("limits each device on its own", async () => {
    const { h, code, guesser } = await setup(2);
    const other = await h.device(3);
    await other.register();
    for (let i = 0; i < 2; i += 1) {
      await guesser.call("POST", "/v1/crews/join", { code: wrong(i) });
    }
    expect((await guesser.call("POST", "/v1/crews/join", { code })).status).toBe(429);
    expect((await other.call("POST", "/v1/crews/join", { code })).status).toBe(200);
  });
});

// ---------------------------------------------------------------------------------------------
// Unbounded creation of crews, invites and clips
// ---------------------------------------------------------------------------------------------

describe("crew limit", () => {
  it("lets a device create only N crews and leaves no half-created crew behind", async () => {
    const h = createHarness({ limits: { maxCrewsPerDevice: 3 } });
    const alice = await h.device(1);
    const bob = await h.device(2);
    await alice.register();
    await bob.register();
    for (let i = 0; i < 3; i += 1) {
      expect((await alice.call("POST", "/v1/crews", { name: `Crew ${i}` })).status).toBe(201);
    }
    const refused = await alice.call("POST", "/v1/crews", { name: "One too many" });
    expect(refused.status).toBe(409);
    expect(await json(refused)).toMatchObject({ error: "crew_limit" });
    expect(h.d1.query("SELECT * FROM crews")).toHaveLength(3);
    expect(h.d1.query("SELECT * FROM crew_members")).toHaveLength(3);
    // The limit is per creator: Bob can still create his own.
    expect((await bob.call("POST", "/v1/crews", { name: "Bob's" })).status).toBe(201);
  });

  it("holds under concurrency", async () => {
    const h = createHarness({ limits: { maxCrewsPerDevice: 2 } });
    const alice = await h.device(1);
    await alice.register();
    const requests = await Promise.all(Array.from({ length: 6 }, () => alice.signed("POST", "/v1/crews", { name: "Race" })));
    const statuses = (await Promise.all(requests.map((r) => h.send(r)))).map((r) => r.status).sort();
    expect(statuses).toEqual([201, 201, 409, 409, 409, 409]);
    expect(h.d1.query("SELECT * FROM crews")).toHaveLength(2);
    expect(h.d1.query("SELECT * FROM crew_members")).toHaveLength(2);
  });

  it("does not count crews a device merely joined", async () => {
    const h = createHarness({ limits: { maxCrewsPerDevice: 1 } });
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    expect((await bob.call("POST", "/v1/crews", { name: "Mine" })).status).toBe(201);
  });
});

describe("active invite limit", () => {
  it("allows N valid invites per crew; spent, expired and other crews' invites do not count", async () => {
    const h = createHarness({ limits: { maxActiveInvitesPerCrew: 2 } });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const other = await json<{ crew_id: string }>(await alice.call("POST", "/v1/crews", { name: "Other" }));
    const invite = () => alice.call("POST", `/v1/crews/${crew}/invites`);

    const first = await json<{ code: string }>(await invite());
    expect((await invite()).status).toBe(201);
    const refused = await invite();
    expect(refused.status).toBe(409);
    expect(await json(refused)).toMatchObject({ error: "invite_limit" });
    expect((await alice.call("POST", `/v1/crews/${other.crew_id}/invites`)).status).toBe(201);

    // A used-up invite frees a slot ...
    h.d1.database.prepare("UPDATE invites SET uses_left = 0 WHERE code = ?").run(first.code);
    expect((await invite()).status).toBe(201);
    expect((await invite()).status).toBe(409);
    // ... and so does expiry.
    h.clock.now += 25 * 60 * 60 * 1000;
    expect((await invite()).status).toBe(201);
  });
});

describe("daily clip registration limit", () => {
  it("counts only new clips, per device and per day", async () => {
    const h = createHarness({ limits: { maxClipsPerDevicePerDay: 3 } });
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    const ids = [1, 2, 3].map(() => h.app.randomUuid());
    for (const id of ids) {
      expect((await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew })).status).toBe(201);
    }
    // Repeating a registration is idempotent and free.
    expect((await alice.call("POST", "/v1/clips", { clip_id: ids[0], crew_id: crew })).status).toBe(200);
    const refused = await alice.call("POST", "/v1/clips", { clip_id: h.app.randomUuid(), crew_id: crew });
    expect(refused.status).toBe(429);
    expect(await json(refused)).toMatchObject({ error: "clip_registration_limited" });
    expect(refused.headers.get("retry-after")).toBe(String(secondsUntilNextUtcDay(h.clock.now)));
    expect(counter(h, clipRegistrationsScope(alice.id))).toBe(3);
    expect(h.d1.query("SELECT * FROM clips")).toHaveLength(3);

    expect((await bob.call("POST", "/v1/clips", { clip_id: h.app.randomUuid(), crew_id: crew })).status).toBe(201);
    h.clock.now = T0 + DAY_MS;
    expect((await alice.call("POST", "/v1/clips", { clip_id: h.app.randomUuid(), crew_id: crew })).status).toBe(201);
  });

  it("gives the slot back when another request registered the same clip first", async () => {
    const h = createHarness({ limits: { maxClipsPerDevicePerDay: 5 } });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const id = h.app.randomUuid();
    const real = h.app.db.getClip.bind(h.app.db);
    let first = true;
    h.app.db.getClip = async (clip) => {
      if (first) {
        first = false;
        return null;
      }
      return real(clip);
    };
    await h.app.db.insertClip({ clip_id: id, crew_id: crew, owner: alice.id, created_at: 1, expires_at: T0 + 3_600_000 });
    expect((await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew })).status).toBe(200);
    expect(counter(h, clipRegistrationsScope(alice.id)) ?? 0).toBe(0);
  });
});

// ---------------------------------------------------------------------------------------------
// Wiring through the real entry point
// ---------------------------------------------------------------------------------------------

describe("configuration of the limits through Worker variables", () => {
  it("reads MAX_NEW_DEVICES_PER_DAY and MAX_GLOBAL_DAILY_BYTES in fetch()", async () => {
    const env = {
      ACCOUNT_ID: "0123456789abcdef0123456789abcdef",
      BUCKET_NAME: "duoclip-clips",
      R2_ACCESS_KEY_ID: "AKIAEXAMPLE",
      R2_SECRET_ACCESS_KEY: "secret",
      MAX_NEW_DEVICES_PER_DAY: "1",
      MAX_GLOBAL_DAILY_BYTES: "1",
      CLIPS: { list: () => Promise.resolve({ objects: [], truncated: false }), delete: () => Promise.resolve() },
      DB: new SqliteD1(),
    } as unknown as Env;
    const h = createHarness();
    h.send = (request) => worker.fetch(request as never, env);
    h.clock.now = Date.now();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    const blocked = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(bob.registrationBody()) }));
    expect(blocked.status).toBe(429);
    const clip = h.app.randomUuid();
    await alice.call("POST", "/v1/clips", { clip_id: clip, crew_id: crew });
    const res = await upload(alice, clip, { indices: [0], sizes: [1000] });
    expect(res.status).toBe(429);
    expect(await json(res)).toMatchObject({ error: "global_quota_exceeded" });
  });
});
