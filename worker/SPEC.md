# DuoClip Worker — SPEC (Cloudflare Worker + R2 + D1)

Minimal backend for a private group of friends. It **never sees clip content in clear**: clips are encrypted
end-to-end by the apps (duoclip-crypto), and the Worker only authenticates devices, manages crews (friend groups),
mints short-lived presigned R2 URLs, and deletes expired clips. See docs section 10.

## Stack

- TypeScript, Cloudflare Workers (module syntax), `wrangler` for dev/deploy.
- R2 bucket binding `CLIPS` (for list/delete), D1 binding `DB`, and a cron trigger `0 * * * *` (hourly sweep).
- Presigned URLs use the S3 API of R2 via `aws4fetch` (`AwsClient.sign(..., { aws: { signQuery: true } })`) against
  `https://{ACCOUNT_ID}.r2.cloudflarestorage.com/{BUCKET_NAME}/{key}`, with `X-Amz-Expires=900` (15 min).
- Secrets/vars: `ACCOUNT_ID`, `BUCKET_NAME`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`.
- Tests: `vitest` in a Node environment (Node 22 has WebCrypto Ed25519). Test pure modules (auth canonicalization and
  verification, key building and validation, quotas, presign URL shape, request validation). Route handlers take
  injected `Env`-like interfaces so they can be tested with simple in-memory fakes for D1/R2. Do NOT require
  network or wrangler login to run tests. Also run `tsc --noEmit`.

## Layout

```
worker/
  package.json        scripts: "typecheck": "tsc --noEmit", "test": "vitest run", "dev": "wrangler dev"
  tsconfig.json       strict
  wrangler.toml       name="duoclip-worker", main="src/index.ts", compatibility_date, r2_buckets, d1_databases, triggers.crons
  migrations/0001_init.sql
  src/index.ts        fetch + scheduled handlers, router
  src/auth.ts         device request signing/verification
  src/keys.ts         object key naming/validation (must match crates/duoclip-proto object_key/manifest_key exactly)
  src/presign.ts      aws4fetch presigning
  src/db.ts           typed D1 queries (interface so tests can fake it)
  src/quota.ts
  src/validate.ts     body validation (no external schema lib needed; small hand-written validators are fine)
  test/*.test.ts
  README.md           (Portuguese) how to create the bucket, D1 DB, secrets, deploy, and the lifecycle backstop rule
```

## Data model (D1)

```sql
devices(device_id TEXT PRIMARY KEY, public_key_b64 TEXT NOT NULL, display_name TEXT NOT NULL, created_at INTEGER NOT NULL)
crews(crew_id TEXT PRIMARY KEY, name TEXT NOT NULL, created_by TEXT NOT NULL, created_at INTEGER NOT NULL)
crew_members(crew_id TEXT, device_id TEXT, joined_at INTEGER, PRIMARY KEY(crew_id, device_id))
invites(code TEXT PRIMARY KEY, crew_id TEXT NOT NULL, created_by TEXT NOT NULL, expires_at INTEGER NOT NULL, uses_left INTEGER NOT NULL)
clips(clip_id TEXT PRIMARY KEY, crew_id TEXT NOT NULL, owner TEXT NOT NULL, created_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL, deleted_at INTEGER)
usage(device_id TEXT, day TEXT, bytes INTEGER, PRIMARY KEY(device_id, day))
seen_signatures(sig TEXT PRIMARY KEY, seen_at INTEGER NOT NULL)
```

## Authentication

- Each device has an Ed25519 key pair. The private key stays on the PC (DPAPI).
- `POST /v1/devices` `{device_id(uuid), public_key_b64 (32 bytes raw), display_name (1..32)}` is the only unsigned
  route. It is idempotent for the same key, and returns 409 if the device_id exists with a different key.
- Every other route requires these headers:
  - `X-DC-Device`: device id;
  - `X-DC-Timestamp`: unix ms;
  - `X-DC-Signature`: base64 of the Ed25519 signature over the canonical string
    `"DC1\n" + METHOD + "\n" + PATH (with query) + "\n" + TIMESTAMP + "\n" + hex(sha256(body))`.
- Reject a timestamp skew over ±5 min. Replay protection: store the signature in `seen_signatures` and reject a duplicate within 10 min.
- Use constant-time comparisons where relevant. Use 401 for auth failures and 403 for membership failures. Never echo secrets.

## Routes (JSON in/out; validate everything; max body 16 KiB)

| Method & path | Who | Behavior |
|---|---|---|
| `POST /v1/crews` `{name}` | any device | Create a crew, with the creator as a member. Returns `{crew_id}`. |
| `POST /v1/crews/:crew/invites` | member | Create an invite: a random code of 10 chars `[A-Z2-9]`, valid 24 h, 5 uses. Returns `{code, expires_at}`. |
| `POST /v1/crews/join` `{code}` | any device | Join if the code is valid and has uses left (decrement atomically). Returns `{crew_id}`. |
| `GET /v1/crews/:crew/members` | member | `[{device_id, display_name}]` |
| `POST /v1/clips` `{clip_id, crew_id, ttl_s}` | member | Register a clip (`ttl_s` ≤ 259200 = 72 h, default 72 h). Idempotent for the same owner. |
| `POST /v1/clips/:clip/upload-urls` `{pov, quality, indices[], sizes[], manifest}` | member, and `pov` == caller | Presigned **PUT** URLs for the chunk keys (+ manifest key if requested). At most 64 indices, each size ≤ 64 MiB. Per-clip total ≤ 1.5 GiB. Per device per UTC day ≤ 10 GiB (usage table). Content-Type `application/octet-stream` is signed. |
| `POST /v1/clips/:clip/download-urls` `{pov, quality, indices[], manifest}` | member | Presigned **GET** URLs |
| `DELETE /v1/clips/:clip` | owner or any member | Delete all objects under `clip_prefix` (R2 list + delete in batches) and mark the clip deleted |
| `GET /v1/health` | public | `{ok:true}` |

`scheduled()` runs hourly:
- delete clips whose `expires_at < now` (R2 objects first, then the row);
- purge `seen_signatures` older than 15 min;
- delete expired invites;
- log counts.

Documented in the README as a backstop: an R2 lifecycle rule on prefix `clips/` that expires objects after 3 days and aborts incomplete
multipart uploads after 1 day.

Key format (must match the Rust crate exactly):
- `clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin`
- `clips/{crew}/{clip}/{pov}/{proxy|full}/manifest.bin`

All uuids are lowercase and hyphenated.

## Tests (required)

- Canonical string and signature verification: a valid signature passes. A modified body, path, method or timestamp fails. Skew is rejected, and replay is rejected.
- Key building matches golden strings identical to the Rust tests (`00000000-...` style uuids). Validation rejects malformed input.
- Quota math: the daily limit, the per-clip limit and the 64-index cap.
- Presign: the URL has `X-Amz-Expires=900`, the method is right, and the path is the expected key.
- Routes, using in-memory fakes: membership enforcement (403), the pov ≠ caller upload rejection, invite expiry and uses, and idempotent clip registration.
- `npm run typecheck` and `npm test` both pass.
