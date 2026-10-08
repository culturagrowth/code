/** Explicit, opt-in probe of the real R2 service. Never logs credentials or signed URLs. */
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { parseEnv } from "node:util";
import { AwsClient } from "aws4fetch";
import { objectKey } from "../src/keys.js";
import { createPresigner, objectUrl, presignConfigFromEnv, UPLOAD_CONTENT_TYPE } from "../src/presign.js";
import type { PresignConfig } from "../src/presign.js";

type HttpFetch = (url: string, init: RequestInit) => Promise<Response>;
type Step = "check_absent" | "upload" | "download" | "reject_wrong_length" | "delete" | "check_deleted";

export interface ProbeResult {
  ok: boolean;
  checks: Array<{ step: Step; status: number }>;
  cleanup: "not_needed" | "done" | "failed";
  failure?: { step: Step; code: string };
  /** Only shown when cleanup failed, so the caller can remove this exact test object. */
  object_key?: string;
}

class ProbeError extends Error {
  constructor(readonly code: string) { super(code); }
}

/** Removes details that could contain signed URLs, headers, credentials or server response bodies. */
function safeCode(error: unknown): string {
  if (error instanceof ProbeError) return error.code;
  const cause = error instanceof Error ? (error.cause as { code?: unknown } | undefined)?.code : undefined;
  return typeof cause === "string" && ["EACCES", "ENOTFOUND", "ECONNREFUSED", "ETIMEDOUT", "ECONNRESET"].includes(cause)
    ? cause : "request_failed";
}

/** Tests the actual Worker presigner; touches one fresh object and attempts cleanup even after a lost PUT response. */
export async function runR2Smoke(
  config: PresignConfig,
  httpFetch: HttpFetch = (url, init) => fetch(url, init),
): Promise<ProbeResult> {
  const presigner = createPresigner(config);
  const client = new AwsClient({ accessKeyId: config.accessKeyId,
    secretAccessKey: config.secretAccessKey, service: "s3", region: "auto" });
  const key = objectKey(randomUUID(), randomUUID(), randomUUID(), "proxy", 0);
  const cipherKey = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt"]) as CryptoKey;
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: crypto.getRandomValues(new Uint8Array(12)) }, cipherKey,
    new TextEncoder().encode("DuoClip R2 connectivity probe"),
  ));
  const result: ProbeResult = { ok: false, checks: [], cleanup: "not_needed" };
  let step: Step = "check_absent";
  let putAttempted = false;
  const request = async (url: string, init: RequestInit): Promise<Response> => {
    const response = await httpFetch(url, { ...init, signal: AbortSignal.timeout(15_000), redirect: "error" });
    result.checks.push({ step, status: response.status });
    return response;
  };
  const status = (response: Response, expected: number): void => {
    if (response.status !== expected) throw new ProbeError(`unexpected_http_${response.status}`);
  };
  const getUrl = await presigner.presign("GET", key);
  try {
    const absent = await request(getUrl, { method: "GET" });
    status(absent, 404);
    // A missing bucket also returns 404; only a missing object permits the write.
    if (!/<Code>NoSuchKey<\/Code>/.test(await absent.text())) throw new ProbeError("missing_key_not_confirmed");
    const putUrl = await presigner.presign("PUT", key, { contentLength: ciphertext.byteLength });
    step = "upload";
    putAttempted = true;
    const uploaded = await request(putUrl, { method: "PUT", headers: {
      "content-type": UPLOAD_CONTENT_TYPE, "content-length": String(ciphertext.byteLength),
    }, body: ciphertext.buffer });
    status(uploaded, 200);
    await uploaded.arrayBuffer();
    step = "download";
    const downloaded = await request(getUrl, { method: "GET" });
    status(downloaded, 200);
    const bytes = new Uint8Array(await downloaded.arrayBuffer());
    if (bytes.length !== ciphertext.length || bytes.some((value, i) => value !== ciphertext[i])) {
      throw new ProbeError("download_mismatch");
    }
    step = "reject_wrong_length";
    const wrong = new Uint8Array(ciphertext.length + 1);
    wrong.set(ciphertext);
    const rejected = await request(putUrl, { method: "PUT", headers: {
      "content-type": UPLOAD_CONTENT_TYPE, "content-length": String(wrong.length),
    }, body: wrong.buffer });
    status(rejected, 403);
    const errorBody = await rejected.text();
    if (!/<Code>SignatureDoesNotMatch<\/Code>/.test(errorBody)) {
      throw new ProbeError("length_signature_rejection_not_confirmed");
    }
    result.ok = true;
  } catch (error) {
    result.failure = { step, code: safeCode(error) };
  } finally {
    if (putAttempted) {
      try {
        step = "delete";
        const signed = await client.sign(objectUrl(config.accountId, config.bucketName, key), { method: "DELETE" });
        const deleted = await request(signed.url, { method: "DELETE", headers: signed.headers });
        status(deleted, 204);
        await deleted.arrayBuffer();
        step = "check_deleted";
        const absent = await request(getUrl, { method: "GET" });
        status(absent, 404);
        if (!/<Code>NoSuchKey<\/Code>/.test(await absent.text())) throw new ProbeError("deletion_not_confirmed");
        result.cleanup = "done";
      } catch (error) {
        result.ok = false;
        result.cleanup = "failed";
        result.object_key = key;
        result.failure ??= { step, code: safeCode(error) };
      }
    }
  }
  return result;
}

async function main(): Promise<void> {
  const args = process.argv.slice(2);
  if (args.length !== 0 && !(args.length === 2 && args[0] === "--env-file")) {
    console.error("Usage: npm run test:r2 -- [--env-file path/to/.dev.vars]");
    process.exitCode = 1;
    return;
  }
  try {
    const values = parseEnv(readFileSync(resolve(args[1] ?? ".dev.vars"), "utf8"));
    const config = presignConfigFromEnv(values);
    if ([config.accessKeyId, config.secretAccessKey].some((value) => value === "replace-me")) {
      throw new ProbeError("credentials_not_configured");
    }
    const result = await runR2Smoke(config);
    console.log(JSON.stringify(result, null, 2));
    if (!result.ok) process.exitCode = 1;
  } catch {
    console.error("Unable to load a valid local configuration or start the R2 probe. No credentials were printed.");
    process.exitCode = 1;
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  await main();
}
