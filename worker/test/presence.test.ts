import { describe, expect, it } from "vitest";
import type { PresenceRow } from "../src/db.js";
import { PRESENCE_TTL_MS } from "../src/presence.js";
import { runSweep } from "../src/sweep.js";
import { parsePresence } from "../src/validate.js";
import type { PresenceInput } from "../src/validate.js";
import { createHarness, joinCrew, json, newCrew, ORIGIN } from "./helpers/harness.js";
import type { TestDevice } from "./helpers/harness.js";

const INPUT: PresenceInput = { game: "cs2", active_crew: null, seq: 1, online_since_ms: 10 };
type ApiPresence = PresenceRow & { expires_at: number };

async function announce(device: TestDevice, overrides: Partial<PresenceInput> = {}): Promise<Response> {
  return device.call("POST", "/v1/presence", { ...INPUT, ...overrides });
}

describe("presence validation", () => {
  it("accepts idle, playing, Unicode ids and safe integer boundaries without changing ids", () => {
    expect(parsePresence({ ...INPUT })).toEqual(INPUT);
    expect(parsePresence({ ...INPUT, game: null, seq: 0, online_since_ms: 0 })).toMatchObject({ game: null, seq: 0 });
    expect(parsePresence({ ...INPUT, game: "á".repeat(32), seq: Number.MAX_SAFE_INTEGER,
      online_since_ms: Number.MAX_SAFE_INTEGER })).toMatchObject({ game: "á".repeat(32) });
    expect(parsePresence({ ...INPUT, game: "minecraft-java", unknown: true })).toEqual({ ...INPUT, game: "minecraft-java" });
  });

  it.each(["game", "active_crew", "seq", "online_since_ms"])("requires %s", (field) => {
    const input: Record<string, unknown> = { ...INPUT };
    delete input[field];
    expect(() => parsePresence(input)).toThrow();
  });

  it.each([
    "", " ", "x".repeat(65), "á".repeat(33), "cs2\u0000", "cs2\n", "cs2\u202e", "cs2\u2066",
    "\ud800", 123, true, {}, [],
  ])("rejects invalid game ids: %j", (game) => {
    expect(() => parsePresence({ ...INPUT, game })).toThrow();
  });

  it.each(["crew", "00000000-0000-0000-0000-00000000000A", 1, {}, false])("rejects active_crew: %j", (active_crew) => {
    expect(() => parsePresence({ ...INPUT, active_crew })).toThrow();
  });

  it.each(["seq", "online_since_ms"])("rejects unsafe, negative or nonnumeric %s", (field) => {
    for (const value of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1, NaN, Infinity, "1", null, true, {}]) {
      expect(() => parsePresence({ ...INPUT, [field]: value })).toThrow();
    }
  });
});

describe("authenticated presence routes using real SQL", () => {
  it("returns a complete available-member snapshot ordered by device id", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    expect(await json(await alice.call("GET", `/v1/crews/${crew}/presence`))).toEqual([]);
    const response = await announce(alice, { active_crew: crew });
    const ack = await json<{ ok: boolean; seen_at_ms: number; expires_at: number }>(response);
    expect(response.status).toBe(200);
    expect(ack).toEqual({ ok: true, seen_at_ms: h.clock.now, expires_at: h.clock.now + PRESENCE_TTL_MS });
    await announce(bob, { game: null });
    const listed = await alice.call("GET", `/v1/crews/${crew}/presence`);
    expect(listed.headers.get("cache-control")).toBe("no-store");
    const rows = await json<ApiPresence[]>(listed);
    expect(rows.map((p) => p.device_id)).toEqual([alice.id, bob.id].sort());
    expect(rows.find((p) => p.device_id === alice.id)).toEqual({
      device_id: alice.id, display_name: alice.displayName, ...INPUT, active_crew: crew,
      seen_at_ms: ack.seen_at_ms, expires_at: ack.expires_at,
    });
    expect(rows.find((p) => p.device_id === bob.id)?.game).toBeNull();
  });

  it("requires valid signatures for both routes and rejects replay without updating freshness", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    for (const [method, path] of [["POST", "/v1/presence"], ["GET", `/v1/crews/${crew}/presence`]]) {
      expect((await h.send(new Request(`${ORIGIN}${path}`, { method, ...(method === "POST" ? { body: JSON.stringify(INPUT) } : {}) }))).status).toBe(401);
    }
    const forged = await alice.signed("POST", "/v1/presence", INPUT, { signedBody: "{}" });
    expect((await h.send(forged)).status).toBe(401);
    const request = await alice.signed("POST", "/v1/presence", INPUT);
    expect((await h.send(request.clone())).status).toBe(200);
    const seen = h.clock.now;
    h.clock.now += 10_000;
    const replay = await h.send(request);
    expect(replay.status).toBe(401);
    expect(await json(replay)).toMatchObject({ reason: "replay" });
    expect(h.d1.query("SELECT seen_at_ms FROM device_presence")).toEqual([{ seen_at_ms: seen }]);
  });

  it("rejects nonmember readers and active crews; ignores body device spoofing", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await bob.register();
    expect((await bob.call("GET", `/v1/crews/${crew}/presence`)).status).toBe(403);
    expect((await announce(bob, { active_crew: crew })).status).toBe(403);
    expect(h.d1.query("SELECT * FROM device_presence")).toEqual([]);
    expect((await alice.call("POST", "/v1/presence", { ...INPUT, device_id: bob.id })).status).toBe(200);
    expect(h.d1.query("SELECT device_id FROM device_presence")).toEqual([{ device_id: alice.id }]);
    expect((await alice.call("GET", "/v1/crews/not-a-uuid/presence")).status).toBe(400);
  });

  it("isolates crews and omits members who are busy in another crew without disclosing that crew", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const charlie = await h.device(3);
    const a = await newCrew(alice, "A");
    const b = await newCrew(charlie, "B");
    await joinCrew(alice, bob, a);
    await joinCrew(charlie, bob, b);
    await announce(alice);
    await announce(charlie, { active_crew: b });
    await announce(bob);
    const pathA = `/v1/crews/${a}/presence`;
    const pathB = `/v1/crews/${b}/presence`;
    expect((await json<ApiPresence[]>(await alice.call("GET", pathA))).map((p) => p.device_id)).toEqual([alice.id, bob.id].sort());
    await announce(bob, { seq: 2, active_crew: b });
    const response = await alice.call("GET", pathA);
    const text = await response.text();
    expect(text).not.toContain(b);
    expect(text).not.toContain(charlie.id);
    expect(JSON.parse(text).map((p: ApiPresence) => p.device_id)).toEqual([alice.id]);
    expect((await json<ApiPresence[]>(await charlie.call("GET", pathB))).map((p) => p.device_id)).toEqual([bob.id, charlie.id].sort());
    await announce(bob, { seq: 3 });
    expect((await json<ApiPresence[]>(await alice.call("GET", pathA))).map((p) => p.device_id)).toContain(bob.id);
    expect(h.d1.query("SELECT * FROM device_presence WHERE device_id = ?", bob.id)).toHaveLength(1);
  });

  it("keeps different games and more than eight members for the session client to select", async () => {
    const h = createHarness();
    const owner = await h.device(1);
    const crew = await newCrew(owner);
    await announce(owner);
    for (let seed = 2; seed <= 10; seed += 1) {
      const peer = await h.device(seed);
      await peer.register();
      await h.app.db.addMember(crew, peer.id, h.clock.now);
      await announce(peer, { game: seed % 2 === 0 ? "valorant" : "cs2" });
    }
    const rows = await json<ApiPresence[]>(await owner.call("GET", `/v1/crews/${crew}/presence`));
    expect(rows).toHaveLength(10);
    expect(new Set(rows.map((p) => p.game))).toEqual(new Set(["cs2", "valorant"]));
  });

  it("expires immediately after the TTL boundary without relying on cron or client clocks", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    await announce(alice, { online_since_ms: Number.MAX_SAFE_INTEGER });
    const seen = h.clock.now;
    const path = `/v1/crews/${crew}/presence`;
    h.clock.now = seen + PRESENCE_TTL_MS - 1; // signed() adds one ms
    expect(await json(await alice.call("GET", path))).toHaveLength(1);
    expect(await json(await alice.call("GET", path))).toEqual([]);
    expect(h.d1.query("SELECT * FROM device_presence")).toHaveLength(1);
  });

  it("renews receipt time and changes games with a newer heartbeat", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    await announce(alice);
    const firstSeen = h.clock.now;
    h.clock.now += 20_000;
    expect((await announce(alice, { seq: 2, game: "valorant" })).status).toBe(200);
    const latest = h.clock.now;
    h.clock.now = firstSeen + PRESENCE_TTL_MS + 1;
    const rows = await json<ApiPresence[]>(await alice.call("GET", `/v1/crews/${crew}/presence`));
    expect(rows).toMatchObject([{ game: "valorant", seq: 2, online_since_ms: INPUT.online_since_ms, seen_at_ms: latest }]);
  });

  it("refuses stale sequences without extending TTL and allows restarted peers after expiry", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    await announce(alice, { seq: 100 });
    const seen = h.clock.now;
    h.clock.now += 10_000;
    for (const seq of [100, 99, 0]) {
      const response = await announce(alice, { seq, game: null, online_since_ms: 0 });
      expect(response.status).toBe(409);
      expect(await json(response)).toMatchObject({ error: "stale_presence" });
    }
    expect(h.d1.query("SELECT seq, game, online_since_ms, seen_at_ms FROM device_presence")).toEqual([
      { seq: 100, game: "cs2", online_since_ms: INPUT.online_since_ms, seen_at_ms: seen },
    ]);
    h.clock.now = seen + PRESENCE_TTL_MS - 1;
    expect((await announce(alice, { seq: 0 })).status).toBe(409);
    expect((await announce(alice, { seq: 0, online_since_ms: 0 })).status).toBe(200);
    expect((await announce(alice, { seq: 1, game: null })).status).toBe(200);
    expect(h.d1.query("SELECT seq, game FROM device_presence")).toEqual([{ seq: 1, game: null }]);
  });

  it("atomically preserves the highest concurrent sequence and never moves receipt time backwards", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const seen = h.clock.now;
    const updates = await Promise.all([3, 8, 4, 8, 1, 7].map((seq) =>
      h.app.db.updatePresence(alice.id, { ...INPUT, seq }, seen, PRESENCE_TTL_MS)));
    expect(updates.filter((result) => result !== null)).toHaveLength(2);
    expect((await h.app.db.listPresence(crew, seen, PRESENCE_TTL_MS))[0]?.seq).toBe(8);
    expect(await h.app.db.updatePresence(alice.id, { ...INPUT, seq: 9 }, seen - 1, PRESENCE_TTL_MS)).toBe(seen);
    expect((await h.app.db.listPresence(crew, seen, PRESENCE_TTL_MS))[0]?.seen_at_ms).toBe(seen);
  });

  it("checks current membership when reading even if a presence row is still fresh", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    await announce(bob);
    h.d1.database.prepare("DELETE FROM crew_members WHERE crew_id = ? AND device_id = ?").run(crew, bob.id);
    expect(await json(await alice.call("GET", `/v1/crews/${crew}/presence`))).toEqual([]);
    expect((await bob.call("GET", `/v1/crews/${crew}/presence`)).status).toBe(403);
  });

  it.each([{}, { ...INPUT, game: 5 }, { ...INPUT, seq: -1 }, { ...INPUT, active_crew: "invalid" }, null, []])(
    "rejects malformed bodies without changing presence: %j", async (input) => {
      const h = createHarness();
      const alice = await h.device(1);
      await alice.register();
      expect((await alice.call("POST", "/v1/presence", input)).status).toBe(400);
      expect(h.d1.query("SELECT * FROM device_presence")).toEqual([]);
    },
  );

  it("rejects oversized and invalid JSON bodies", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    expect((await alice.call("POST", "/v1/presence", "{")).status).toBe(400);
    expect((await alice.call("POST", "/v1/presence", { ...INPUT, padding: "x".repeat(16_384) })).status).toBe(413);
    expect(h.d1.query("SELECT * FROM device_presence")).toEqual([]);
  });

  it("purges expired rows at cron time while preserving the exact boundary and renewed rows", async () => {
    const h = createHarness();
    const devices = await Promise.all([h.device(1), h.device(2), h.device(3)]);
    for (const device of devices) await device.register();
    const now = h.clock.now;
    for (const [index, device] of devices.entries()) {
      await h.app.db.updatePresence(device.id, INPUT, now - PRESENCE_TTL_MS + index - 1, PRESENCE_TTL_MS);
    }
    const stats = await runSweep(h.app);
    expect(stats.presence_rows_purged).toBe(1);
    expect(h.d1.query<{ device_id: string }>("SELECT device_id FROM device_presence ORDER BY device_id")
      .map((p) => p.device_id)).toEqual(devices.slice(1).map((d) => d.id).sort());
    expect((await runSweep(h.app)).presence_rows_purged).toBe(0);
  });
});
