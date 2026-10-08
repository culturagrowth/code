export const meta = {
  name: 'duoclip-phase-a-implement',
  description: 'Fase A: implementa proto+crypto, worker (agentes rápidos) e clock+buffer (agentes avançados), cada um com revisão adversarial e correção',
  phases: [
    { title: 'Implementar', detail: 'sonnet: proto+crypto, worker · opus: clock, buffer' },
    { title: 'Revisar', detail: 'revisão adversarial + correção (mesmo nível de modelo)' },
  ],
}

const ROOT = '/home/user/code'
const COMMON = `You are implementing part of DuoClip, a Windows app for friends that records each player's game and, on a hotkey, saves every player's POV synchronized by a global clock (N s before and M s after the press), exchanging encrypted clips through a Cloudflare R2 bucket. Repository root: ${ROOT}. Architecture/research doc (Portuguese): ${ROOT}/docs/pesquisa-app-clipes-sincronizados.md — read the sections your SPEC references.

RULES:
- Work ONLY inside the directory you are assigned. Do NOT edit other crates, the root Cargo.toml, README.md or docs/. Do NOT run git commands (the coordinator commits).
- Dependencies are pre-resolved in the root Cargo.lock. Prefer the deps already declared in your Cargo.toml / [workspace.dependencies]. Only if truly necessary, add a small well-known crate to YOUR crate's Cargo.toml and mention it in your result.
- Rust: edition 2021, #![forbid(unsafe_code)], no panics on untrusted input, idiomatic error handling with thiserror, doc comments on public items (English). Keep the public API exactly as the SPEC says (you may add items).
- Before finishing, all of these must pass for your crate: \`cargo fmt -p <crate>\`, \`cargo clippy -p <crate> --all-targets -- -D warnings\`, \`cargo test -p <crate>\`, and \`cargo check -p <crate> --target x86_64-pc-windows-gnu\` (the target is installed; check only, no linking). Other agents may be compiling concurrently; if cargo waits on a lock, just wait.
- Tests must be fast (whole crate < 10 s) and deterministic (seeded RNG).
- Your final output is consumed by a program: return the structured result only.`

const IMPL_SCHEMA = {
  type: 'object',
  properties: {
    component: { type: 'string' },
    status: { type: 'string', enum: ['ok', 'partial', 'failed'] },
    summary: { type: 'string' },
    tests_passed: { type: 'integer' },
    checks: { type: 'string', description: 'Exact commands run and their final result (fmt/clippy/test/windows check or npm typecheck/test).' },
    deviations_from_spec: { type: 'array', items: { type: 'string' } },
    known_limitations: { type: 'array', items: { type: 'string' } },
    files: { type: 'array', items: { type: 'string' } },
  },
  required: ['component', 'status', 'summary', 'tests_passed', 'checks', 'deviations_from_spec', 'known_limitations', 'files'],
}

const REVIEW_SCHEMA = {
  type: 'object',
  properties: {
    component: { type: 'string' },
    issues: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          severity: { type: 'string', enum: ['critical', 'major', 'minor'] },
          description: { type: 'string' },
          fixed: { type: 'boolean' },
          note: { type: 'string' },
        },
        required: ['severity', 'description', 'fixed', 'note'],
      },
    },
    tests_passed: { type: 'integer' },
    checks: { type: 'string' },
    remaining_risks: { type: 'array', items: { type: 'string' } },
    summary: { type: 'string' },
  },
  required: ['component', 'issues', 'tests_passed', 'checks', 'remaining_risks', 'summary'],
}

const TASKS = [
  {
    key: 'proto+crypto', model: 'sonnet',
    impl: `${COMMON}

YOUR TASK (two crates, in this order because crypto depends on proto):
1) Implement ${ROOT}/crates/duoclip-proto exactly per ${ROOT}/crates/duoclip-proto/SPEC.md.
2) Then implement ${ROOT}/crates/duoclip-crypto exactly per ${ROOT}/crates/duoclip-crypto/SPEC.md (it depends on duoclip-proto types and key helpers).
You may only edit files under crates/duoclip-proto and crates/duoclip-crypto. Write thorough tests as the SPECs require.`,
    review: `${COMMON}

YOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-proto and ${ROOT}/crates/duoclip-crypto against their SPEC.md files. Read the SPECs and the code carefully. Hunt for: spec deviations, validation gaps (anything untrusted that could panic, overflow, or slip through: object keys with "..", "//", uppercase uuids, huge vectors, base64 edge cases), crypto mistakes (nonce reuse, AAD not bound to all fields, key material in Debug/logs, missing zeroize, non-constant-time comparisons where it matters, manifest well-formedness checks missing, truncation not detectable), chunker off-by-one / is_last rule, serde tagging that doesn't match the SPEC JSON shape, missing negative tests. FIX every real issue directly in those two crates and add regression tests. Do not rewrite working code for style. Re-run fmt/clippy/test/windows check.`,
  },
  {
    key: 'worker', model: 'sonnet',
    impl: `${COMMON}

YOUR TASK: implement the Cloudflare Worker in ${ROOT}/worker exactly per ${ROOT}/worker/SPEC.md (TypeScript, aws4fetch presigning, D1, R2, cron). You may only edit files under ${ROOT}/worker. Use npm (registry access works): install exact versions with "npm install --save-exact" for aws4fetch and devDependencies typescript, vitest, wrangler, @cloudflare/workers-types. Keep node_modules out of git (root .gitignore already ignores it). Golden key strings in your tests must match crates/duoclip-proto's object_key/manifest_key format described in ${ROOT}/crates/duoclip-proto/SPEC.md: "clips/{crew}/{clip}/{pov}/{proxy|full}/{index:06}.bin" and ".../manifest.bin". Instead of cargo checks, your required checks are: \`npm run typecheck\` and \`npm test\` both pass. Write the README.md in Portuguese (setup: create R2 bucket, R2 API token for S3 access, D1 database + migrations, secrets, deploy, lifecycle backstop rule).`,
    review: `${COMMON}

YOUR TASK: ADVERSARIAL SECURITY + CORRECTNESS REVIEW and FIX of the Cloudflare Worker in ${ROOT}/worker against ${ROOT}/worker/SPEC.md. Hunt for: auth bypasses (routes missing signature checks, canonical string ambiguity, timestamp skew/replay holes, signature replay table race), membership checks missing (IDOR on crew/clip ids), pov != caller uploads allowed, path traversal or key injection in object keys, unvalidated bodies / missing size limits, quota bypass (sizes not enforced, negative numbers, integer overflow), invite code brute force or reuse, presigned URL scope too broad (wrong method/expiry/content-type), scheduled cleanup bugs (deleting rows before objects, unbounded list loops), SQL injection (must use bound parameters), error messages leaking secrets. FIX every real issue directly under ${ROOT}/worker and add regression tests. Required checks: \`npm run typecheck\` and \`npm test\` pass.`,
  },
  {
    key: 'clock', model: 'opus',
    impl: `${COMMON}

YOUR TASK: implement ${ROOT}/crates/duoclip-clock exactly per ${ROOT}/crates/duoclip-clock/SPEC.md — the "Relógio Global DuoClip" (read docs section 8 fully). This is a sophisticated numerical component: SNTP packet handling with era pivot, per-source sample filtering + weighted least squares offset/rate estimation with honest error bounds, Marzullo-style source combination with falseticker rejection, a monotonic slew-only AppClock with controlled steps/epochs and holdover, two-sided retroactive remapping, P2P cross-check, and a poll scheduler with KoD handling. Build the simulation harness described in the SPEC and make the statistical tests meaningful (seeded RNG, assert on error vs bound, rate convergence, monotonicity under thousands of random updates). Think carefully about numeric precision (i128/f64), overflow, and edge cases (empty sets, single sample, zero span, huge delays). You may only edit files under crates/duoclip-clock.`,
    review: `${COMMON}

YOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-clock against its SPEC.md and docs section 8. Verify mathematically: offset/delay formulas and sign conventions (server vs peer), WLS weights and rate units (ppb), bound formula, Marzullo correctness (interval endpoints ordering, ties), AppClock monotonicity proof (look for any update path that could make utc_at decrease or jump without an epoch increment), slew clamping, step rules, holdover growth, local_at being the inverse of utc_at, era pivot around 2036 and negative/huge values, NTP parse rejections (cookie check, LI=3, KoD), scheduler min interval and backoff caps. Write additional adversarial tests (e.g., update storms, alternating ±large corrections, samples with identical timestamps, delay=0, all samples filtered out, NaN/inf avoidance) and FIX every real issue found. Confirm the statistical tests are not tautological (they must fail if the estimator were broken — e.g., temporarily reason about what a wrong sign would do). Re-run fmt/clippy/test/windows check.`,
  },
  {
    key: 'buffer', model: 'opus',
    impl: `${COMMON}

YOUR TASK: implement ${ROOT}/crates/duoclip-buffer exactly per ${ROOT}/crates/duoclip-buffer/SPEC.md — the replay ring buffer of encoded packets and the "fixar e coletar" (pin-and-collect) post-roll clip state machine (read docs section 6 fully, including the edge-case table). Key correctness properties: pinned packets survive ring eviction via Arc; GOP-wise eviction; per-track end detection with encoder-latency tolerance; finalize on all-tracks-reached-end / timeout / source end; fragment readiness rule (next keyframe seen AND every audio track past it), every packet in exactly one fragment; coverage/gap reporting; idempotent requests; extension capped at max_len; memory budget. Build the synthetic stream generator and the tests the SPEC lists, including a randomized interleaving test. You may only edit files under crates/duoclip-buffer.`,
    review: `${COMMON}

YOUR TASK: ADVERSARIAL REVIEW + FIX of ${ROOT}/crates/duoclip-buffer against its SPEC.md and docs section 6 (edge-case table 6.6). Hunt for: pre-roll loss (any path where packets needed by an active clip are not captured), duplicate or missing packets across fragments, audio packets assigned to the wrong fragment, fragments emitted before audio passed the next keyframe, finalization never happening (a track that never sends packets must still finalize via timeout), late-request handling, extend after Done, window arithmetic overflow, eviction removing the in-progress GOP, out-of-order handling, memory accounting double counting or underflow, O(n^2) hot paths on push (packets arrive ~200/s for hours — push must be amortized O(1)/O(log n)). Add adversarial tests (long runs, many clips, randomized jitter, tracks starting late, packets with equal timestamps) and FIX every real issue. Re-run fmt/clippy/test/windows check.`,
  },
]

const results = await pipeline(
  TASKS,
  t => agent(t.impl, { label: `implementar:${t.key}`, phase: 'Implementar', model: t.model, schema: IMPL_SCHEMA }),
  (implResult, t) => agent(
    `${t.review}\n\nThe implementer reported:\n${JSON.stringify(implResult)}`,
    { label: `revisar:${t.key}`, phase: 'Revisar', model: t.model, schema: REVIEW_SCHEMA }
  ).then(review => ({ task: t.key, model: t.model, impl: implResult, review })),
)

return results.filter(Boolean)
