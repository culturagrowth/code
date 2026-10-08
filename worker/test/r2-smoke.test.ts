import { describe, expect, it } from "vitest";
import { runR2Smoke } from "../tools/r2-smoke.js";

const CONFIG = { accountId: "0123456789abcdef0123456789abcdef", bucketName: "test-bucket",
  accessKeyId: "PRIVATE_ACCESS_KEY_SENTINEL", secretAccessKey: "PRIVATE_SECRET_SENTINEL" };
const missing = () => new Response("<Error><Code>NoSuchKey</Code></Error>", { status: 404 });

function storage(options: { corrupt?: boolean; acceptWrongLength?: boolean; losePutResponse?: boolean;
  deleteFails?: boolean; existing?: boolean; missingBucket?: boolean } = {}) {
  let data: Uint8Array<ArrayBuffer> | null = options.existing ? new Uint8Array([1]) : null;
  let putCount = 0;
  const calls: Array<{ method: string; url: string; init: RequestInit }> = [];
  return {
    calls,
    present: () => data !== null,
    async fetch(url: string, init: RequestInit): Promise<Response> {
      const method = init.method ?? "GET";
      calls.push({ method, url, init });
      if (options.missingBucket) return new Response("<Code>NoSuchBucket</Code>", { status: 404 });
      if (method === "PUT") {
        putCount += 1;
        if (putCount > 1 && !options.acceptWrongLength) {
          return new Response("<Error><Code>SignatureDoesNotMatch</Code></Error>", { status: 403 });
        }
        data = new Uint8Array(init.body as ArrayBuffer).slice();
        if (options.losePutResponse) throw new Error("lost response with secret in URL", { cause: { code: "ETIMEDOUT" } });
        return new Response(null, { status: 200 });
      }
      if (method === "DELETE") {
        if (options.deleteFails) return new Response(CONFIG.secretAccessKey, { status: 500 });
        data = null;
        return new Response(null, { status: 204 });
      }
      if (data === null) return missing();
      const bytes = data.slice();
      if (options.corrupt) bytes[0] = (bytes[0] ?? 0) ^ 1;
      return new Response(bytes.buffer, { status: 200 });
    },
  };
}

describe("opt-in R2 probe (offline resource-safety and redaction checks)", () => {
  it("uses the real presigner, exact upload headers, redirects/deadlines and only one fresh key", async () => {
    const fake = storage();
    const result = await runR2Smoke(CONFIG, fake.fetch);
    expect(result).toMatchObject({ ok: true, cleanup: "done" });
    expect(result.checks.map((check) => check.status)).toEqual([404, 200, 200, 403, 204, 404]);
    expect(fake.present()).toBe(false);
    const paths = new Set(fake.calls.map((call) => new URL(call.url).pathname));
    expect(paths.size).toBe(1);
    expect([...paths][0]).toMatch(/^\/test-bucket\/clips\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/[0-9a-f-]{36}\/proxy\/000000\.bin$/);
    const puts = fake.calls.filter((call) => call.method === "PUT");
    expect(puts[1]?.url).toBe(puts[0]?.url);
    for (const call of puts) {
      const headers = new Headers(call.init.headers);
      expect(headers.get("content-type")).toBe("application/octet-stream");
      expect(Number(headers.get("content-length"))).toBe((call.init.body as ArrayBuffer).byteLength);
      expect(new URL(call.url).searchParams.get("X-Amz-SignedHeaders")).toBe("content-length;content-type;host");
    }
    for (const call of fake.calls) {
      expect(call.init.redirect).toBe("error");
      expect(call.init.signal).toBeInstanceOf(AbortSignal);
    }
    expect(JSON.stringify(result)).not.toContain(CONFIG.accessKeyId);
    expect(JSON.stringify(result)).not.toContain(CONFIG.secretAccessKey);
    expect(JSON.stringify(result)).not.toContain("X-Amz");
  });

  it("does not write or delete if a key unexpectedly exists", async () => {
    const fake = storage({ existing: true });
    expect(await runR2Smoke(CONFIG, fake.fetch)).toMatchObject({ ok: false, cleanup: "not_needed",
      failure: { step: "check_absent", code: "unexpected_http_200" } });
    expect(fake.calls.map((call) => call.method)).toEqual(["GET"]);
    expect(fake.present()).toBe(true);
  });

  it("does not mistake a missing bucket for a safe empty key", async () => {
    const fake = storage({ missingBucket: true });
    expect(await runR2Smoke(CONFIG, fake.fetch)).toMatchObject({ ok: false, cleanup: "not_needed",
      failure: { step: "check_absent", code: "missing_key_not_confirmed" } });
    expect(fake.calls).toHaveLength(1);
  });

  it("redacts network errors and never attempts writes when the first request is blocked", async () => {
    const result = await runR2Smoke(CONFIG, async () => {
      throw new Error(`request to signed URL using ${CONFIG.secretAccessKey}`, { cause: { code: "EACCES" } });
    });
    expect(result).toMatchObject({ ok: false, cleanup: "not_needed", failure: { step: "check_absent", code: "EACCES" } });
    expect(JSON.stringify(result)).not.toContain(CONFIG.secretAccessKey);
  });

  it("cleans an uploaded object even if the upload response was lost", async () => {
    const fake = storage({ losePutResponse: true });
    expect(await runR2Smoke(CONFIG, fake.fetch)).toMatchObject({ ok: false, cleanup: "done",
      failure: { step: "upload", code: "ETIMEDOUT" } });
    expect(fake.present()).toBe(false);
  });

  it("detects corrupted downloads and still cleans the test object", async () => {
    const fake = storage({ corrupt: true });
    expect(await runR2Smoke(CONFIG, fake.fetch)).toMatchObject({ ok: false, cleanup: "done",
      failure: { step: "download", code: "download_mismatch" } });
    expect(fake.present()).toBe(false);
  });

  it("fails if R2 accepts a changed signed length and cleans the overwritten probe", async () => {
    const fake = storage({ acceptWrongLength: true });
    expect(await runR2Smoke(CONFIG, fake.fetch)).toMatchObject({ ok: false, cleanup: "done",
      failure: { step: "reject_wrong_length", code: "unexpected_http_200" } });
    expect(fake.present()).toBe(false);
  });

  it("reports cleanup failure without echoing credentials or raw server errors", async () => {
    const fake = storage({ deleteFails: true });
    const result = await runR2Smoke(CONFIG, fake.fetch);
    expect(result).toMatchObject({ ok: false, cleanup: "failed", failure: { step: "delete", code: "unexpected_http_500" } });
    expect(result.object_key).toMatch(/^clips\//);
    expect(JSON.stringify(result)).not.toContain(CONFIG.secretAccessKey);
    expect(JSON.stringify(result)).not.toContain(CONFIG.accessKeyId);
    expect(fake.present()).toBe(true);
  });

  it("requires the signed-length rejection, not an unrelated permission failure", async () => {
    const fake = storage();
    const result = await runR2Smoke(CONFIG, async (url, init) => {
      const response = await fake.fetch(url, init);
      return response.status === 403 ? new Response("<Code>AccessDenied</Code>", { status: 403 }) : response;
    });
    expect(result).toMatchObject({ ok: false, cleanup: "done",
      failure: { step: "reject_wrong_length", code: "length_signature_rejection_not_confirmed" } });
  });
});
