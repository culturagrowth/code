import { describe, expect, it } from "vitest";
import { manifestKey, objectKey } from "../src/keys.js";
import { utcDay } from "../src/quota.js";
import {
  SIGNATURE_RETENTION_MS,
  SWEEP_MAX_CLIPS,
  SWEEP_STORE_CALL_BUDGET,
  runSweep,
  USAGE_RETENTION_DAYS,
} from "../src/sweep.js";
import { createHarness, joinCrew, newCrew } from "./helpers/harness.js";
import type { Harness, TestDevice } from "./helpers/harness.js";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

async function clipWithObjects(h: Harness, owner: TestDevice, crew: string, ttl_s: number): Promise<{ id: string; keys: string[] }> {
  const id = h.app.randomUuid();
  const res = await owner.call("POST", "/v1/clips", { clip_id: id, crew_id: crew, ttl_s });
  expect(res.status).toBe(201);
  const keys = [
    objectKey(crew, id, owner.id, "proxy", 0),
    objectKey(crew, id, owner.id, "full", 0),
    objectKey(crew, id, owner.id, "full", 1),
    manifestKey(crew, id, owner.id, "full"),
  ];
  h.store.put(...keys);
  return { id, keys };
}

function clipIds(h: Harness): string[] {
  return h.d1.query<{ clip_id: string }>("SELECT clip_id FROM clips ORDER BY clip_id").map((row) => row.clip_id);
}

describe("runSweep", () => {
  it("deletes objects of expired clips and leaves live clips alone", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const short = await clipWithObjects(h, alice, crew, 3600);
    const long = await clipWithObjects(h, alice, crew, 72 * 3600);

    h.clock.now += 2 * HOUR;
    const stats = await runSweep(h.app);
    expect(stats.clips_processed).toBe(1);
    expect(stats.objects_deleted).toBe(short.keys.length);
    for (const key of short.keys) {
      expect(h.store.keys.has(key)).toBe(false);
    }
    for (const key of long.keys) {
      expect(h.store.keys.has(key)).toBe(true);
    }
    // 1 h past expiry is beyond the 15 min presign horizon, so the row goes too.
    expect(stats.clip_rows_deleted).toBe(1);
    expect(clipIds(h)).toEqual([long.id]);
  });

  it("deletes everything once the 72 h lifetime is over", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clip = await clipWithObjects(h, alice, crew, 72 * 3600);
    h.clock.now += 72 * HOUR - 1;
    expect((await runSweep(h.app)).clips_processed).toBe(0);
    h.clock.now += HOUR + 1;
    const stats = await runSweep(h.app);
    expect(stats.objects_deleted).toBe(clip.keys.length);
    expect(h.store.keys.size).toBe(0);
    expect(clipIds(h)).toEqual([]);
  });

  it("keeps the row for one more sweep so late uploads through still-valid URLs get removed", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clip = await clipWithObjects(h, alice, crew, 3600);

    h.clock.now += HOUR + 5 * MINUTE; // just expired: presigned URLs may still be live
    const first = await runSweep(h.app);
    expect(first.objects_deleted).toBe(clip.keys.length);
    expect(first.clip_rows_deleted).toBe(0);
    expect(clipIds(h)).toEqual([clip.id]);

    // A late PUT lands after the first sweep ...
    const late = objectKey(crew, clip.id, alice.id, "full", 9);
    h.store.put(late);

    h.clock.now += HOUR; // ... and the next hourly sweep cleans it up and drops the row.
    const second = await runSweep(h.app);
    expect(second.objects_deleted).toBe(1);
    expect(second.clip_rows_deleted).toBe(1);
    expect(h.store.keys.size).toBe(0);
    expect(clipIds(h)).toEqual([]);
  });

  it("also cleans clips that were deleted by hand but whose objects reappeared", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clip = await clipWithObjects(h, alice, crew, 3600);
    expect((await alice.call("DELETE", `/v1/clips/${clip.id}`)).status).toBe(200);
    h.store.put(objectKey(crew, clip.id, alice.id, "full", 5)); // late upload after the delete
    h.clock.now += 2 * HOUR;
    const stats = await runSweep(h.app);
    expect(stats.objects_deleted).toBe(1);
    expect(h.store.keys.size).toBe(0);
    expect(clipIds(h)).toEqual([]);
  });

  it("continues after a failing clip and retries it next time", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const bad = await clipWithObjects(h, alice, crew, 600);
    const good = await clipWithObjects(h, alice, crew, 600);
    h.store.failDeleteWhen = (keys) => keys.some((key) => key.includes(`/${bad.id}/`));
    h.clock.now += 2 * HOUR;

    const first = await runSweep(h.app);
    expect(first.clip_failures).toBe(1);
    expect(first.clips_processed).toBe(1);
    expect(clipIds(h)).toEqual([bad.id]);
    for (const key of good.keys) {
      expect(h.store.keys.has(key)).toBe(false);
    }
    for (const key of bad.keys) {
      expect(h.store.keys.has(key)).toBe(true);
    }

    h.store.failDeleteWhen = null;
    const second = await runSweep(h.app);
    expect(second.clip_failures).toBe(0);
    expect(h.store.keys.size).toBe(0);
    expect(clipIds(h)).toEqual([]);
  });

  it("handles at most SWEEP_MAX_CLIPS clips per run", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const total = SWEEP_MAX_CLIPS + 5;
    for (let i = 0; i < total; i += 1) {
      await clipWithObjects(h, alice, crew, 600);
    }
    h.clock.now += 2 * HOUR;
    expect((await runSweep(h.app)).clips_processed).toBe(SWEEP_MAX_CLIPS);
    expect(clipIds(h)).toHaveLength(5);
    expect((await runSweep(h.app)).clips_processed).toBe(5);
    expect(clipIds(h)).toHaveLength(0);
  });

  it("stays inside the R2 call budget and never drops a row whose objects may remain", async () => {
    // Pages of 2 keys: a clip with 5 objects takes 3 list + 3 delete calls.
    const h = createHarness({ storePageSize: 2 });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clips = [];
    for (let i = 0; i < 10; i += 1) {
      const id = h.app.randomUuid();
      expect((await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew, ttl_s: 600 })).status).toBe(201);
      const keys = Array.from({ length: 5 }, (_, n) => objectKey(crew, id, alice.id, "full", n));
      h.store.put(...keys);
      clips.push({ id, keys });
    }
    h.clock.now += 2 * HOUR;

    const first = await runSweep(h.app);
    expect(h.store.listCalls + h.store.deleteCalls).toBe(SWEEP_STORE_CALL_BUDGET);
    expect(first).toMatchObject({
      clips_processed: 6,
      clips_incomplete: 1,
      clips_deferred: 3,
      clip_rows_deleted: 6,
      objects_deleted: 6 * 5 + 4,
      clip_failures: 0,
    });
    // Invariant: a clip whose objects may still exist keeps its row (so the next run retries it).
    expect(clipIds(h)).toHaveLength(4);
    for (const clip of clips) {
      if (clip.keys.some((key) => h.store.keys.has(key))) {
        expect(clipIds(h)).toContain(clip.id);
      }
    }

    h.store.listCalls = 0;
    h.store.deleteCalls = 0;
    const second = await runSweep(h.app);
    expect(h.store.listCalls + h.store.deleteCalls).toBeLessThanOrEqual(SWEEP_STORE_CALL_BUDGET);
    expect(second.clips_incomplete + second.clips_deferred).toBe(0);
    expect(h.store.keys.size).toBe(0);
    expect(clipIds(h)).toEqual([]);
  });

  it("keeps the row of a clip it could not empty, however many runs it takes", async () => {
    const h = createHarness({ storePageSize: 1 });
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const id = h.app.randomUuid();
    await alice.call("POST", "/v1/clips", { clip_id: id, crew_id: crew, ttl_s: 600 });
    const keys = Array.from({ length: 60 }, (_, n) => objectKey(crew, id, alice.id, "full", n));
    h.store.put(...keys);
    h.clock.now += 2 * HOUR;

    let runs = 0;
    while (clipIds(h).length > 0) {
      runs += 1;
      expect(runs).toBeLessThan(10);
      const stats = await runSweep(h.app);
      expect(h.store.keys.size === 0 || stats.clips_incomplete === 1).toBe(true);
      // The row survives exactly until the last object is gone.
      if (h.store.keys.size > 0) {
        expect(clipIds(h)).toEqual([id]);
      }
    }
    expect(runs).toBeGreaterThan(1);
    expect(h.store.keys.size).toBe(0);
  });

  it("purges old signatures, expired invites and stale usage rows", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const bob = await h.device(2);
    const crew = await newCrew(alice);
    await joinCrew(alice, bob, crew);
    const clip = await clipWithObjects(h, alice, crew, 72 * 3600);
    await alice.call("POST", `/v1/clips/${clip.id}/upload-urls`, { pov: alice.id, quality: "full", indices: [0], sizes: [10] });
    expect(h.d1.query("SELECT * FROM seen_signatures").length).toBeGreaterThan(0);
    expect(h.d1.query("SELECT * FROM invites")).toHaveLength(1);
    expect(h.d1.query("SELECT * FROM usage")).toHaveLength(1);

    // Just under every threshold: nothing is purged yet.
    h.clock.now += SIGNATURE_RETENTION_MS - 1000;
    let stats = await runSweep(h.app);
    expect(stats.signatures_purged).toBe(0);
    expect(stats.invites_purged).toBe(0);
    expect(stats.usage_rows_purged).toBe(0);

    h.clock.now += 2000;
    stats = await runSweep(h.app);
    expect(stats.signatures_purged).toBeGreaterThan(0);
    expect(h.d1.query("SELECT * FROM seen_signatures")).toHaveLength(0);
    expect(stats.invites_purged).toBe(0);

    h.clock.now += 25 * HOUR; // the invite (24 h) is gone
    stats = await runSweep(h.app);
    expect(stats.invites_purged).toBe(1);
    expect(h.d1.query("SELECT * FROM invites")).toHaveLength(0);

    // Usage older than the retention window is dropped (the clip is still alive at 72 h).
    h.clock.now += USAGE_RETENTION_DAYS * 24 * HOUR;
    expect(utcDay(h.clock.now)).not.toBe(h.d1.query<{ day: string }>("SELECT day FROM usage")[0]?.day);
    stats = await runSweep(h.app);
    expect(stats.usage_rows_purged).toBe(1);
  });

  it("purges the per-day abuse counters after the retention window", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    await newCrew(alice); // registering the device counts it for today
    const today = utcDay(h.clock.now);
    expect(h.d1.query("SELECT day FROM counters")).toEqual([{ day: today }]);
    h.clock.now += (USAGE_RETENTION_DAYS - 1) * 24 * HOUR;
    expect((await runSweep(h.app)).counter_rows_purged).toBe(0);
    h.clock.now += 2 * 24 * HOUR;
    expect((await runSweep(h.app)).counter_rows_purged).toBe(1);
    expect(h.d1.query("SELECT * FROM counters")).toEqual([]);
  });

  it("logs a one-line JSON summary with counts", async () => {
    const h = createHarness();
    const lines: string[] = [];
    const original = console.log;
    console.log = (line: string) => void lines.push(line);
    try {
      await runSweep(h.app);
    } finally {
      console.log = original;
    }
    expect(lines).toHaveLength(1);
    expect(JSON.parse(lines[0] ?? "")).toMatchObject({
      event: "sweep",
      clips_processed: 0,
      objects_deleted: 0,
      clip_rows_deleted: 0,
      signatures_purged: 0,
      invites_purged: 0,
    });
  });
});
