/**
 * Presigned URLs for the R2 S3 API (SigV4 query signing through `aws4fetch`).
 *
 * URLs look like `https://{ACCOUNT_ID}.r2.cloudflarestorage.com/{BUCKET_NAME}/{key}` and are
 * valid for 15 minutes (`X-Amz-Expires=900`). For PUT, the `Content-Type` and `Content-Length`
 * headers are part of the signature, so the uploader must send exactly
 * `application/octet-stream` and exactly the announced number of bytes: the size the quotas were
 * charged for cannot be exceeded through the URL.
 */

import { AwsClient } from "aws4fetch";
import { validateObjectKey } from "./keys.js";
import { MAX_CHUNK_BYTES } from "./quota.js";

/** Lifetime of every presigned URL, in seconds. */
export const PRESIGN_EXPIRES_S = 900;

/** Content type that is signed into upload URLs. */
export const UPLOAD_CONTENT_TYPE = "application/octet-stream";

/** HTTP methods the Worker presigns. */
export type PresignMethod = "GET" | "PUT";

/** R2 S3 API credentials and location. */
export interface PresignConfig {
  accountId: string;
  bucketName: string;
  accessKeyId: string;
  secretAccessKey: string;
}

/** Extra input of {@link Presigner.presign}. */
export interface PresignOptions {
  /**
   * Exact body size, in bytes, that a PUT must send. Required for PUT (an upload URL without a
   * size bound is refused); ignored for GET.
   */
  contentLength?: number;
}

/** Produces presigned URLs for object keys. */
export interface Presigner {
  /**
   * Returns a URL that allows `method` on `key` for {@link PRESIGN_EXPIRES_S} seconds.
   *
   * @throws {RangeError} for a malformed key, or a PUT without a valid `contentLength`.
   */
  presign(method: PresignMethod, key: string, options?: PresignOptions): Promise<string>;
}

const ACCOUNT_ID_RE = /^[0-9a-f]{32}$/;
const BUCKET_NAME_RE = /^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$/;

/** The Worker variables and secrets that configure presigning. */
export interface PresignEnv {
  ACCOUNT_ID: string;
  BUCKET_NAME: string;
  R2_ACCESS_KEY_ID: string;
  R2_SECRET_ACCESS_KEY: string;
}

/**
 * Builds and sanity-checks a {@link PresignConfig} from the Worker environment.
 *
 * Throws an `Error` naming the offending variable (never its value) so that a forgotten
 * `wrangler.toml` placeholder or missing secret is obvious in the logs instead of producing
 * URLs that fail later at R2.
 */
export function presignConfigFromEnv(env: Partial<PresignEnv>): PresignConfig {
  if (typeof env.ACCOUNT_ID !== "string" || !ACCOUNT_ID_RE.test(env.ACCOUNT_ID)) {
    throw new Error("ACCOUNT_ID must be the 32-character hex Cloudflare account id");
  }
  if (typeof env.BUCKET_NAME !== "string" || !BUCKET_NAME_RE.test(env.BUCKET_NAME)) {
    throw new Error("BUCKET_NAME must be a valid R2 bucket name");
  }
  if (!env.R2_ACCESS_KEY_ID) {
    throw new Error("secret R2_ACCESS_KEY_ID is not set");
  }
  if (!env.R2_SECRET_ACCESS_KEY) {
    throw new Error("secret R2_SECRET_ACCESS_KEY is not set");
  }
  return {
    accountId: env.ACCOUNT_ID,
    bucketName: env.BUCKET_NAME,
    accessKeyId: env.R2_ACCESS_KEY_ID,
    secretAccessKey: env.R2_SECRET_ACCESS_KEY,
  };
}

/** The unsigned S3 endpoint URL of an object. */
export function objectUrl(accountId: string, bucketName: string, key: string): string {
  return `https://${accountId}.r2.cloudflarestorage.com/${bucketName}/${key}`;
}

/** Formats unix milliseconds as the SigV4 `X-Amz-Date` value (`YYYYMMDDTHHMMSSZ`). */
export function amzDate(unixMs: number): string {
  return new Date(unixMs).toISOString().replace(/[:-]|\.\d{3}/g, "");
}

/**
 * Creates a {@link Presigner}.
 *
 * @param config R2 credentials. Throws if any value is empty (a missing secret must fail loudly).
 * @param now Clock in unix milliseconds; defaults to `Date.now`.
 */
export function createPresigner(
  config: PresignConfig,
  now: () => number = () => Date.now(),
): Presigner {
  for (const [name, value] of Object.entries(config)) {
    if (typeof value !== "string" || value.length === 0) {
      throw new Error(`presign configuration value ${name} is missing`);
    }
  }
  const client = new AwsClient({
    accessKeyId: config.accessKeyId,
    secretAccessKey: config.secretAccessKey,
    service: "s3",
    region: "auto",
  });

  return {
    async presign(method, key, options = {}) {
      const problem = validateObjectKey(key);
      if (problem !== null) {
        throw new RangeError(problem);
      }
      const { contentLength } = options;
      if (method === "PUT") {
        if (
          contentLength === undefined ||
          !Number.isSafeInteger(contentLength) ||
          contentLength < 1 ||
          contentLength > MAX_CHUNK_BYTES
        ) {
          throw new RangeError("an upload URL needs an exact contentLength in 1..=64 MiB");
        }
      }
      const url = new URL(objectUrl(config.accountId, config.bucketName, key));
      url.searchParams.set("X-Amz-Expires", String(PRESIGN_EXPIRES_S));
      const signed = await client.sign(url.toString(), {
        method,
        // Content-Type and Content-Length are only signed for uploads; downloads sign just the host.
        ...(method === "PUT"
          ? {
              headers: {
                "Content-Type": UPLOAD_CONTENT_TYPE,
                "Content-Length": String(contentLength),
              },
            }
          : {}),
        aws: { signQuery: true, allHeaders: true, datetime: amzDate(now()) },
      });
      return signed.url;
    },
  };
}
