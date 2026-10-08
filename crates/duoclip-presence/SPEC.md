# duoclip-presence — SPEC (Worker presence ⇄ SessionManager adapter)

Context: task 17 in `docs/TAREFAS.md`; Worker presence contract in `worker/SPEC.md` ("Presence contract (task 10)" and "D1 write budget");
session rules in `crates/duoclip-session/SPEC.md`; decision 4.7 in `docs/MEMORIA-DO-PROJETO.md`.

This crate is **pure, portable logic** (`#![forbid(unsafe_code)]`, no I/O, no threads, no clock reads, never panics on data from the network).
It decides **when** to send a presence heartbeat, **what** to send, and **how** to apply the Worker's response to a
`duoclip_session::SessionManager`. HTTP, Ed25519 request signing and timers belong to the Phase C network layer; the caller feeds this crate
its local monotonic time (`now_ms`) and the DuoClip global clock in ms (`app_clock_ms`, AppClock), and performs the requests.

## Wire types (serde, must match the Worker exactly)

```rust
#[derive(Serialize)] pub struct HeartbeatRequest { pub game: Option<String>, pub active_crew: Option<String /* lowercase uuid */>,
    pub seated_since_ms: Option<u64>, pub seq: u64, pub online_since_ms: u64 }          // POST /v1/presence body
#[derive(Deserialize)] pub struct HeartbeatResponse { pub ok: bool, pub seen_at_ms: u64, pub expires_at: u64,
    pub heartbeat_interval_ms: u64, pub crews: Vec<CrewSnapshot> }
#[derive(Deserialize)] pub struct CrewSnapshot { pub crew_id: String, pub members: Vec<MemberPresence> }
#[derive(Deserialize)] pub struct MemberPresence { pub device_id: String, pub display_name: String, pub game: Option<String>,
    pub active_crew: Option<String>, pub seated_since_ms: Option<u64>, pub seq: u64, pub online_since_ms: u64,
    pub seen_at_ms: u64, pub expires_at: u64 }
```
Unknown JSON fields are ignored. Integers sent must be ≤ 2^53−1 (JavaScript safe integers); the client clamps/refuses otherwise.
Malformed entries (bad uuid, oversized game id) are skipped individually, never failing the whole snapshot; a malformed top-level body is an error.

## Change to `duoclip-session` (same task)

`SessionManager::apply_snapshot(&mut self, members: Vec<Presence>, now_ms: u64) -> SnapshotOutcome`:
- the Worker response lists, across ALL of the caller's crews, every fresh and available member; it is **authoritative for departures**:
  every tracked device that is **absent from the union** of the snapshot (and is not me) is forgotten immediately (→ `Left` on the next tick
  if it was a participant), instead of waiting for the local TTL;
- present devices go through the existing `on_presence` rules (ordering by `(online_since_ms, seq)`; a repeated pair does **not** refresh freshness —
  the Worker contract forbids it); my own device is ignored;
- returns counts (accepted, stale, forgotten) for diagnostics.
The local TTL (`presence_ttl_ms`) stays as the safety net when responses stop arriving. Keep every existing invariant and test of the crate,
including `tests/agreement.rs` (add a scenario where managers are fed only Worker-style snapshots).

## Client state machine

```rust
pub struct PresenceConfig { pub interval_ms: u64 /* 30_000, overridden by heartbeat_interval_ms from the Worker within 10_000..=300_000 */,
                            pub presence_ttl_ms: u64 /* 90_000 */, pub max_backoff_ms: u64 /* 30_000 */ }
pub struct PersistedState { pub last_online_since_ms: u64 }   // the app stores this between runs

pub struct PresenceClient { /* owns the SessionManager */ }
impl PresenceClient {
    /// Builds the SessionManager with `presence_ttl_ms` = config.presence_ttl_ms (90 s). `online_since_ms` of this run =
    /// max(app_clock_ms_at_start, persisted.last_online_since_ms + 1), so a restart never announces a smaller run id
    /// (the Worker would answer 409 for up to 90 s otherwise).
    pub fn new(me: DeviceId, session: SessionConfig, cfg: PresenceConfig, app_clock_ms_at_start: u64, persisted: Option<PersistedState>) -> Result<Self, PresenceError>;
    pub fn persisted_state(&self) -> PersistedState;
    pub fn session(&self) -> &SessionManager;  pub fn session_mut(&mut self) -> &mut SessionManager; // membership, local game, choose_crew
    /// Returns the request to send now, or None. Due when:
    ///  - "active" (local game is Some OR the session is not Idle): every interval, and IMMEDIATELY after any change of game,
    ///    active crew or seat compared with the last request sent (commitment/seat changes must propagate fast);
    ///  - "idle" (no game and session Idle): send ONE announcement with game = None after becoming idle, then nothing
    ///    (saves D1 writes: idle devices are never session candidates, and the Worker expires them after the TTL).
    /// At most one request in flight; `seq` increments on every request built.
    pub fn poll(&mut self, now_ms: u64) -> Option<HeartbeatRequest>;
    /// Applies a 200 response: converts every member of every crew snapshot into `Presence`, calls `apply_snapshot`, then `tick`.
    /// Uses the Worker's `heartbeat_interval_ms` (clamped). Returns the session events.
    pub fn on_success(&mut self, resp: HeartbeatResponse, now_ms: u64) -> Vec<SessionEvent>;
    /// 409 stale_presence: keep the run id, retry at the next interval (do not loop). Network/5xx: exponential backoff from 1 s up to
    /// max_backoff_ms. 4xx other than 409: report and back off to the interval. The local TTL handles peers meanwhile.
    pub fn on_failure(&mut self, failure: HeartbeatFailure, now_ms: u64) -> Vec<SessionEvent>;
    /// Periodic housekeeping (call ~1 Hz): forwards to SessionManager::tick.
    pub fn tick(&mut self, now_ms: u64) -> Vec<SessionEvent>;
}
pub enum HeartbeatFailure { Stale409, Http(u16), Network, InvalidResponse /* 200 with an unparsable body */ }
```

Additional API (see Implementation notes):

```rust
impl HeartbeatRequest { pub fn to_json(&self) -> Result<String, PresenceError>; }        // refuses what the Worker would reject
impl HeartbeatResponse { pub fn from_json(body: &[u8]) -> Result<Self, PresenceError>;    // top-level errors, bad entries dropped
                         pub fn presences(&self) -> Vec<Presence>; }                       // valid members of valid crews
impl MemberPresence { pub fn to_presence(&self) -> Option<Presence>; }
impl PresenceConfig { pub fn validate(&self) -> Result<(), PresenceError>; pub fn max_interval_ms(&self) -> u64;
                      pub fn clamp_interval(&self, ms: u64) -> u64; pub fn min_switch_grace_ms(&self) -> u64; }
impl PresenceClient { pub fn choose_crew(&mut self, crew: CrewId, now_ms: u64) -> Result<(), SessionError>;
                      pub fn session_time(&mut self, now_ms: u64) -> u64; pub fn online_since_ms(&self) -> u64;
                      pub fn interval_ms(&self) -> u64; pub fn in_flight(&self) -> bool;
                      pub fn last_snapshot(&self) -> Option<SnapshotOutcome>; }
pub enum PresenceError { Session(SessionError), InvalidConfig(&'static str), ClockOutOfRange(u64),
                         MalformedResponse(String), NotOk, UnsafeInteger(&'static str), InvalidRequest(&'static str) }
pub fn parse_canonical_uuid(s: &str) -> Option<Uuid>;  pub fn is_valid_game_id(g: &str) -> bool;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991; // + MIN/MAX_INTERVAL_MS, INITIAL_BACKOFF_MS, MIN_CHANGE_GAP_MS, GRACE_MARGIN_MS
```

## Tests (required)

- Wire format: request JSON exactly matches the Worker (field names, nulls, lowercase uuids); response parsing ignores unknown fields; bad
  entries are skipped; non-safe integers refused.
- `online_since_ms` monotonic across runs (persisted value larger than the clock → last + 1).
- Poll policy: interval cadence, immediate announce on game/crew/seat change, single idle announcement then silence, one request in flight,
  seq strictly increasing, backoff on failures, 409 does not loop.
- `apply_snapshot`: departures forgotten immediately (participant → Left), repeated pair does not refresh freshness, my own device ignored,
  devices present in another crew's snapshot are kept, empty snapshot forgets everyone, local TTL still expires peers when responses stop.
- End-to-end simulation (no network): a fake Worker in the test (in-memory model of the Worker contract: freshness by receipt time,
  (online_since, seq) ordering, availability filter per crew, full snapshots) with 2 crews and 3–9 devices running `PresenceClient`s;
  assert sessions converge to the same participant sets as `tests/agreement.rs` expects, isolation between crews holds, and the number of
  requests per device per hour while active ≈ 3600/30 (+ change announcements) and ≈ 0 while idle.

Required checks: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`,
`cargo check --workspace --all-targets --target x86_64-pc-windows-gnu`.

## Implementation notes

Decisions taken where this spec was silent or would have been unsafe as written (task 17, 2026-10-08).

- **Clocks / session time.** `now_ms` is the caller's local monotonic clock; the AppClock is given only at start. The
  `SessionManager` runs on *session time* = run id + (`now_ms` − first `now_ms` seen). Reasons: (1) the session must rank
  itself with exactly the `online_since_ms` it announces (otherwise queue order differs between devices), so `new` ticks the
  manager once at the run id, latching it; (2) the announced `seated_since_ms` must be on the AppClock scale, or a device with a
  small local clock (short uptime) would outrank older seats after a seat race. Freshness and grace only use differences of
  session time, i.e. local elapsed time. Calls with a time made through `session_mut()` must use `session_time(now_ms)`;
  `choose_crew` and `tick` wrappers do it. Call `poll`/`tick` right after `new` (the first `now_ms` sets the base).
- **Switch grace raised to 65 s.** Peers are seen up to one interval (30 s) late, so with the session default of 10 s a friend
  who closes and reopens the game within seconds ends my session whenever my heartbeat falls inside that window (verified:
  `a_friend_restarting_the_game_does_not_end_the_session` fails without the floor). The anti-livelock rule also needs the
  lower id's `2 × grace` to cover the higher id's move plus one interval. `new` therefore uses
  `switch_grace_ms = max(given, 2 × max_interval_ms + 5 s)`. Cost: a session whose friends all left lasts 65 s alone before
  another crew can be picked.
- **Interval bounds.** The Worker's `heartbeat_interval_ms` is clamped to `10_000..=min(300_000, presence_ttl_ms / 3)`, i.e. at
  most 30 s with the 90 s TTL: a longer interval would let peers expire between heartbeats (the spec's `..=300_000` alone would
  allow that). `PresenceConfig::validate` requires `interval_ms` in `10_000..=300_000`, `presence_ttl_ms >= 30_000` and
  `max_backoff_ms >= 1_000`.
- **What "the last request sent" means.** The client compares the current game/crew/seat with what the Worker holds: set when
  a request is built, cleared on any failure (so the state is re-announced as soon as the retry is allowed). A failure's
  retry time blocks *every* request, change announcements included (no bypass of the backoff, so a 409 never loops).
- **Change-announcement floor.** At most one change announcement per `MIN_CHANGE_GAP_MS` (1 s) after the previous request:
  a guard against request storms (D1 writes) if some state ever oscillated; in tests the 1 s is invisible.
- **Idle announcement at start.** A client without game sends its single idle announcement at start too: it replaces at once a
  still-fresh row of the previous run (which may claim a seat) instead of leaving it for up to 90 s.
- **Failures.** `Http(409)` is the same as `Stale409`. `Network` and `Http(500..=599)` back off 1, 2, 4, 8, 16 s, then
  `max_backoff_ms`; any other status (4xx, 429, unexpected codes) and `InvalidResponse` wait one interval. `on_success` with
  `ok != true` is handled as `InvalidResponse`. A success resets the backoff and schedules the next heartbeat one interval after
  the response.
- **Malformed data.** Entries are dropped one by one: a member with a non-canonical uuid, a game id the Worker would refuse, or
  an integer above 2^53−1 (including `seen_at_ms`/`expires_at`), and a crew entry with a bad `crew_id` or `members` (with its
  members). A dropped entry counts as **absent**, so a tracked device listed only there is forgotten: clips never go to a device
  whose state is unknown. A top-level problem (not an object, `ok` missing/false, a top-level integer unsafe, `crews` not an
  array) is an error. Nesting deeper than serde_json's recursion limit inside `crews` fails the whole body (no stack overflow);
  inside unknown fields it is skipped.
- **Outgoing values.** `seq` starts at 1; a `seq` above 2^53−1 is never built (`poll` returns `None` forever, unreachable in
  practice). `seated_since_ms` is clamped to 2^53−1. A local game id the Worker would reject (blank, control/bidi characters,
  > 64 bytes) is announced as `game: null`. `to_json` refuses unsafe integers and invalid fields.
- **Run id.** A persisted `last_online_since_ms >= 2^53−1` is treated as corrupt and ignored (the run then uses the clock and
  may be locked out by a fresh row of the previous run for at most the 90 s TTL); `ClockOutOfRange` only when the AppClock
  itself is above 2^53−1. `PersistedState` derives serde for the app to store it.
- **Worker timestamps.** `seen_at_ms` / `expires_at` are validated but never used (the contract forbids using them as the
  local clock); freshness is the local receipt time inside `apply_snapshot`.
- **Measured rates** (`tests/simulation.rs`, fake Worker in JSON, 9 devices in 2 crews): 119–121 requests per active device in a
  stable hour (3600 / 30 = 120); with 6 game switches in an hour, ≤ 145; going idle ≤ 6 (grace heartbeats + one idle
  announcement); then 0 per idle hour. A restart with persisted state never gets 409; without it and with the clock 10 min behind,
  2–3 attempts get 409 (one per interval) until the old row expires.
- **Dependencies.** `uuid` (workspace) to parse canonical uuids without the private helper of `duoclip-proto`.
