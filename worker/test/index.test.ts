import { describe, expect, it } from "vitest";
import worker from "../src/index.js";
import type { Env } from "../src/env.js";
import { presignConfigFromEnv } from "../src/presign.js";
import { secondsUntilNextUtcDay } from "../src/quota.js";
import type { R2BucketLike } from "../src/store.js";
import { ORIGIN, createHarness, joinCrew, newCrew, T0 } from "./helpers/harness.js";
import { SqliteD1 } from "./helpers/sqlite-d1.js";

/** A minimal R2 bucket: sorted keys, `limit`, `truncated`, array delete. */
class FakeBucket implements R2BucketLike {
  readonly keys = new Set<string>();
  list(options: { prefix: string; limit: number }) {
    const all = [...this.keys].filter((key) => key.startsWith(options.prefix)).sort();
    return Promise.resolve({
      objects: all.slice(0, options.limit).map((key) => ({ key })),
      truncated: all.length > options.limit,
    });
  }
  delete(keys: string[]) {
    for (const key of keys) {
      this.keys.delete(key);
    }
    return Promise.resolve();
  }
}

const GOOD_ENV = {
  ACCOUNT_ID: "0123456789abcdef0123456789abcdef",
  BUCKET_NAME: "duoclip-clips",
  R2_ACCESS_KEY_ID: "AKIAEXAMPLE",
  R2_SECRET_ACCESS_KEY: "hunter2-secret",
};

/** Calls the Worker's fetch handler (the `cf` request typing is irrelevant outside Cloudflare). */
function viaWorker(env: Env): (request: Request) => Promise<Response> {
  return (request) => worker.fetch(request as never, env);
}

function makeEnv(overrides: Partial<typeof GOOD_ENV> = {}): { env: Env; bucket: FakeBucket; d1: SqliteD1 } {
  const bucket = new FakeBucket();
  const d1 = new SqliteD1();
  return { env: { ...GOOD_ENV, ...overrides, CLIPS: bucket, DB: d1 } as unknown as Env, bucket, d1 };
}

describe("Worker entry point wiring", () => {
  it("serves health through fetch()", async () => {
    const { env } = makeEnv();
    const res = await viaWorker(env)(new Request(`${ORIGIN}/v1/health`));
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ ok: true });
  });

  it("runs the whole flow against D1 and presigns with the configured secrets", async () => {
    const { env } = makeEnv();
    // Reuse the signing test devices but route requests through the real entry point.
    const h = createHarness();
    h.send = viaWorker(env);
    h.clock.now = Date.now();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clip = "00000000-0000-4000-8000-0000000000c1";
    const reg = await alice.call("POST", "/v1/clips", { clip_id: clip, crew_id: crew });
    expect(reg.status).toBe(201);
    const res = await alice.call("POST", `/v1/clips/${clip}/upload-urls`, {
      pov: alice.id,
      quality: "full",
      indices: [0],
      sizes: [10],
    });
    expect(res.status).toBe(200);
    const body = (await res.json()) as { chunks: Array<{ url: string }> };
    const url = new URL(body.chunks[0]?.url ?? "");
    expect(url.host).toBe(`${GOOD_ENV.ACCOUNT_ID}.r2.cloudflarestorage.com`);
    expect(url.searchParams.get("X-Amz-Credential")).toContain(GOOD_ENV.R2_ACCESS_KEY_ID);
    expect(url.toString()).not.toContain(GOOD_ENV.R2_SECRET_ACCESS_KEY);
  });

  it("fails presigning routes with an opaque 500 when configuration is wrong, but keeps other routes working", async () => {
    const { env } = makeEnv({ ACCOUNT_ID: "REPLACE_WITH_CLOUDFLARE_ACCOUNT_ID" });
    const h = createHarness();
    h.send = viaWorker(env);
    h.clock.now = Date.now();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clip = "00000000-0000-4000-8000-0000000000c2";
    await alice.call("POST", "/v1/clips", { clip_id: clip, crew_id: crew });

    const errors: string[] = [];
    const original = console.error;
    console.error = (line: string) => void errors.push(line);
    try {
      const res = await alice.call("POST", `/v1/clips/${clip}/download-urls`, {
        pov: alice.id,
        quality: "full",
        indices: [0],
      });
      expect(res.status).toBe(500);
      expect(await res.text()).not.toContain("ACCOUNT_ID");
    } finally {
      console.error = original;
    }
    // The cause is logged for the operator (variable name only, never a value).
    expect(errors.join("\n")).toContain("ACCOUNT_ID must be");
    expect(errors.join("\n")).not.toContain("REPLACE_WITH");
    expect((await alice.call("GET", `/v1/crews/${crew}/members`)).status).toBe(200);
  });

  it("runs the sweep from scheduled() and deletes expired clips through the R2 binding", async () => {
    const { env, bucket, d1 } = makeEnv();
    const h = createHarness();
    h.send = viaWorker(env);
    h.clock.now = Date.now();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const clip = "00000000-0000-4000-8000-0000000000c3";
    await alice.call("POST", "/v1/clips", { clip_id: clip, crew_id: crew, ttl_s: 1 });
    const key = `clips/${crew}/${clip}/${alice.id}/full/000000.bin`;
    const other = `clips/${crew}/00000000-0000-4000-8000-0000000000c4/${alice.id}/full/000000.bin`;
    bucket.keys.add(key);
    bucket.keys.add(other);
    // Make the clip look expired long ago (past the 15 minute presign horizon).
    d1.database.prepare("UPDATE clips SET expires_at = ?").run(Date.now() - 3_600_000);

    const logs: string[] = [];
    const original = console.log;
    console.log = (line: string) => void logs.push(line);
    try {
      await worker.scheduled({ cron: "0 * * * *", scheduledTime: Date.now() } as never, env);
    } finally {
      console.log = original;
    }
    expect([...bucket.keys]).toEqual([other]);
    expect(d1.query("SELECT * FROM clips")).toHaveLength(0);
    expect(JSON.parse(logs[0] ?? "{}")).toMatchObject({ event: "sweep", clips_processed: 1, objects_deleted: 1, clip_rows_deleted: 1 });
  });
});

describe("presignConfigFromEnv", () => {
  it("accepts a valid environment", () => {
    expect(presignConfigFromEnv(GOOD_ENV)).toEqual({
      accountId: GOOD_ENV.ACCOUNT_ID,
      bucketName: GOOD_ENV.BUCKET_NAME,
      accessKeyId: GOOD_ENV.R2_ACCESS_KEY_ID,
      secretAccessKey: GOOD_ENV.R2_SECRET_ACCESS_KEY,
    });
  });

  it.each([
    ["placeholder account id", { ACCOUNT_ID: "REPLACE_WITH_CLOUDFLARE_ACCOUNT_ID" }, /ACCOUNT_ID/],
    ["uppercase account id", { ACCOUNT_ID: "0123456789ABCDEF0123456789ABCDEF" }, /ACCOUNT_ID/],
    ["missing account id", { ACCOUNT_ID: undefined }, /ACCOUNT_ID/],
    ["bad bucket", { BUCKET_NAME: "Bad_Bucket" }, /BUCKET_NAME/],
    ["path-like bucket", { BUCKET_NAME: "a/b" }, /BUCKET_NAME/],
    ["missing key id", { R2_ACCESS_KEY_ID: "" }, /R2_ACCESS_KEY_ID/],
    ["missing secret", { R2_SECRET_ACCESS_KEY: undefined }, /R2_SECRET_ACCESS_KEY/],
  ])("rejects %s without echoing values", (_name, patch, message) => {
    let thrown: unknown;
    try {
      presignConfigFromEnv({ ...GOOD_ENV, ...patch });
    } catch (error) {
      thrown = error;
    }
    expect(thrown).toBeInstanceOf(Error);
    expect((thrown as Error).message).toMatch(message);
    expect((thrown as Error).message).not.toContain(GOOD_ENV.R2_SECRET_ACCESS_KEY);
  });
});

describe("Retry-After on the daily quota", () => {
  it("counts the seconds to the next UTC midnight", () => {
    expect(secondsUntilNextUtcDay(Date.UTC(2026, 9, 8, 23, 59, 0))).toBe(60);
    expect(secondsUntilNextUtcDay(Date.UTC(2026, 9, 8, 0, 0, 0))).toBe(86_400);
    expect(secondsUntilNextUtcDay(T0)).toBe(12 * 3600);
    expect(secondsUntilNextUtcDay(Date.UTC(2026, 9, 8, 23, 59, 59, 999))).toBe(1);
  });

  it("is sent with the 429 response", async () => {
    const h = createHarness();
    const alice = await h.device(1);
    const crew = await newCrew(alice);
    const bob = await h.device(2);
    await joinCrew(alice, bob, crew);
    const clip = "00000000-0000-4000-8000-0000000000c5";
    await alice.call("POST", "/v1/clips", { clip_id: clip, crew_id: crew });
    h.d1.database.prepare("INSERT INTO usage (device_id, day, bytes) VALUES (?, '2026-10-08', ?)").run(alice.id, 10 * 1024 ** 3);
    const res = await alice.call("POST", `/v1/clips/${clip}/upload-urls`, { pov: alice.id, quality: "full", indices: [0], sizes: [1] });
    expect(res.status).toBe(429);
    expect(res.headers.get("retry-after")).toBe(String(secondsUntilNextUtcDay(h.clock.now)));
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  });
});
