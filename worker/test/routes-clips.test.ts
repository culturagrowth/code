import { describe, expect, it } from "vitest";
import { clipPrefix, manifestKey, objectKey } from "../src/keys.js";
import { MAX_CLIP_BYTES, MAX_DAILY_BYTES, utcDay } from "../src/quota.js";
import { createHarness, joinCrew, json, newCrew, T0 } from "./helpers/harness.js";
import type { Harness, TestDevice } from "./helpers/harness.js";

const MIB = 1024 * 1024;
const GIB = 1024 * MIB;
const HOUR = 3_600_000;

interface Scene {
  h: Harness;
  alice: TestDevice;
  bob: TestDevice;
  eve: TestDevice;
  crew: string;
}

/** Alice and Bob share a crew; Eve is a registered outsider. */
async function scene(): Promise<Scene> {
  const h = createHarness();
  const alice = await h.device(1, "Alice");
  const bob = await h.device(2, "Bob");
  const eve = await h.device(3, "Eve");
  const crew = await newCrew(alice);
  await joinCrew(alice, bob, crew);
  await eve.register();
  return { h, alice, bob, eve, crew };
}

let counter = 0;
function clipId(): string {
  counter += 1;
  return `c0000000-0000-4000-8000-${String(counter).padStart(12, "0")}`;
}

async function register(owner: TestDevice, crew: string, extra: Record<string, unknown> = {}): Promise<string> {
  const id = clipId();
  const res = await owner.call("POST", "/v1/clips", { clip_id: id, crew_id: crew, ...extra });
  expect([200, 201]).toContain(res.status);
  return id;
}

function chunks(count: number, size: number, start = 0): { indices: number[]; sizes: number[] } {
  return {
    indices: Array.from({ length: count }, (_, i) => start + i),
    sizes: Array.from({ length: count }, () => size),
  };
}

function bytesOf(h: Harness, clip: string): number {
  return h.d1.query<{ bytes: number }>("SELECT bytes FROM clips WHERE clip_id = ?", clip)[0]?.bytes ?? -1;
}

describe("POST /v1/clips", () => {
  it("registers a clip for 72 hours by default", async () => {
    const { h, alice, crew } = await scene();
    const id = clipId();
    const res = await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew });
    expect(res.status).toBe(201);
    const body = await json<{ clip_id: string; expires_at: number }>(res);
    expect(body.clip_id).toBe(id);
    expect(body.expires_at).toBe(h.clock.now + 72 * HOUR);
    expect(h.d1.query("SELECT * FROM clips WHERE clip_id = ?", id)[0]).toMatchObject({
      crew_id: crew,
      owner: alice.id,
      deleted_at: null,
      bytes: 0,
    });
  });

  it("honours ttl_s up to 72 hours", async () => {
    const { h, alice, crew } = await scene();
    const res = await alice.call("POST", "/v1/clips", { clip_id: clipId(), crew_id: crew, ttl_s: 3600 });
    expect((await json<{ expires_at: number }>(res)).expires_at).toBe(h.clock.now + HOUR);
    for (const ttl_s of [0, -1, 259_201, 1.5, "60"]) {
      const bad = await alice.call("POST", "/v1/clips", { clip_id: clipId(), crew_id: crew, ttl_s });
      expect(bad.status, `ttl_s=${String(ttl_s)}`).toBe(400);
    }
  });

  it("is idempotent for the same owner and keeps the original expiry", async () => {
    const { h, alice, crew } = await scene();
    const id = clipId();
    const first = await json<{ expires_at: number }>(
      await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew, ttl_s: 600 }),
    );
    h.clock.now += 100_000;
    const again = await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew, ttl_s: 259_200 });
    expect(again.status).toBe(200);
    expect(await json(again)).toEqual({ clip_id: id, expires_at: first.expires_at });
    expect(h.d1.query("SELECT * FROM clips WHERE clip_id = ?", id)).toHaveLength(1);
  });

  it("refuses to hand an existing clip id to another owner or crew (409)", async () => {
    const { h, alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    const stolen = await bob.call("POST", "/v1/clips", { clip_id: id, crew_id: crew });
    expect(stolen.status).toBe(409);
    expect((await json(stolen))["error"]).toBe("clip_exists");
    const otherCrew = await newCrew(alice, "Other");
    expect((await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: otherCrew })).status).toBe(409);
    expect(h.d1.query<{ owner: string }>("SELECT owner FROM clips WHERE clip_id = ?", id)[0]?.owner).toBe(alice.id);
  });

  it("requires membership of the crew (403) and valid ids (400)", async () => {
    const { alice, eve, crew } = await scene();
    expect((await eve.call("POST", "/v1/clips", { clip_id: clipId(), crew_id: crew })).status).toBe(403);
    expect((await alice.call("POST", "/v1/clips", { clip_id: "nope", crew_id: crew })).status).toBe(400);
    expect((await alice.call("POST", "/v1/clips", { clip_id: clipId() })).status).toBe(400);
  });

  it("does not revive a deleted or expired clip", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew, { ttl_s: 60 });
    h.clock.now += 61_000;
    expect((await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew })).status).toBe(410);
    const id2 = await register(alice, crew);
    expect((await alice.call("DELETE", `/v1/clips/${id2}`)).status).toBe(200);
    expect((await alice.call("POST", "/v1/clips", { clip_id: id2, crew_id: crew })).status).toBe(410);
  });

  it("handles a lost registration race", async () => {
    const { h, alice, bob, crew } = await scene();
    const id = clipId();
    const realGet = h.app.db.getClip.bind(h.app.db);
    let first = true;
    h.app.db.getClip = async (clip) => {
      if (first) {
        first = false;
        return null;
      }
      return realGet(clip);
    };
    await h.app.db.insertClip({ clip_id: id, crew_id: crew, owner: bob.id, created_at: 1, expires_at: T0 + HOUR });
    expect((await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew })).status).toBe(409);
  });
});

describe("POST /v1/clips/:clip/upload-urls", () => {
  it("returns presigned PUT URLs for exactly the expected keys", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew);
    const res = await alice.call("POST", `/v1/clips/${id}/upload-urls`, {
      pov: alice.id,
      quality: "proxy",
      indices: [0, 1, 12],
      sizes: [1000, 2000, 3000],
      manifest: true,
      manifest_size: 777,
    });
    expect(res.status).toBe(200);
    const body = await json<{
      expires_in: number;
      expires_at: number;
      headers: Record<string, string>;
      chunks: Array<{ index: number; key: string; url: string; content_length: number }>;
      manifest: { key: string; url: string; content_length: number } | null;
    }>(res);
    expect(body.expires_in).toBe(900);
    expect(body.expires_at).toBe(h.clock.now + 900_000);
    expect(body.headers).toEqual({ "Content-Type": "application/octet-stream" });
    expect(body.chunks.map((c) => c.index)).toEqual([0, 1, 12]);
    for (const chunk of body.chunks) {
      expect(chunk.key).toBe(objectKey(crew, id, alice.id, "proxy", chunk.index));
      const url = new URL(chunk.url);
      expect(url.host).toBe("testaccount.r2.cloudflarestorage.com");
      expect(url.pathname).toBe(`/test-bucket/${chunk.key}`);
      expect(url.searchParams.get("X-Amz-Expires")).toBe("900");
      // The announced size is signed in: the PUT cannot carry more bytes than were charged.
      expect(url.searchParams.get("X-Amz-SignedHeaders")).toBe("content-length;content-type;host");
    }
    expect(body.chunks.map((c) => c.content_length)).toEqual([1000, 2000, 3000]);
    expect(body.chunks[2]?.key).toMatch(/\/proxy\/000012\.bin$/);
    expect(body.manifest?.key).toBe(manifestKey(crew, id, alice.id, "proxy"));
    expect(body.manifest?.content_length).toBe(777);
    expect(new URL(body.manifest?.url ?? "").searchParams.get("X-Amz-SignedHeaders")).toBe(
      "content-length;content-type;host",
    );
    expect(new URL(body.manifest?.url ?? "").pathname).toBe(`/test-bucket/${body.manifest?.key}`);
    // Every key sits under the clip prefix, i.e. what DELETE and the sweep remove.
    for (const { key } of body.chunks) {
      expect(key.startsWith(clipPrefix(crew, id))).toBe(true);
    }
  });

  it("omits the manifest unless asked for", async () => {
    const { alice, crew } = await scene();
    const id = await register(alice, crew);
    const body = await json<{ manifest: unknown; chunks: unknown[] }>(
      await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", indices: [0], sizes: [5] }),
    );
    expect(body.manifest).toBeNull();
    expect(body.chunks).toHaveLength(1);
    const manifestOnly = await json<{ manifest: { key: string } | null; chunks: unknown[] }>(
      await alice.call("POST", `/v1/clips/${id}/upload-urls`, {
        pov: alice.id,
        quality: "full",
        indices: [],
        sizes: [],
        manifest: true,
        manifest_size: 1234,
      }),
    );
    expect(manifestOnly.chunks).toEqual([]);
    expect(manifestOnly.manifest?.key).toMatch(/\/full\/manifest\.bin$/);
  });

  it("rejects non-members with 403, even for their own POV", async () => {
    const { eve, alice, crew } = await scene();
    const id = await register(alice, crew);
    const res = await eve.call("POST", `/v1/clips/${id}/upload-urls`, { pov: eve.id, quality: "full", indices: [0], sizes: [5] });
    expect(res.status).toBe(403);
    expect((await json(res))["error"]).toBe("not_a_member");
  });

  it("rejects uploads for somebody else's POV with 403", async () => {
    const { alice, bob, crew, h } = await scene();
    const id = await register(alice, crew);
    const res = await bob.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", indices: [0], sizes: [5] });
    expect(res.status).toBe(403);
    expect((await json(res))["error"]).toBe("pov_mismatch");
    // The owner of the clip may not upload bob's POV either.
    const own = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: bob.id, quality: "full", indices: [0], sizes: [5] });
    expect(own.status).toBe(403);
    expect(bytesOf(h, id)).toBe(0);
  });

  it("lets a non-owner member upload their own POV to someone else's clip", async () => {
    const { alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    const res = await bob.call("POST", `/v1/clips/${id}/upload-urls`, { pov: bob.id, quality: "full", indices: [0], sizes: [5] });
    expect(res.status).toBe(200);
    const body = await json<{ chunks: Array<{ key: string }> }>(res);
    expect(body.chunks[0]?.key).toBe(objectKey(crew, id, bob.id, "full", 0));
  });

  it("rejects unknown, expired and deleted clips", async () => {
    const { h, alice, crew } = await scene();
    const ghost = "00000000-0000-0000-0000-00000000dead";
    const request = { pov: alice.id, quality: "full", indices: [0], sizes: [5] };
    expect((await alice.call("POST", `/v1/clips/${ghost}/upload-urls`, request)).status).toBe(404);
    expect((await alice.call("POST", "/v1/clips/not-a-uuid/upload-urls", request)).status).toBe(400);
    const short = await register(alice, crew, { ttl_s: 60 });
    h.clock.now += 61_000;
    const expired = await alice.call("POST", `/v1/clips/${short}/upload-urls`, request);
    expect(expired.status).toBe(410);
    expect((await json(expired))["error"]).toBe("clip_expired");
    const gone = await register(alice, crew);
    await alice.call("DELETE", `/v1/clips/${gone}`);
    const deleted = await alice.call("POST", `/v1/clips/${gone}/upload-urls`, request);
    expect(deleted.status).toBe(410);
    expect((await json(deleted))["error"]).toBe("clip_gone");
  });

  it("caps requests at 64 indices", async () => {
    const { alice, crew } = await scene();
    const id = await register(alice, crew);
    const ok = chunks(64, 1000);
    expect((await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", ...ok })).status).toBe(200);
    const tooMany = chunks(65, 1000, 100);
    const res = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", ...tooMany });
    expect(res.status).toBe(400);
    expect((await json(res))["error"]).toBe("too_many_indices");
  });

  it("caps each chunk at 64 MiB", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew);
    const exact = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", indices: [0], sizes: [64 * MIB] });
    expect(exact.status).toBe(200);
    const over = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", indices: [1], sizes: [64 * MIB + 1] });
    expect(over.status).toBe(413);
    expect((await json(over))["error"]).toBe("chunk_too_large");
    expect(bytesOf(h, id)).toBe(64 * MIB);
  });

  it("rejects malformed bodies without reserving anything", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew);
    const base = { pov: alice.id, quality: "full", indices: [0, 1], sizes: [5, 5] };
    for (const patch of [
      { pov: undefined },
      { quality: "hd" },
      { indices: [0, 0] },
      { sizes: [5] },
      { sizes: [0, 5] },
      { indices: [-1, 1] },
      { indices: [], sizes: [] },
    ]) {
      const res = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { ...base, ...patch });
      expect(res.status, JSON.stringify(patch)).toBe(400);
    }
    expect((await alice.call("POST", `/v1/clips/${id}/upload-urls`, "garbage")).status).toBe(400);
    expect(bytesOf(h, id)).toBe(0);
    expect(h.d1.query("SELECT * FROM usage")).toHaveLength(0);
  });

  it("enforces the 1.5 GiB per-clip total and does not charge retries twice", async () => {
    const { h, alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    const url = `/v1/clips/${id}/upload-urls`;
    const first = chunks(24, 64 * MIB); // exactly 1.5 GiB
    expect(24 * 64 * MIB).toBe(MAX_CLIP_BYTES);
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", ...first })).status).toBe(200);
    expect(bytesOf(h, id)).toBe(MAX_CLIP_BYTES);

    // Asking again for the same chunks (a retry) is free.
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", ...first })).status).toBe(200);
    expect(bytesOf(h, id)).toBe(MAX_CLIP_BYTES);

    // One more byte anywhere in the clip is refused, for every POV and quality.
    const extra = await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [24], sizes: [1] });
    expect(extra.status).toBe(413);
    expect((await json(extra))["error"]).toBe("clip_quota_exceeded");
    expect((await bob.call("POST", url, { pov: bob.id, quality: "proxy", indices: [0], sizes: [1] })).status).toBe(413);
    expect((await alice.call("POST", url, { pov: alice.id, quality: "proxy", indices: [0], sizes: [1] })).status).toBe(413);
    expect(bytesOf(h, id)).toBe(MAX_CLIP_BYTES);
    // Growing an announced chunk also counts.
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [0], sizes: [64 * MIB] })).status).toBe(200);
  });

  it("charges only the growth when a chunk is announced larger the second time", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew);
    const url = `/v1/clips/${id}/upload-urls`;
    const K = 1024;
    await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [0, 1], sizes: [300 * K, 400 * K] });
    expect(bytesOf(h, id)).toBe(700 * K);
    await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [0, 1, 2], sizes: [350 * K, 50 * K, 500 * K] });
    expect(bytesOf(h, id)).toBe(700 * K + 50 * K + 500 * K);
    // Same index, different quality or pov is a different chunk.
    await alice.call("POST", url, { pov: alice.id, quality: "proxy", indices: [0], sizes: [600 * K] });
    expect(bytesOf(h, id)).toBe(1850 * K);
    expect(
      h.d1.query<{ quality: string; idx: number; bytes: number }>(
        "SELECT quality, idx, bytes FROM clip_chunks ORDER BY quality, idx",
      ),
    ).toEqual([
      { quality: "full", idx: 0, bytes: 350 * K },
      { quality: "full", idx: 1, bytes: 400 * K },
      { quality: "full", idx: 2, bytes: 500 * K },
      { quality: "proxy", idx: 0, bytes: 600 * K },
    ]);
  });

  it("enforces the 10 GiB daily limit per device and resets on the next UTC day", async () => {
    const { h, alice, bob, crew } = await scene();
    const day = utcDay(h.clock.now);
    // 6 clips x 1.5 GiB = 9 GiB.
    for (let i = 0; i < 6; i += 1) {
      const id = await register(alice, crew);
      const res = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", ...chunks(24, 64 * MIB) });
      expect(res.status, `clip ${i}`).toBe(200);
    }
    const usage = () => h.d1.query<{ bytes: number }>("SELECT bytes FROM usage WHERE device_id = ? AND day = ?", alice.id, day)[0]?.bytes;
    expect(usage()).toBe(9 * GIB);

    // The 7th clip would bring the day to 10.5 GiB: refused, and nothing stays reserved on the clip.
    const seventh = await register(alice, crew);
    const refused = await alice.call("POST", `/v1/clips/${seventh}/upload-urls`, { pov: alice.id, quality: "full", ...chunks(24, 64 * MIB) });
    expect(refused.status).toBe(429);
    expect((await json(refused))["error"]).toBe("daily_quota_exceeded");
    expect(bytesOf(h, seventh)).toBe(0);
    expect(usage()).toBe(9 * GIB);

    // Exactly up to the limit is fine; one byte more is not.
    const url = `/v1/clips/${seventh}/upload-urls`;
    const fill = { pov: alice.id, quality: "full", indices: Array.from({ length: 16 }, (_, i) => i), sizes: Array.from({ length: 16 }, () => 64 * MIB) };
    expect((await alice.call("POST", url, fill)).status).toBe(200); // +1 GiB -> exactly 10 GiB
    expect(usage()).toBe(MAX_DAILY_BYTES);
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [100], sizes: [1] })).status).toBe(429);
    // The limit is per device: Bob is unaffected.
    const bobs = await bob.call("POST", url, { pov: bob.id, quality: "full", indices: [0], sizes: [1] });
    expect(bobs.status).toBe(200);

    // A new UTC day starts from zero again.
    h.clock.now = Date.UTC(2026, 9, 9, 0, 0, 30);
    expect(utcDay(h.clock.now)).not.toBe(day);
    const later = await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [200], sizes: [5] });
    expect(later.status).toBe(200);
  });
});

describe("POST /v1/clips/:clip/download-urls", () => {
  it("gives any crew member GET URLs for any POV", async () => {
    const { alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    const res = await bob.call("POST", `/v1/clips/${id}/download-urls`, {
      pov: alice.id,
      quality: "full",
      indices: [0, 3],
      manifest: true,
    });
    expect(res.status).toBe(200);
    const body = await json<{
      expires_in: number;
      chunks: Array<{ index: number; key: string; url: string }>;
      manifest: { key: string; url: string };
    }>(res);
    expect(body.expires_in).toBe(900);
    expect(body.chunks.map((c) => c.key)).toEqual([
      objectKey(crew, id, alice.id, "full", 0),
      objectKey(crew, id, alice.id, "full", 3),
    ]);
    for (const { url, key } of [...body.chunks, body.manifest]) {
      const parsed = new URL(url);
      expect(parsed.pathname).toBe(`/test-bucket/${key}`);
      expect(parsed.searchParams.get("X-Amz-Expires")).toBe("900");
      expect(parsed.searchParams.get("X-Amz-SignedHeaders")).toBe("host");
    }
    expect(body.manifest.key).toBe(manifestKey(crew, id, alice.id, "full"));
  });

  it("rejects non-members (403), unknown clips (404) and expired clips (410)", async () => {
    const { h, alice, eve, crew } = await scene();
    const id = await register(alice, crew, { ttl_s: 120 });
    const request = { pov: alice.id, quality: "full", indices: [0] };
    expect((await eve.call("POST", `/v1/clips/${id}/download-urls`, request)).status).toBe(403);
    expect((await alice.call("POST", "/v1/clips/00000000-0000-0000-0000-00000000dead/download-urls", request)).status).toBe(404);
    h.clock.now += 121_000;
    expect((await alice.call("POST", `/v1/clips/${id}/download-urls`, request)).status).toBe(410);
  });

  it("validates the request and caps it at 64 indices", async () => {
    const { alice, crew } = await scene();
    const id = await register(alice, crew);
    const url = `/v1/clips/${id}/download-urls`;
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", indices: [] })).status).toBe(400);
    expect((await alice.call("POST", url, { pov: alice.id, quality: "bad", indices: [1] })).status).toBe(400);
    expect((await alice.call("POST", url, { pov: "x", quality: "full", indices: [1] })).status).toBe(400);
    const tooMany = Array.from({ length: 65 }, (_, i) => i);
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", indices: tooMany })).status).toBe(400);
    const many = Array.from({ length: 64 }, (_, i) => i);
    expect((await alice.call("POST", url, { pov: alice.id, quality: "full", indices: many })).status).toBe(200);
  });

  it("does not touch quotas", async () => {
    const { h, alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    await bob.call("POST", `/v1/clips/${id}/download-urls`, { pov: alice.id, quality: "full", indices: [0] });
    expect(bytesOf(h, id)).toBe(0);
    expect(h.d1.query("SELECT * FROM usage")).toHaveLength(0);
  });
});

describe("DELETE /v1/clips/:clip", () => {
  async function populate(h: Harness, crew: string, id: string, povs: string[]): Promise<string[]> {
    const keys: string[] = [];
    for (const pov of povs) {
      for (const quality of ["proxy", "full"] as const) {
        for (let i = 0; i < 3; i += 1) {
          keys.push(objectKey(crew, id, pov, quality, i));
        }
        keys.push(manifestKey(crew, id, pov, quality));
      }
    }
    h.store.put(...keys);
    return keys;
  }

  it("lets any member delete every object of the clip and nothing else", async () => {
    const { h, alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    const other = await register(alice, crew);
    const mine = await populate(h, crew, id, [alice.id, bob.id]);
    const untouched = await populate(h, crew, other, [alice.id]);
    h.store.put("clips/00000000-0000-0000-0000-000000000009/x/y.bin");

    const res = await bob.call("DELETE", `/v1/clips/${id}`);
    expect(res.status).toBe(200);
    expect(await json(res)).toEqual({ ok: true, deleted_objects: mine.length, complete: true });
    for (const key of mine) {
      expect(h.store.keys.has(key)).toBe(false);
    }
    for (const key of untouched) {
      expect(h.store.keys.has(key)).toBe(true);
    }
    expect(h.store.keys.has("clips/00000000-0000-0000-0000-000000000009/x/y.bin")).toBe(true);
    expect(h.d1.query<{ deleted_at: number | null }>("SELECT deleted_at FROM clips WHERE clip_id = ?", id)[0]?.deleted_at).toBe(h.clock.now);
  });

  it("works with R2 pagination (deletes in batches)", async () => {
    const h = createHarness({ storePageSize: 4 });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const id = await register(alice, crew);
    const keys = await populate(h, crew, id, [alice.id]);
    expect(keys.length).toBeGreaterThan(4);
    const res = await alice.call("DELETE", `/v1/clips/${id}`);
    expect((await json<{ deleted_objects: number }>(res)).deleted_objects).toBe(keys.length);
    expect(h.store.keys.size).toBe(0);
    expect(h.store.deleteCalls).toBeGreaterThan(1);
  });

  it("is idempotent and blocks later uploads", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew);
    await populate(h, crew, id, [alice.id]);
    expect((await alice.call("DELETE", `/v1/clips/${id}`)).status).toBe(200);
    const again = await alice.call("DELETE", `/v1/clips/${id}`);
    expect(again.status).toBe(200);
    expect(await json(again)).toEqual({ ok: true, deleted_objects: 0, complete: true });
    const up = await alice.call("POST", `/v1/clips/${id}/upload-urls`, { pov: alice.id, quality: "full", indices: [0], sizes: [1] });
    expect(up.status).toBe(410);
  });

  it("removes an upload that landed after the first delete when deleted again", async () => {
    const { h, alice, bob, crew } = await scene();
    const id = await register(alice, crew);
    await populate(h, crew, id, [alice.id]);
    expect((await alice.call("DELETE", `/v1/clips/${id}`)).status).toBe(200);
    const late = objectKey(crew, id, bob.id, "full", 9);
    h.store.put(late); // a PUT through a presigned URL that was still valid
    const again = await bob.call("DELETE", `/v1/clips/${id}`);
    expect(await json(again)).toEqual({ ok: true, deleted_objects: 1, complete: true });
    expect(h.store.keys.size).toBe(0);
    expect(h.d1.query<{ deleted_at: number }>("SELECT deleted_at FROM clips WHERE clip_id = ?", id)[0]?.deleted_at).toBeLessThan(h.clock.now);
  });

  it("reports an incomplete deletion instead of claiming success, and finishes on retry", async () => {
    const h = createHarness({ storePageSize: 1 });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const id = await register(alice, crew);
    const keys = Array.from({ length: 20 }, (_, i) => objectKey(crew, id, alice.id, "full", i));
    h.store.put(...keys);
    const first = await json<{ deleted_objects: number; complete: boolean }>(await alice.call("DELETE", `/v1/clips/${id}`));
    expect(first.complete).toBe(false);
    expect(first.deleted_objects).toBe(15); // 30 R2 calls: 15 list + 15 delete
    expect(h.store.keys.size).toBe(5);
    expect(h.store.listCalls + h.store.deleteCalls).toBe(30);
    const second = await json<{ deleted_objects: number; complete: boolean }>(await alice.call("DELETE", `/v1/clips/${id}`));
    expect(second).toMatchObject({ deleted_objects: 5, complete: true });
    expect(h.store.keys.size).toBe(0);
  });

  it("rejects non-members (403) and unknown clips (404)", async () => {
    const { h, alice, eve, crew } = await scene();
    const id = await register(alice, crew);
    const keys = await populate(h, crew, id, [alice.id]);
    expect((await eve.call("DELETE", `/v1/clips/${id}`)).status).toBe(403);
    expect(h.store.keys.size).toBe(keys.length);
    expect((await alice.call("DELETE", "/v1/clips/00000000-0000-0000-0000-00000000dead")).status).toBe(404);
    expect((await alice.call("DELETE", "/v1/clips/not-a-uuid")).status).toBe(400);
  });

  it("does not mark the clip deleted when R2 fails, so it can be retried", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew);
    await populate(h, crew, id, [alice.id]);
    h.store.failDeleteWhen = () => true;
    expect((await alice.call("DELETE", `/v1/clips/${id}`)).status).toBe(500);
    expect(h.d1.query<{ deleted_at: number | null }>("SELECT deleted_at FROM clips WHERE clip_id = ?", id)[0]?.deleted_at).toBeNull();
    h.store.failDeleteWhen = null;
    expect((await alice.call("DELETE", `/v1/clips/${id}`)).status).toBe(200);
    expect(h.store.keys.size).toBe(0);
  });

  it("can delete a clip that already expired but was not swept yet", async () => {
    const { h, alice, crew } = await scene();
    const id = await register(alice, crew, { ttl_s: 60 });
    const keys = await populate(h, crew, id, [alice.id]);
    h.clock.now += 120_000;
    const res = await alice.call("DELETE", `/v1/clips/${id}`);
    expect((await json<{ deleted_objects: number }>(res)).deleted_objects).toBe(keys.length);
  });
});
