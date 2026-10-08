import { describe, expect, it } from "vitest";
import { createD1Db } from "../src/db.js";
import type { Db } from "../src/db.js";
import { SqliteD1 } from "./helpers/sqlite-d1.js";

const CREW = "00000000-0000-0000-0000-000000000001";
const A = "00000000-0000-0000-0000-00000000000a";
const B = "00000000-0000-0000-0000-00000000000b";
const CLIP = "00000000-0000-0000-0000-0000000000c1";
const LIMITS = { clipLimit: 1000, dayLimit: 2500 };

function fresh(): { d1: SqliteD1; db: Db } {
  const d1 = new SqliteD1();
  return { d1, db: createD1Db(d1) };
}

async function withClip(db: Db, clipId = CLIP): Promise<void> {
  await db.insertClip({ clip_id: clipId, crew_id: CREW, owner: A, created_at: 1, expires_at: 10_000 });
}

describe("reserveBytes (atomic quota accounting in SQL)", () => {
  it("reserves against both the clip and the day", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "2026-10-08", delta: 400, ...LIMITS })).toBe("ok");
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "2026-10-08", delta: 600, ...LIMITS })).toBe("ok");
    expect((await db.getClip(CLIP))?.bytes).toBe(1000);
    expect(d1.query("SELECT bytes FROM usage")).toEqual([{ bytes: 1000 }]);
  });

  it("refuses the clip limit without touching the day counter", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 900, ...LIMITS });
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 101, ...LIMITS })).toBe("clip_limit");
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 100, ...LIMITS })).toBe("ok");
    expect(d1.query("SELECT bytes FROM usage")).toEqual([{ bytes: 1000 }]);
  });

  it("refuses the day limit and rolls the clip reservation back", async () => {
    const { d1, db } = fresh();
    await withClip(db, CLIP);
    await withClip(db, "00000000-0000-0000-0000-0000000000c2");
    await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 1000, ...LIMITS });
    await db.reserveBytes({ clipId: "00000000-0000-0000-0000-0000000000c2", deviceId: A, day: "d", delta: 1000, ...LIMITS });
    // 2000 used; 501 more would be 2501 > 2500.
    const third = "00000000-0000-0000-0000-0000000000c3";
    await withClip(db, third);
    expect(await db.reserveBytes({ clipId: third, deviceId: A, day: "d", delta: 501, ...LIMITS })).toBe("day_limit");
    expect((await db.getClip(third))?.bytes).toBe(0);
    expect(d1.query("SELECT bytes FROM usage")).toEqual([{ bytes: 2000 }]);
    expect(await db.reserveBytes({ clipId: third, deviceId: A, day: "d", delta: 500, ...LIMITS })).toBe("ok");
    expect(d1.query("SELECT bytes FROM usage")).toEqual([{ bytes: 2500 }]);
  });

  it("checks the day limit even for the first write of a day", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 900, clipLimit: 1000, dayLimit: 800 })).toBe("day_limit");
    expect(d1.query("SELECT * FROM usage")).toEqual([]);
    expect((await db.getClip(CLIP))?.bytes).toBe(0);
  });

  it("keeps counters per device and per day", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d1", delta: 100, ...LIMITS });
    await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d2", delta: 100, ...LIMITS });
    await db.reserveBytes({ clipId: CLIP, deviceId: B, day: "d1", delta: 100, ...LIMITS });
    expect(d1.query("SELECT device_id, day, bytes FROM usage ORDER BY device_id, day")).toEqual([
      { device_id: A, day: "d1", bytes: 100 },
      { device_id: A, day: "d2", bytes: 100 },
      { device_id: B, day: "d1", bytes: 100 },
    ]);
  });

  it("treats zero as a no-op and never exceeds the limit under concurrency", async () => {
    const { db } = fresh();
    await withClip(db);
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 0, ...LIMITS })).toBe("ok");
    const verdicts = await Promise.all(
      Array.from({ length: 20 }, () => db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 100, ...LIMITS })),
    );
    expect(verdicts.filter((v) => v === "ok")).toHaveLength(10);
    expect((await db.getClip(CLIP))?.bytes).toBe(1000);
  });
});

describe("reserveBytes with a service-wide budget", () => {
  const GLOBAL = { ...LIMITS, globalDayLimit: 1500 };

  it("charges the global counter together with the clip and the device", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 400, ...GLOBAL })).toBe("ok");
    expect(await db.reserveBytes({ clipId: CLIP, deviceId: B, day: "d", delta: 600, ...GLOBAL })).toBe("ok");
    expect(d1.query("SELECT scope, day, n FROM counters")).toEqual([{ scope: "bytes", day: "d", n: 1000 }]);
  });

  it("rolls the clip and device reservations back when the global budget is exhausted", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    await withClip(db, "00000000-0000-0000-0000-0000000000c2");
    await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 1000, ...GLOBAL });
    const other = "00000000-0000-0000-0000-0000000000c2";
    expect(await db.reserveBytes({ clipId: other, deviceId: B, day: "d", delta: 501, ...GLOBAL })).toBe("global_limit");
    expect((await db.getClip(other))?.bytes).toBe(0);
    expect(d1.query("SELECT device_id, bytes FROM usage ORDER BY device_id")).toEqual([
      { device_id: A, bytes: 1000 },
      { device_id: B, bytes: 0 },
    ]);
    expect(d1.query("SELECT n FROM counters")).toEqual([{ n: 1000 }]);
    expect(await db.reserveBytes({ clipId: other, deviceId: B, day: "d", delta: 500, ...GLOBAL })).toBe("ok");
    expect(d1.query("SELECT n FROM counters")).toEqual([{ n: 1500 }]);
  });

  it("is skipped entirely when no global limit is given", async () => {
    const { d1, db } = fresh();
    await withClip(db);
    await db.reserveBytes({ clipId: CLIP, deviceId: A, day: "d", delta: 400, ...LIMITS });
    expect(d1.query("SELECT * FROM counters")).toEqual([]);
  });

  it("never exceeds the budget under concurrency", async () => {
    const { db } = fresh();
    const clips = Array.from({ length: 6 }, (_, i) => `00000000-0000-0000-0000-0000000000d${i}`);
    for (const clip of clips) {
      await withClip(db, clip);
    }
    const verdicts = await Promise.all(
      clips.map((clip) => db.reserveBytes({ clipId: clip, deviceId: A, day: "d", delta: 400, ...LIMITS, dayLimit: 10_000, globalDayLimit: 1000 })),
    );
    expect(verdicts.filter((v) => v === "ok")).toHaveLength(2);
    expect(verdicts.filter((v) => v === "global_limit")).toHaveLength(4);
    const total = (await Promise.all(clips.map((clip) => db.getClip(clip)))).reduce((sum, clip) => sum + (clip?.bytes ?? 0), 0);
    expect(total).toBe(800);
  });
});

describe("counters", () => {
  it("reserves up to the limit, refuses beyond it and gives amounts back", async () => {
    const { d1, db } = fresh();
    expect(await db.reserveCounter("s", "d", 3, 5)).toBe(true);
    expect(await db.reserveCounter("s", "d", 3, 5)).toBe(false);
    expect(await db.reserveCounter("s", "d", 2, 5)).toBe(true);
    expect(await db.reserveCounter("s", "d", 1, 5)).toBe(false);
    await db.releaseCounter("s", "d", 4);
    expect(d1.query("SELECT n FROM counters")).toEqual([{ n: 1 }]);
    await db.releaseCounter("s", "d", 99); // never below zero
    expect(d1.query("SELECT n FROM counters")).toEqual([{ n: 0 }]);
    await db.releaseCounter("missing", "d", 1);
    expect(d1.query("SELECT * FROM counters")).toHaveLength(1);
  });

  it("keeps scopes and days apart, and purges old days", async () => {
    const { d1, db } = fresh();
    await db.reserveCounter("a", "2026-10-01", 1, 1);
    await db.reserveCounter("a", "2026-10-08", 1, 1);
    await db.reserveCounter("b", "2026-10-08", 1, 1);
    expect(await db.reserveCounter("a", "2026-10-08", 1, 1)).toBe(false);
    expect(await db.purgeCounters("2026-10-08")).toBe(1);
    expect(d1.query("SELECT scope, day FROM counters ORDER BY scope")).toEqual([
      { scope: "a", day: "2026-10-08" },
      { scope: "b", day: "2026-10-08" },
    ]);
  });
});

describe("crew limit", () => {
  it("creates nothing once the creator reached the limit", async () => {
    const { d1, db } = fresh();
    const id = (n: number) => `00000000-0000-0000-0000-00000000010${n}`;
    expect(await db.createCrew(id(1), "one", A, 1, 2)).toBe(true);
    expect(await db.createCrew(id(2), "two", A, 2, 2)).toBe(true);
    expect(await db.createCrew(id(3), "three", A, 3, 2)).toBe(false);
    expect(await db.isMember(id(3), A)).toBe(false);
    expect(d1.query("SELECT crew_id FROM crews ORDER BY crew_id")).toEqual([{ crew_id: id(1) }, { crew_id: id(2) }]);
    expect(d1.query("SELECT crew_id FROM crew_members ORDER BY crew_id")).toHaveLength(2);
    expect(await db.createCrew(id(4), "other creator", B, 4, 2)).toBe(true);
    expect(await db.createCrew(id(5), "zero limit", B, 5, 0)).toBe(false);
  });
});

describe("active invites", () => {
  it("counts only unexpired invites with uses left", async () => {
    const { db } = fresh();
    const base = { crew_id: CREW, created_by: A, uses_left: 5 };
    await db.insertInvite({ ...base, code: "AAAAAAAAA2", expires_at: 1000 });
    await db.insertInvite({ ...base, code: "AAAAAAAAA3", expires_at: 1000, uses_left: 0 });
    await db.insertInvite({ ...base, code: "AAAAAAAAA4", expires_at: 100 });
    await db.insertInvite({ ...base, code: "AAAAAAAAA5", expires_at: 1000, crew_id: "00000000-0000-0000-0000-000000000002" });
    expect(await db.countActiveInvites(CREW, 500)).toBe(1);
    expect(await db.countActiveInvites(CREW, 50)).toBe(2);
    expect(await db.countActiveInvites(CREW, 1000)).toBe(0);
    expect(await db.countActiveInvites("00000000-0000-0000-0000-000000000009", 0)).toBe(0);
  });
});

describe("chunk accounting", () => {
  it("returns announced sizes and keeps the larger value", async () => {
    const { db } = fresh();
    await db.recordChunks(CLIP, A, "full", [[0, 10], [1, 20], [5, 50]]);
    await db.recordChunks(CLIP, A, "full", [[0, 5], [1, 30]]);
    await db.recordChunks(CLIP, A, "proxy", [[0, 99]]);
    await db.recordChunks(CLIP, B, "full", [[0, 77]]);
    expect(await db.getChunkSizes(CLIP, A, "full", [0, 1, 2, 5])).toEqual(new Map([[0, 10], [1, 30], [5, 50]]));
    expect(await db.getChunkSizes(CLIP, A, "proxy", [0, 1])).toEqual(new Map([[0, 99]]));
    expect(await db.getChunkSizes(CLIP, B, "full", [0])).toEqual(new Map([[0, 77]]));
    expect(await db.getChunkSizes(CLIP, A, "full", [])).toEqual(new Map());
    await db.recordChunks(CLIP, A, "full", []);
  });
});

describe("invites", () => {
  const invite = { code: "ABCDEFGHJK", crew_id: CREW, created_by: A, expires_at: 1000, uses_left: 2 };

  it("spends uses atomically and never below zero", async () => {
    const { db } = fresh();
    expect(await db.insertInvite(invite)).toBe(true);
    expect(await db.insertInvite(invite)).toBe(false);
    const results = await Promise.all(Array.from({ length: 6 }, () => db.consumeInvite(invite.code, 500)));
    expect(results.filter((r) => r === CREW)).toHaveLength(2);
    expect(results.filter((r) => r === null)).toHaveLength(4);
    expect((await db.getInvite(invite.code))?.uses_left).toBe(0);
  });

  it("refuses expired and unknown invites without consuming", async () => {
    const { db } = fresh();
    await db.insertInvite(invite);
    expect(await db.consumeInvite(invite.code, 1000)).toBeNull();
    expect(await db.consumeInvite(invite.code, 5000)).toBeNull();
    expect(await db.consumeInvite("NOPENOPENO", 1)).toBeNull();
    expect((await db.getInvite(invite.code))?.uses_left).toBe(2);
    expect(await db.consumeInvite(invite.code, 999)).toBe(CREW);
  });
});

describe("housekeeping queries", () => {
  it("lists expired clips oldest first with a limit and deletes them with their chunks", async () => {
    const { d1, db } = fresh();
    for (const [i, expires] of [[1, 300], [2, 100], [3, 200], [4, 900]] as const) {
      await db.insertClip({
        clip_id: `00000000-0000-0000-0000-00000000000${i}`,
        crew_id: CREW,
        owner: A,
        created_at: 1,
        expires_at: expires,
      });
      await db.recordChunks(`00000000-0000-0000-0000-00000000000${i}`, A, "full", [[0, 1]]);
    }
    const expired = await db.listExpiredClips(300, 10);
    expect(expired.map((c) => c.expires_at)).toEqual([100, 200]);
    expect((await db.listExpiredClips(1000, 3)).map((c) => c.expires_at)).toEqual([100, 200, 300]);
    expect(await db.deleteClips(expired.map((c) => c.clip_id))).toBe(2);
    expect(await db.deleteClips([])).toBe(0);
    expect(d1.query("SELECT clip_id FROM clips")).toHaveLength(2);
    expect(d1.query("SELECT clip_id FROM clip_chunks")).toHaveLength(2);
  });

  it("purges signatures, invites and usage by age", async () => {
    const { d1, db } = fresh();
    await db.recordSignature("old", 100, 10);
    await db.recordSignature("new", 900, 10);
    expect(await db.purgeSignatures(500)).toBe(1);
    expect(d1.query("SELECT sig FROM seen_signatures")).toEqual([{ sig: "new" }]);

    await db.insertInvite({ code: "EXPIREDXXX", crew_id: CREW, created_by: A, expires_at: 50, uses_left: 5 });
    await db.insertInvite({ code: "USEDUPXXXX", crew_id: CREW, created_by: A, expires_at: 5000, uses_left: 0 });
    await db.insertInvite({ code: "VALIDXXXXX", crew_id: CREW, created_by: A, expires_at: 5000, uses_left: 1 });
    expect(await db.purgeInvites(100)).toBe(2);
    expect(d1.query("SELECT code FROM invites")).toEqual([{ code: "VALIDXXXXX" }]);

    await withClip(db);
    for (const day of ["2026-10-01", "2026-10-07", "2026-10-08"]) {
      await db.reserveBytes({ clipId: CLIP, deviceId: A, day, delta: 1, clipLimit: 100, dayLimit: 100 });
    }
    expect(await db.purgeUsage("2026-10-07")).toBe(1);
    expect(d1.query("SELECT day FROM usage ORDER BY day")).toEqual([{ day: "2026-10-07" }, { day: "2026-10-08" }]);
  });

  it("creates a crew and its first member atomically", async () => {
    const { d1, db } = fresh();
    expect(await db.createCrew(CREW, "Squad", A, 5, 10)).toBe(true);
    expect(d1.query("SELECT crew_id, name, created_by, created_at FROM crews")).toEqual([
      { crew_id: CREW, name: "Squad", created_by: A, created_at: 5 },
    ]);
    expect(await db.isMember(CREW, A)).toBe(true);
    expect(await db.isMember(CREW, B)).toBe(false);
    // A duplicate crew id fails as a whole: no half-created crew.
    await expect(db.createCrew(CREW, "Again", B, 6, 10)).rejects.toThrow();
    expect(d1.query("SELECT * FROM crew_members")).toHaveLength(1);
  });

  it("marks a clip deleted once", async () => {
    const { db } = fresh();
    await withClip(db);
    await db.markClipDeleted(CLIP, 111);
    await db.markClipDeleted(CLIP, 222);
    expect((await db.getClip(CLIP))?.deleted_at).toBe(111);
  });
});
