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
- Current deployment uses the existing dedicated `povclip` bucket (user decision of 2026-10-08).
  The `CLIPS` binding and `BUCKET_NAME` must refer to the same bucket. The user confirmed that no other app's data remains there.
  The lifecycle backstop is scoped to `clips/`: expire objects after 3 days and abort incomplete multipart uploads after 1 day.
  Configure it through the dashboard before deployment; the user's Wrangler administration request returned code 10042,
  while the S3 probe succeeded. The cause of that administration error is unverified.
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
| `POST /v1/presence` `{game, active_crew, seated_since_ms, seq, online_since_ms}` | any device | Heartbeat and complete snapshots of the caller's crews; see presence contract below. |
| `GET /v1/crews/:crew/presence` | member | Fresh, available members of this crew; see presence contract below. |
| `POST /v1/clips` `{clip_id, crew_id, ttl_s}` | member | Register a clip (`ttl_s` ≤ 259200 = 72 h, default 72 h). Idempotent for the same owner. |
| `POST /v1/clips/:clip/upload-urls` `{pov, quality, indices[], sizes[], manifest}` | member, and `pov` == caller | Presigned **PUT** URLs for the chunk keys (+ manifest key if requested). At most 64 indices, each size ≤ 64 MiB. Per-clip total ≤ 1.5 GiB. Per device per UTC day ≤ 10 GiB (usage table). Content-Type `application/octet-stream` is signed. |
| `POST /v1/clips/:clip/download-urls` `{pov, quality, indices[], manifest}` | member | Presigned **GET** URLs |
| `DELETE /v1/clips/:clip` | owner or any member | Delete all objects under `clip_prefix` (R2 list + delete in batches) and mark the clip deleted |
| `GET /v1/health` | public | `{ok:true}` |

`scheduled()` runs hourly:
- delete clips whose `expires_at < now` (R2 objects first, then the row);
- purge `seen_signatures` older than 15 min;
- delete expired invites;
- delete presence rows older than the presence TTL;
- log counts.

### Presence contract (task 10)

- Migration `0003_presence.sql` adds one `device_presence` row per authenticated device, not per crew.
  A heartbeat updates the device's availability consistently across all its crews.
- All five fields are required: `game` is `null` (app open, no game) or a nonempty UTF-8 string of at most
  64 bytes without control/surrogate/bidi-control characters (a games-db id, not a display name);
  `active_crew` is `null` (no crew chosen) or a lowercase canonical UUID of a crew the caller belongs to;
  `seq` and `online_since_ms` are nonnegative JavaScript safe integers (0..9007199254740991).
  `seated_since_ms` is `null` when not seated or a nonnegative safe integer when holding a seat.
  Device identity always comes from the signature, never a body field. Unknown fields are ignored as on other routes.
- `seq` increases within an app run. Fresh rows accept only a lexicographically newer `(online_since_ms, seq)` pair:
  a greater `online_since_ms` accepts a restarted app immediately, even when its sequence resets.
  An old run or an equal/lower sequence in the same run returns
  `409 stale_presence`, without updating any field or extending freshness. Once it expires, any sequence is accepted,
  allowing app restarts. The comparison and update are one conditional SQL statement.
- Freshness uses **Worker receipt time**, never a client timestamp: `now - seen_at_ms <= 90000`.
  `online_since_ms` and `seated_since_ms` use the **DuoClip global clock (AppClock, milliseconds)**, so different PCs can compare them
  for queue and seat ordering. They do not determine TTL or membership authorization.
  Clients heartbeat every 30 s and immediately on commitment, game or seat changes; a new run starts a new online-since/sequence.
  A restarted clock that goes backwards may be locked out for at most the 90-second TTL.
- POST returns `200 {ok:true, seen_at_ms, expires_at, heartbeat_interval_ms:30000, crews:[{crew_id,members:[]}]}`,
  where `expires_at = seen_at_ms + 90000`. Each crew the caller currently belongs to has one complete snapshot,
  sorted by crew id; members use the GET shape and ordering below. An empty crew is included with `members: []`.
  A device without crews receives `crews: []`. This avoids a separate authenticated GET per heartbeat.
  All snapshots are fetched with one SQL read; the number of crews does not add a query per crew.
  the row is still fresh at that exact millisecond. Invalid input returns 400; an active crew without membership returns 403.
- GET returns an array sorted by device id:
  `[{device_id, display_name, game, active_crew, seated_since_ms, seq, online_since_ms, seen_at_ms, expires_at}]`.
  It includes the caller if fresh and available, idle members (`game: null`), and members playing any game.
  The session client selects the matching game and caps the session at 8; the Worker must not truncate the crew to 8.
- A member is available when `active_crew` is `null` or equals the queried crew. Members active in a different crew
  are omitted, so its identity is never disclosed across groups. No query can expose non-members or expired rows.
  The same isolation applies separately to every POST snapshot; the caller cannot choose extra crew ids to query in the body.
  The client treats POST and GET results as full snapshots (missing members leave); it must not refresh cached presence from a
  snapshot that repeats a run/sequence pair, or use Worker timestamps as its local clock.
  Configure `SessionConfig.presence_ttl_ms` to 90000 when using this transport, and translate freshness using local receipt time.
- GET remains available for explicit reads; clients must not poll it in parallel with the regular POST snapshots.
- Hourly sweep purges rows with `seen_at_ms < now - 90000` and reports `presence_rows_purged`.
  Expired devices disappear from GET immediately, even before cron runs.
- Required tests: auth and membership, multi-crew isolation and busy peers, idle/different games and 8+ members,
  malformed/oversized input, receipt-time TTL including the boundary, heartbeat renewal, stale/concurrent sequences,
  restart after expiry, purge boundaries and the real SQL migration/query/update path. No network or capture is needed.
  Also cover nullable/safe-integer seats, restarted-run ordering, all-crew POST snapshots including empty crews,
  isolation across those snapshots, and eight clients obtaining peers with one authenticated request per interval.

#### D1 write budget

Migration 0003 deliberately omits the receipt-time index: small private groups can scan this one-row-per-device table during cron.
This avoids one index write per heartbeat. Replay protection remains unchanged: each signature has a table row plus two indexes,
then expires and is deleted by cron. Conservatively budget 3 insertion + 3 deletion writes for replay protection and up to 2 for
the presence row/primary index, or **8 written rows per heartbeat including deferred cleanup**.
At 30 s, 8 devices produce 960 heartbeats/hour: at most **7680 estimated written rows/hour**, or **92160 in 12 hours**.
This is an estimate, not a measured D1 billing guarantee. Other API requests, state-change bursts, setup writes and presence cleanup
share the daily quota; more groups/devices increase usage. Monitor actual D1 metrics and retain headroom.
The free-plan allowance is currently 100000 written rows/day; continuously polling for 24 hours exceeds it even with this policy.
Index accounting and the allowance are documented in [Cloudflare D1 pricing](https://developers.cloudflare.com/d1/platform/pricing/).
Using separate GET polling would add replay writes and must be included in the budget if a client chooses to do it.

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

### Opt-in real R2 verification (task 13)

- `npm run test:r2 -- [--env-file path/to/.dev.vars]` compiles and runs the existing Worker presigner against real R2.
  The environment file defaults to the current Worker's `.dev.vars`. This command is separate from offline `npm test`.
- One fresh random `clips/{crew}/{clip}/{pov}/proxy/000000.bin` key is checked for `404 NoSuchKey`, uploaded with synthetic
  AES-GCM ciphertext and the exact signed length/type, then downloaded and compared byte-for-byte.
- Reusing the PUT URL with a different body length must yield `403 SignatureDoesNotMatch`. Cleanup signs a DELETE of
  this one key only, attempts it even after a lost PUT response, and verifies `404 NoSuchKey` afterwards.
- Each HTTP request has a 15-second deadline and rejects redirects. No existing clips, bucket settings, D1 data or
  deployed Workers are modified. Credentials, request headers, signed URLs and raw error bodies are never logged.
- The JSON result reports steps/statuses, success and cleanup. A failed cleanup includes only the random object key
  for manual removal. Exit code is nonzero for configuration, network, protocol, data-integrity or cleanup failures.
- Real R2 verification requires a successful opt-in run. D1's id in the local file is configuration metadata;
  it does not supply Cloudflare administration authentication or prove that remote migrations have been applied.

## Two-friend diagnostic (task 19)

`npm run test:crew -- [--url ORIGIN]` runs `tools/crew-smoke.ts` against a running Worker (default
`http://127.0.0.1:8787`). It checks health, registers two fresh devices with in-memory non-extractable Ed25519 private keys,
creates a crew, creates/redeems an invite, verifies membership, and verifies complete two-peer presence snapshots through
signed heartbeats. It then announces both devices idle. All signed requests use canonical path/body hashing, increasing
timestamps to avoid duplicate signatures, a 15-second request timeout, and no redirect following.

The JSON result reports only step names, HTTP statuses, success, idle cleanup and a sanitized failure code; keys, signatures,
invites and response bodies are never printed. `ok: false` gives CLI exit code 1. HTTP is allowed only for loopback; other
origins must use HTTPS. No R2 credentials, media capture or deployment is involved. Presence time/game data is synthetic;
this probe is not the production AppClock or persistent device-identity implementation.

Two devices, one crew and the invite remain in the selected D1 after execution. Cleanup means idle presence, not deletion;
on early failure presence expires normally after 90 s. Repeated probes consume registration quota. Tests exercise the command's
flow over real loopback HTTP and the real router/SQLite migrations, plus failed registration and an incomplete membership result.

## Implementation notes (accepted deviations, Phase A review) — the Rust client MUST follow these

- **Upload URLs sign `Content-Length` and `Content-Type: application/octet-stream`.** The client must PUT exactly the returned
  `content_length`, without chunked transfer encoding. `upload-urls` requires `manifest_size` whenever `manifest: true`, and manifest
  bytes count against quotas.
- Response shape: `{expires_in, expires_at, headers, chunks: [{index, key, url, content_length}], manifest: {key, url} | null}`.
- Extra tables, in `migrations/0002_abuse_limits.sql`:
  - `clips.bytes` and `clip_chunks`, for an atomic per-clip cap; retrying with the same size is not charged twice;
  - abuse counters and circuit breakers: new devices per day, global bytes per day, invite-join attempts, a minimum chunk size against floods of tiny objects.
- Sweep: R2 objects are deleted at `expires_at`, and the row is dropped one run later (two-pass), so late uploads through still-valid URLs are removed too.
  The sweep stays within the subrequest limits.
- Status codes:
  - 201 for created, 200 for repeated;
  - 404 `invalid_invite`, 409 `clip_exists`, 410 `clip_gone`/`clip_expired`;
  - 413 `chunk_too_large`/`clip_quota_exceeded`, 429 `daily_quota_exceeded` with `Retry-After`;
  - 401 with a `reason`.
- The Ed25519 verification is strict (non-malleable). Invite codes are normalized ASCII-only. The client signs the WHATWG-serialized `pathname + search`.
- Tests need Node ≥ 22.13 (`node:sqlite`, WebCrypto Ed25519). Phase A validated R2 presigning against an independent SigV4 implementation and
  `wrangler dev`. Task 13 additionally verified PUT/GET, signed-length rejection and cleanup against real R2; see below.
- Recommended after the friends register: Cloudflare rate-limiting rules on `POST /v1/devices` and `POST /v1/crews/join`, and possibly closing
  registration. Any crew member can delete any clip (per SPEC), and there is no member removal yet.

## Implementation notes (task 13, approved by Claude 2026-10-08)

- The opt-in probe uses existing dependencies and does not change production routes, quotas or presigning rules.
  Nine offline tests cover the probe's control flow, exact headers, cleanup, redaction and negative length check.
- The user ran the real R2 probe successfully on 2026-10-08 and supplied the six expected statuses with cleanup completed.
  The GPT sandbox's earlier attempts were blocked with `EACCES`. See `R2-VALIDACAO.md` for the user-supplied evidence.
  Non-secret R2/D1 identifiers are configured. The user subsequently queried the configured D1 successfully with Wrangler;
  it had zero tables. Worker deployment and remote D1 migrations remain pending.

## Implementation notes (task 10, approved by Claude 2026-10-08)

- The presence contract above extends the Phase A API; no existing route changes its response or authorization.
  Presence uses the existing injected `Db`, clock and Ed25519 authentication, with no new dependencies.
- Mapping to the session crate: `device_id` becomes `Presence.device`; the other announcement fields retain their names.
  The JSON safe-integer limit is narrower than Rust's `u64`. The client must stay in that range; sequence zero is accepted
  for a newer app run or after the old heartbeat expires. Presence transport adapters in the native app remain outside this Worker task.
- Busy members are omitted across crews to preserve isolation. An adapter must apply complete POST/GET snapshots, including
  departures; simply replaying the returned rows through `on_presence` will leave missing peers cached until the local TTL.
- Verification uses real SQLite SQL and synthetic data; this task does not deploy the Worker or apply migrations to remote D1.
- Cross-review follow-up adds the session's nullable seat timestamp, global-clock semantics and atomic app-run ordering.
  The 30-second heartbeat/90-second TTL, POST snapshots and omission of a receipt-time index reduce D1 write pressure.
  All own-crew snapshots use one SQL query, preserving empty crews and avoiding one query per crew.
  Author responses and verification are recorded in `PRESENCA-AJUSTES.md`; approved by Claude on 2026-10-08.
