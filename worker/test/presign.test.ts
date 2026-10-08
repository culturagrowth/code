import { createHash, createHmac } from "node:crypto";
import { describe, expect, it } from "vitest";
import { manifestKey, objectKey } from "../src/keys.js";
import { amzDate, createPresigner, objectUrl, PRESIGN_EXPIRES_S } from "../src/presign.js";

const CREW = "00000000-0000-0000-0000-000000000001";
const CLIP = "00000000-0000-0000-0000-000000000002";
const POV = "00000000-0000-0000-0000-000000000003";
const NOW = Date.UTC(2026, 9, 8, 12, 30, 45, 123);

const config = {
  accountId: "0123456789abcdef0123456789abcdef",
  bucketName: "duoclip-clips",
  accessKeyId: "AKIAEXAMPLEKEYID",
  secretAccessKey: "example/secret+access/key",
};

/** Independent SigV4 query-signature computation, straight from the AWS specification. */
function referenceSignature(method: string, url: URL, signedHeaders: Record<string, string>): string {
  const params = [...url.searchParams].filter(([name]) => name !== "X-Amz-Signature");
  const canonicalQuery = params
    .map(([k, v]) => [encodeURIComponent(k), encodeURIComponent(v)] as const)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))
    .map(([k, v]) => `${k}=${v}`)
    .join("&");
  const headers: Record<string, string> = { ...signedHeaders, host: url.host };
  const names = Object.keys(headers).sort();
  const canonicalHeaders = names.map((name) => `${name}:${headers[name]}\n`).join("");
  const canonicalRequest = [
    method,
    url.pathname,
    canonicalQuery,
    canonicalHeaders,
    names.join(";"),
    "UNSIGNED-PAYLOAD",
  ].join("\n");
  const datetime = url.searchParams.get("X-Amz-Date") ?? "";
  const date = datetime.slice(0, 8);
  const scope = `${date}/auto/s3/aws4_request`;
  const stringToSign = [
    "AWS4-HMAC-SHA256",
    datetime,
    scope,
    createHash("sha256").update(canonicalRequest).digest("hex"),
  ].join("\n");
  const hmac = (key: Buffer | string, data: string) => createHmac("sha256", key).update(data).digest();
  const signingKey = hmac(hmac(hmac(hmac(`AWS4${config.secretAccessKey}`, date), "auto"), "s3"), "aws4_request");
  return createHmac("sha256", signingKey).update(stringToSign).digest("hex");
}

describe("presigned URLs", () => {
  const presigner = createPresigner(config, () => NOW);
  const key = objectKey(CREW, CLIP, POV, "full", 7);
  const SIZE = 5_000_000;
  const put = (target = key, contentLength = SIZE) => presigner.presign("PUT", target, { contentLength });

  it("targets the R2 S3 endpoint with bucket and key in the path", async () => {
    const url = new URL(await put());
    expect(url.protocol).toBe("https:");
    expect(url.host).toBe(`${config.accountId}.r2.cloudflarestorage.com`);
    expect(url.pathname).toBe(`/${config.bucketName}/${key}`);
    expect(objectUrl(config.accountId, config.bucketName, key)).toBe(`${url.origin}${url.pathname}`);
  });

  it("expires after 900 seconds and carries SigV4 query parameters", async () => {
    expect(PRESIGN_EXPIRES_S).toBe(900);
    for (const method of ["GET", "PUT"] as const) {
      const url = new URL(method === "PUT" ? await put() : await presigner.presign(method, key));
      expect(url.searchParams.get("X-Amz-Expires")).toBe("900");
      expect(url.searchParams.get("X-Amz-Algorithm")).toBe("AWS4-HMAC-SHA256");
      expect(url.searchParams.get("X-Amz-Date")).toBe("20261008T123045Z");
      expect(url.searchParams.get("X-Amz-Credential")).toBe(
        `${config.accessKeyId}/20261008/auto/s3/aws4_request`,
      );
      expect(url.searchParams.get("X-Amz-Signature")).toMatch(/^[0-9a-f]{64}$/);
      // Secrets never appear in the URL.
      expect(url.toString()).not.toContain(config.secretAccessKey);
    }
  });

  it("signs Content-Type and Content-Length for uploads and only the host for downloads", async () => {
    const upload = new URL(await put());
    expect(upload.searchParams.get("X-Amz-SignedHeaders")).toBe("content-length;content-type;host");
    const get = new URL(await presigner.presign("GET", key));
    expect(get.searchParams.get("X-Amz-SignedHeaders")).toBe("host");
  });

  it("binds an upload URL to the exact announced size", async () => {
    const a = new URL(await put(key, 1000));
    const b = new URL(await put(key, 1001));
    expect(a.searchParams.get("X-Amz-Signature")).not.toBe(b.searchParams.get("X-Amz-Signature"));
    expect(a.searchParams.get("X-Amz-Signature")).toBe(
      referenceSignature("PUT", a, { "content-type": "application/octet-stream", "content-length": "1000" }),
    );
  });

  it("never mints an upload URL without a valid size bound", async () => {
    await expect(presigner.presign("PUT", key)).rejects.toThrow(RangeError);
    for (const contentLength of [0, -1, 1.5, Number.NaN, Number.POSITIVE_INFINITY, 64 * 1024 * 1024 + 1]) {
      await expect(presigner.presign("PUT", key, { contentLength })).rejects.toThrow(RangeError);
    }
    await expect(presigner.presign("PUT", key, { contentLength: 64 * 1024 * 1024 })).resolves.toContain("X-Amz-Signature=");
    await expect(presigner.presign("PUT", key, { contentLength: 1 })).resolves.toContain("X-Amz-Signature=");
  });

  it("does not sign a size into download URLs, even when one is passed", async () => {
    const get = new URL(await presigner.presign("GET", key, { contentLength: 5 }));
    expect(get.searchParams.get("X-Amz-SignedHeaders")).toBe("host");
    expect(get.searchParams.get("X-Amz-Signature")).toBe(referenceSignature("GET", get, {}));
  });

  it("produces the signature an independent SigV4 implementation computes", async () => {
    const upload = new URL(await put());
    expect(upload.searchParams.get("X-Amz-Signature")).toBe(
      referenceSignature("PUT", upload, {
        "content-type": "application/octet-stream",
        "content-length": String(SIZE),
      }),
    );
    const get = new URL(await presigner.presign("GET", manifestKey(CREW, CLIP, POV, "proxy")));
    expect(get.searchParams.get("X-Amz-Signature")).toBe(referenceSignature("GET", get, {}));
  });

  it("binds the signature to the method and the key", async () => {
    const upload = new URL(await put());
    const get = new URL(await presigner.presign("GET", key));
    const other = new URL(await put(objectKey(CREW, CLIP, POV, "full", 8)));
    const signatures = new Set([upload, get, other].map((url) => url.searchParams.get("X-Amz-Signature")));
    expect(signatures.size).toBe(3);
  });

  it("is deterministic for a fixed clock and follows the clock otherwise", async () => {
    expect(await presigner.presign("GET", key)).toBe(await presigner.presign("GET", key));
    const later = createPresigner(config, () => NOW + 60_000);
    expect(await later.presign("GET", key)).not.toBe(await presigner.presign("GET", key));
  });

  it("refuses keys that break the structural rules", async () => {
    for (const bad of ["clips/../x", "other/x", "clips//x", "clips/a b"]) {
      await expect(presigner.presign("GET", bad)).rejects.toThrow(RangeError);
    }
  });

  it("refuses to be built with missing credentials", () => {
    for (const field of Object.keys(config) as Array<keyof typeof config>) {
      expect(() => createPresigner({ ...config, [field]: "" })).toThrow(/missing/);
    }
  });
});

describe("amzDate", () => {
  it("formats SigV4 timestamps", () => {
    expect(amzDate(NOW)).toBe("20261008T123045Z");
    expect(amzDate(0)).toBe("19700101T000000Z");
  });
});
