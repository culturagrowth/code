/** Worker bindings, variables and secrets (see `wrangler.toml` and README.md). */
export interface Env {
  /** R2 bucket binding, used to list and delete objects. */
  CLIPS: R2Bucket;
  /** D1 database binding. */
  DB: D1Database;
  /** Cloudflare account id (variable); part of the R2 S3 endpoint host. */
  ACCOUNT_ID: string;
  /** R2 bucket name (variable); first path segment of presigned URLs. */
  BUCKET_NAME: string;
  /** R2 S3 API access key id (secret). */
  R2_ACCESS_KEY_ID: string;
  /** R2 S3 API secret access key (secret). */
  R2_SECRET_ACCESS_KEY: string;
  /** Optional variable: new devices that may register per UTC day (default 50; `0` closes registration). */
  MAX_NEW_DEVICES_PER_DAY?: string | number;
  /** Optional variable: bytes all devices together may announce per UTC day (default 200 GiB). */
  MAX_GLOBAL_DAILY_BYTES?: string | number;
}
