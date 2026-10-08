import { describe, expect, it } from "vitest";
import { MAX_BODY_BYTES } from "../src/http.js";
import { createHarness, joinCrew, json, newCrew, ORIGIN, T0 } from "./helpers/harness.js";

describe("health and routing", () => {
  it("serves GET /v1/health without authentication", async () => {
    const h = createHarness();
    const res = await h.send(new Request(`${ORIGIN}/v1/health`));
    expect(res.status).toBe(200);
    expect(await json(res)).toEqual({ ok: true });
    expect(res.headers.get("content-type")).toContain("application/json");
    expect(res.headers.get("cache-control")).toBe("no-store");
  });

  it("returns 404 for unknown paths and 405 with Allow for wrong methods", async () => {
    const h = createHarness();
    for (const path of ["/", "/v1", "/v1/nope", "/v1/health/", "/v1/clips/x/y/z", "/health"]) {
      expect((await h.send(new Request(`${ORIGIN}${path}`))).status, path).toBe(404);
    }
    const res = await h.send(new Request(`${ORIGIN}/v1/health`, { method: "POST", body: "{}" }));
    expect(res.status).toBe(405);
    expect(res.headers.get("allow")).toBe("GET");
    const del = await h.send(new Request(`${ORIGIN}/v1/crews`, { method: "DELETE" }));
    expect(del.status).toBe(405);
    expect(del.headers.get("allow")).toBe("POST");
  });

  it("requires a signature on every route except health and device registration", async () => {
    const h = createHarness();
    const u = "00000000-0000-0000-0000-000000000001";
    const routes: Array<[string, string]> = [
      ["POST", "/v1/crews"],
      ["POST", "/v1/crews/join"],
      ["POST", `/v1/crews/${u}/invites`],
      ["GET", `/v1/crews/${u}/members`],
      ["POST", "/v1/clips"],
      ["POST", `/v1/clips/${u}/upload-urls`],
      ["POST", `/v1/clips/${u}/download-urls`],
      ["DELETE", `/v1/clips/${u}`],
    ];
    for (const [method, path] of routes) {
      const res = await h.send(new Request(`${ORIGIN}${path}`, { method, ...(method === "POST" ? { body: "{}" } : {}) }));
      expect(res.status, `${method} ${path}`).toBe(401);
    }
  });

  it("rejects bodies above 16 KiB with 413 before authenticating", async () => {
    const h = createHarness();
    const big = JSON.stringify({ name: "x".repeat(MAX_BODY_BYTES) });
    const res = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: big }));
    expect(res.status).toBe(413);
    const alice = await h.device(1);
    await alice.register();
    expect((await alice.call("POST", "/v1/crews", big)).status).toBe(413);
  });

  it("turns unexpected failures into an opaque 500", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    h.app.db.createCrew = () => Promise.reject(new Error("db exploded with secret=hunter2"));
    const res = await alice.call("POST", "/v1/crews", { name: "Squad" });
    expect(res.status).toBe(500);
    const text = await res.text();
    expect(text).toContain("internal_error");
    expect(text).not.toContain("hunter2");
  });
});

describe("POST /v1/devices", () => {
  it("registers a device, then is idempotent for the same key", async () => {
    const h = createHarness();
    const alice = await h.device(1, "Alice");
    const first = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(alice.registrationBody()) }));
    expect(first.status).toBe(201);
    expect(await json(first)).toEqual({ device_id: alice.id });
    const again = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(alice.registrationBody()) }));
    expect(again.status).toBe(200);
    expect(await json(again)).toEqual({ device_id: alice.id });
    expect(h.d1.query("SELECT * FROM devices")).toEqual([
      { device_id: alice.id, public_key_b64: alice.publicKeyB64, display_name: "Alice", created_at: T0 },
    ]);
  });

  it("keeps the original display name on re-registration", async () => {
    const h = createHarness();
    const alice = await h.device(1, "Alice");
    await alice.register();
    await h.send(new Request(`${ORIGIN}/v1/devices`, {
      method: "POST",
      body: JSON.stringify({ ...alice.registrationBody(), display_name: "Renamed" }),
    }));
    expect(h.d1.query<{ display_name: string }>("SELECT display_name FROM devices")[0]?.display_name).toBe("Alice");
  });

  it("returns 409 when the id exists with a different key", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const mallory = await h.device(2);
    await alice.register();
    const res = await h.send(new Request(`${ORIGIN}/v1/devices`, {
      method: "POST",
      body: JSON.stringify({ ...mallory.registrationBody(), device_id: alice.id }),
    }));
    expect(res.status).toBe(409);
    expect((await json(res))["error"]).toBe("device_exists");
    // The legitimate key is untouched.
    expect(h.d1.query<{ public_key_b64: string }>("SELECT public_key_b64 FROM devices")[0]?.public_key_b64).toBe(
      alice.publicKeyB64,
    );
  });

  it("handles a lost registration race by comparing keys", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const mallory = await h.device(2);
    // Simulate: lookup sees nothing, but another request inserts before ours does.
    const realGet = h.app.db.getDevice.bind(h.app.db);
    let first = true;
    h.app.db.getDevice = async (id) => {
      if (first) {
        first = false;
        return null;
      }
      return realGet(id);
    };
    await h.app.db.insertDeviceIfAbsent({
      device_id: alice.id,
      public_key_b64: mallory.publicKeyB64,
      display_name: "x",
      created_at: 1,
    });
    const res = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(alice.registrationBody()) }));
    expect(res.status).toBe(409);
  });

  it.each([
    ["not json", "nope"],
    ["json array", "[]"],
    ["empty body", ""],
    ["bad id", JSON.stringify({ device_id: "x", public_key_b64: "AAAA", display_name: "a" })],
  ])("rejects %s with 400", async (_name, body) => {
    const h = createHarness();
    const res = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body }));
    expect(res.status).toBe(400);
  });

  it("rejects a key that is the right length but not valid base64 canonical form", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const body = { ...alice.registrationBody(), public_key_b64: `${alice.publicKeyB64}\n` };
    const res = await h.send(new Request(`${ORIGIN}/v1/devices`, { method: "POST", body: JSON.stringify(body) }));
    expect(res.status).toBe(400);
  });
});

describe("crews and invites", () => {
  it("creates a crew with the creator as its only member", async () => {
    const h = createHarness();
    const alice = await h.device(1, "Alice");
    const crew = await newCrew(alice);
    expect(crew).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    const members = await alice.call("GET", `/v1/crews/${crew}/members`);
    expect(members.status).toBe(200);
    expect(await json(members)).toEqual([{ device_id: alice.id, display_name: "Alice" }]);
  });

  it("validates the crew name", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await alice.register();
    expect((await alice.call("POST", "/v1/crews", { name: "" })).status).toBe(400);
    expect((await alice.call("POST", "/v1/crews", {})).status).toBe(400);
    expect((await alice.call("POST", "/v1/crews", "not json")).status).toBe(400);
  });

  it("enforces membership (403) on invites and member lists", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const eve = await h.device(2);
    const crew = await newCrew(alice);
    await eve.register();
    expect((await eve.call("POST", `/v1/crews/${crew}/invites`)).status).toBe(403);
    expect((await eve.call("GET", `/v1/crews/${crew}/members`)).status).toBe(403);
    // A crew that does not exist looks the same as one the caller is not in.
    const ghost = "00000000-0000-0000-0000-00000000dead";
    expect((await eve.call("GET", `/v1/crews/${ghost}/members`)).status).toBe(403);
    expect((await alice.call("GET", "/v1/crews/not-a-uuid/members")).status).toBe(400);
    expect((await alice.call("POST", "/v1/crews/NOT-A-UUID/invites")).status).toBe(400);
  });

  it("creates invites of 10 chars from [A-Z2-9] valid for 24 hours", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const res = await alice.call("POST", `/v1/crews/${crew}/invites`);
    expect(res.status).toBe(201);
    const invite = await json<{ code: string; expires_at: number }>(res);
    expect(invite.code).toMatch(/^[A-Z2-9]{10}$/);
    expect(invite.expires_at).toBe(h.clock.now + 24 * 3600 * 1000);
    const row = h.d1.query<{ uses_left: number; created_by: string; crew_id: string }>("SELECT * FROM invites")[0];
    expect(row).toMatchObject({ uses_left: 5, created_by: alice.id, crew_id: crew });
  });

  it("lets a device join with a valid code and lists both members", async () => {
    const h = createHarness();
    const alice = await h.device(1, "Alice");
    const bob = await h.device(2, "Bob");
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    const members = await json<Array<{ device_id: string; display_name: string }>>(
      await bob.call("GET", `/v1/crews/${crew}/members`),
    );
    expect(members).toEqual([
      { device_id: alice.id, display_name: "Alice" },
      { device_id: bob.id, display_name: "Bob" },
    ]);
  });

  it("accepts lowercase codes with surrounding spaces", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await bob.register();
    const { code } = await json<{ code: string }>(await alice.call("POST", `/v1/crews/${crew}/invites`));
    const res = await bob.call("POST", "/v1/crews/join", { code: ` ${code.toLowerCase()} ` });
    expect(res.status).toBe(200);
    expect(await json(res)).toEqual({ crew_id: crew });
  });

  it("admits exactly 5 devices per invite", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const { code } = await json<{ code: string }>(await alice.call("POST", `/v1/crews/${crew}/invites`));
    const outcomes: number[] = [];
    for (let seed = 10; seed < 17; seed += 1) {
      const guest = await h.device(seed);
      await guest.register();
      outcomes.push((await guest.call("POST", "/v1/crews/join", { code })).status);
    }
    expect(outcomes).toEqual([200, 200, 200, 200, 200, 404, 404]);
    expect(h.d1.query("SELECT * FROM crew_members WHERE crew_id = ?", crew)).toHaveLength(6);
    expect(h.d1.query<{ uses_left: number }>("SELECT uses_left FROM invites")[0]?.uses_left).toBe(0);
  });

  it("does not spend another use when a member joins again", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await bob.register();
    const { code } = await json<{ code: string }>(await alice.call("POST", `/v1/crews/${crew}/invites`));
    for (let i = 0; i < 3; i += 1) {
      const res = await bob.call("POST", "/v1/crews/join", { code });
      expect(res.status).toBe(200);
      expect(await json(res)).toEqual({ crew_id: crew });
    }
    expect(h.d1.query<{ uses_left: number }>("SELECT uses_left FROM invites")[0]?.uses_left).toBe(4);
    // The creator "joining" their own crew does not burn a use either.
    expect((await alice.call("POST", "/v1/crews/join", { code })).status).toBe(200);
    expect(h.d1.query<{ uses_left: number }>("SELECT uses_left FROM invites")[0]?.uses_left).toBe(4);
  });

  it("rejects expired invites", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await bob.register();
    const { code, expires_at } = await json<{ code: string; expires_at: number }>(
      await alice.call("POST", `/v1/crews/${crew}/invites`),
    );
    h.clock.now = expires_at; // expiry instant itself is already too late
    const res = await bob.call("POST", "/v1/crews/join", { code });
    expect(res.status).toBe(404);
    expect((await json(res))["error"]).toBe("invalid_invite");
    expect(h.d1.query("SELECT * FROM crew_members WHERE device_id = ?", bob.id)).toHaveLength(0);
    expect(h.d1.query<{ uses_left: number }>("SELECT uses_left FROM invites")[0]?.uses_left).toBe(5);
  });

  it("accepts an invite until just before it expires", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await bob.register();
    const { code, expires_at } = await json<{ code: string; expires_at: number }>(
      await alice.call("POST", `/v1/crews/${crew}/invites`),
    );
    h.clock.now = expires_at - 3; // signed() adds 1 ms
    expect((await bob.call("POST", "/v1/crews/join", { code })).status).toBe(200);
  });

  it("rejects unknown and malformed codes", async () => {
    const h = createHarness();
    const bob = await h.device(2);
    await bob.register();
    expect((await bob.call("POST", "/v1/crews/join", { code: "AAAAAAAAAA" })).status).toBe(404);
    expect((await bob.call("POST", "/v1/crews/join", { code: "short" })).status).toBe(400);
    expect((await bob.call("POST", "/v1/crews/join", {})).status).toBe(400);
  });

  it("only one of several concurrent joins gets the last use", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const { code } = await json<{ code: string }>(await alice.call("POST", `/v1/crews/${crew}/invites`));
    h.d1.database.prepare("UPDATE invites SET uses_left = 1").run();
    const guests = await Promise.all([20, 21, 22, 23].map((seed) => h.device(seed)));
    for (const guest of guests) {
      await guest.register();
    }
    const requests = await Promise.all(guests.map((g) => g.signed("POST", "/v1/crews/join", { code })));
    const statuses = (await Promise.all(requests.map((r) => h.send(r)))).map((r) => r.status).sort();
    expect(statuses).toEqual([200, 404, 404, 404]);
  });
});
