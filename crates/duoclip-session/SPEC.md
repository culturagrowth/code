# duoclip-session — SPEC (automatic per-group play sessions)

Context: docs/MEMORIA-DO-PROJETO.md section 4.7 (user decision of 2026-10-08). A person can belong to several **groups** (crews).
On a given day they play with one of them; groups never see each other's clips. Sometimes 3 friends play, sometimes 6.
The **session** is the set of devices that take part in a clip right now. It forms **automatically**:

> members of the same crew, with the app open, playing the same game — at most 8 devices (self included).

This crate is **pure, portable decision logic** (`#![forbid(unsafe_code)]`, no I/O, no threads, no clock reads): the caller feeds it
membership, presence announcements and its own monotonic time in milliseconds, and it returns the session state and events.
Transport of presence (Worker heartbeat and/or WebRTC) is Phase C and **not** part of this crate. It must never panic on untrusted input.

**Clock.** `now_ms` should be the DuoClip global clock (AppClock, UTC-disciplined, monotonic) in ms. Then the `online_since_ms` and
`seated_since_ms` that devices announce are comparable across devices, which makes queue order fair and seats sticky. With a
per-device clock everything below still holds (all devices still agree), but the order among devices becomes arbitrary.

## Concepts

- **Membership**: the crews I belong to and their member devices (from the Worker `GET /v1/crews/:crew/members`).
- **Presence**: what each device announces periodically: its `game` (games-db id of the foreground game, or `None`), the crew it is
  **committed** to (`active_crew`: the crew of the session it is in, seated or queued), `seated_since_ms` (`Some(t)` while it holds a
  seat), `online_since_ms` (identifies the run of the app) and an increasing `seq`. The receiver stamps `seen_at_ms` with its own clock
  on arrival (never trust the sender's clock for freshness).
- A peer is a **candidate for crew X** when: it is a member of X (per MY membership), it is not me, its last presence is fresh
  (`now - seen_at <= presence_ttl_ms`), its `game` equals my game (both `Some`), and its `active_crew` is `None` or `X`
  (someone committed to another crew is busy).
- A candidate is **committed to X** when its `active_crew == Some(X)`. Only committed candidates can be **participants**.
- A crew is **eligible** when it has ≥ 1 candidate and I have a game.

## Selection rules

1. No local game, or no eligible crew → `Idle`.
2. Exactly one eligible crew → I commit to it automatically (`Active` or `Queued`).
3. Several eligible crews → if a remembered choice exists for my current game and that crew is eligible → commit to it; otherwise
   `NeedsChoice { crews }` (sorted by number of candidates desc, then crew id) until `choose_crew` is called. The choice is remembered
   per game id (in memory; the app persists `remembered_choices()` itself). While undecided I announce `active_crew = None`, so no
   crew counts me as a participant.
4. **Stickiness (no flapping):** while committed to X, stay on X as long as X is eligible. If X stops being eligible, keep the session
   for `switch_grace_ms` (default 10 s) before re-running rules 1–3 (a friend's game restart, my own game restart (local game `None`)
   or a lost heartbeat must not end the session). A newly eligible second crew never steals a session (no `NeedsChoice` while in one).
   **Anti-livelock:** if some members of X play my game but are committed to another crew ("rivals") and my device id is lower than
   all of theirs, my grace is `2 × switch_grace_ms`. Two friends in both crews with different remembered choices would otherwise swap
   crews at the same moment forever; with this rule the higher id moves first and both end up on the lower id's crew.
5. Switching my local game to a **different** game, or leaving the crew (membership), ends the session immediately (no grace).
6. **Participants (mutual commitment):** a peer is my participant only if it is a candidate *committed to the same crew* and seated.
   So if A lists B, B has itself declared a session with that crew (and, after one heartbeat, lists A); a device is never a participant
   of two crews; and clips go only to devices that agreed to be in the session.
7. **Size cap (agreed by everyone):** the devices committed to X (me included) are ranked by the same key on every device:
   seated devices first by `seated_since_ms`, then the others by `online_since_ms`, then device id. The first `max_size`
   (default 8, valid 2..=8) are seated, the rest are the queue (`overflow`). Seated devices announce their seat, so they keep it when a
   newcomer arrives (a newcomer queues even if it came online earlier); a freed seat goes to the head of the queue. If I am not
   seated, my state is `Queued` (nobody sends me clips and I accept none). A device only takes a *free* seat while it plays the game.
8. A participant leaves when its presence expires or stops matching (other game, other committed crew, removed from the crew). It is
   reported as `Left`.

## API

```rust
pub const MAX_SESSION_SIZE: usize = 8;
pub const MAX_GAME_ID_BYTES: usize = 64;
pub const MAX_TRACKED_DEVICES: usize = 256;
pub const MAX_REMEMBERED_CHOICES: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig { pub presence_ttl_ms: u64 /* 30_000 */, pub switch_grace_ms: u64 /* 10_000 */, pub max_size: usize /* 8 */ }
impl Default for SessionConfig { .. }
impl SessionConfig { pub fn validate(&self) -> Result<(), SessionError>; } // ttl >= 1000, max_size 2..=8

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    pub device: DeviceId, pub seq: u64, pub game: Option<String>, pub active_crew: Option<CrewId>,
    pub seated_since_ms: Option<u64>, pub online_since_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    NeedsChoice { game: String, crews: Vec<CrewId> },
    Active { crew: CrewId, game: String, participants: Vec<DeviceId> /* self first, then join order */, overflow: Vec<DeviceId> },
    Queued { crew: CrewId, game: String, overflow: Vec<DeviceId> /* the queue, me included */ },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    Started { crew: CrewId, game: String },        // I committed to `crew` (seated or queued)
    Ended { crew: CrewId },
    Joined { crew: CrewId, device: DeviceId },     // a fellow participant (both of us seated)
    Left { crew: CrewId, device: DeviceId },
    ChoiceNeeded { game: String, crews: Vec<CrewId> },
    ChoiceDismissed { game: String },              // the pending choice is no longer asked
    Overflow { crew: CrewId, devices: Vec<DeviceId> }, // the queue changed as a set (may be empty)
}

pub struct SessionManager { .. }
impl SessionManager {
    pub fn new(me: DeviceId, config: SessionConfig) -> Result<Self, SessionError>;
    /// Replaces the membership (crew → members) and re-evaluates at once. A crew I left ends with no grace.
    pub fn set_membership(&mut self, crews: BTreeMap<CrewId, BTreeSet<DeviceId>>);
    /// Sets my game and re-evaluates at once. Another game ends the session; `None` keeps it for the grace.
    pub fn set_local_game(&mut self, game: Option<String>);
    /// Records a peer announcement (takes effect at the next tick). Ignored (returns false): my own device, devices in none of
    /// my crews, game ids longer than 64 bytes, announcements not newer than the stored one while it is fresh.
    /// Keeps at most 256 tracked devices (oldest seen evicted).
    pub fn on_presence(&mut self, p: Presence, now_ms: u64) -> bool;
    /// Applies a complete Worker snapshot (every fresh and available member of ALL my crews, merged). Authoritative for
    /// departures: tracked devices absent from it are forgotten at once (participants become `Left` at the next tick).
    /// Listed devices follow the `on_presence` rules, but a repeated `(online_since_ms, seq)` pair never refreshes
    /// freshness. My own device is ignored. Takes effect at the next tick. Returns counts for diagnostics.
    pub fn apply_snapshot(&mut self, members: Vec<Presence>, now_ms: u64) -> SnapshotOutcome;
    /// User picks a crew in NeedsChoice (or switches explicitly). Error if the crew is not one of mine, there is no local game,
    /// or the crew has no candidate.
    pub fn choose_crew(&mut self, crew: CrewId, now_ms: u64) -> Result<(), SessionError>;
    /// Re-evaluates everything at `now_ms` (call ~1 Hz and after every input). Returns the events since the last tick.
    /// `now_ms` going backwards is treated as `now_ms = last` (never panics).
    pub fn tick(&mut self, now_ms: u64) -> Vec<SessionEvent>;
    pub fn state(&self) -> &SessionState;
    /// What to announce (game, committed crew, seat time, online_since, seq incremented on every call).
    pub fn my_presence(&mut self) -> Presence;
    /// Snapshot for a ClipRequest: the active crew and the OTHER participants. `None` unless Active (seated).
    /// A friend who joins later is not part of an already-sent request (the caller keeps this snapshot per clip).
    pub fn clip_targets(&self) -> Option<(CrewId, Vec<DeviceId>)>;
    /// Gate for an incoming ClipRequest (crew from the envelope): Ok only if Active on `crew` and `from` is a current participant
    /// other than me; otherwise Err(RejectReason::NotInSession).
    pub fn check_request(&self, from: DeviceId, crew: CrewId) -> Result<(), RejectReason>;
    pub fn remembered_choices(&self) -> &BTreeMap<String, CrewId>;
    pub fn set_remembered_choices(&mut self, choices: BTreeMap<String, CrewId>);
    pub fn tracked_devices(&self) -> usize;
}
```

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapshotOutcome { pub accepted: usize, pub stale: usize, pub ignored: usize, pub forgotten: usize }
```

Types `DeviceId`, `CrewId`, `RejectReason` come from `duoclip-proto`. `SessionError`: `TtlTooShort`, `InvalidMaxSize`,
`UnknownCrew`, `NoLocalGame`, `NoCandidates`.

**Transport contract (Phase C).** Send `my_presence()` periodically (well under the TTL, e.g. every 5–10 s) **and right after any
tick that returned events**, so a commitment or a seat change reaches the others quickly (a session needs one exchange after
`Started` before friends appear as participants). Deliver a device's presence to the members of its crews. Authenticate presences
and drop replays: this crate only orders them (see Implementation notes).

## Tests (required)

- Two crews A and B, I'm in both, friends of A online in game G → session on A automatically; same with B on another day.
- Friends of A and B both online in G → NeedsChoice; choose B → Active(B) and remembered; next time (same game) Active(B) without asking;
  remembered choice not eligible → falls back to the rules. The prompt is dismissed (`ChoiceDismissed`) when the choice is made or
  the crews stop being eligible.
- Partial attendance: crew of 6, 3 online in G → participants = me + 2; a 4th joins → Joined; one leaves (TTL) → Left.
- Different game → not a candidate. `active_crew` = other crew → not a candidate (busy). A free (uncommitted) candidate is not a
  participant, not a clip target, and its requests are rejected.
- Stickiness: A active, A's only friend misses heartbeats for < grace → still Active; > grace → Ended/Idle (or switch to B).
  B becoming eligible while A is active → stays on A, no ChoiceNeeded. Heartbeats exactly at the TTL edge never flap. My game
  restart (None) keeps the session; switching to another game ends it at once.
- Anti-livelock: the lowest id holds `2 × grace` against rivals; two devices with conflicting remembered choices converge.
- Cap: 11 committed candidates → me + 7, overflow 4; 12 committed candidates → me + 7, overflow **5** (ties by id); seated devices
  keep their seat when a better-ranked newcomer arrives; a participant leaving frees a seat for the head of the queue; `Queued`
  state when I don't fit; `max_size` 2 and 3.
- `check_request`: participant of active crew → Ok; non-participant member, other crew, Idle, NeedsChoice, Queued, my own id →
  NotInSession. Removing a friend from the crew takes effect without waiting for a tick.
- `clip_targets` excludes me and is stable for the snapshot.
- Robustness: stale/duplicate seq ignored, a restarted app (new `online_since_ms`) is accepted at once and a delayed packet of its
  previous run is ignored, non-member ignored, my own device ignored, time going backwards, u64 extremes, oversized game id,
  > 256 devices, membership change removing the active crew → Ended.
- A property test with a seeded PRNG: random presence/time sequences never panic, `participants.len() <= max_size`, self is always
  first while Active, no device is both participant and overflow, participants are members of the crew under the current
  membership, the state equals the state rebuilt from the events alone (events never contradict the state, nothing changes
  silently), a repeated tick returns no events, and the state is a deterministic function of the inputs.
- A multi-device simulation (`tests/agreement.rs`): 2–10 managers exchanging `my_presence()` every round, two crews with overlapping
  members, random game changes / restarts / choices, `max_size` 2, 3 and 8. After a few quiet rounds: if A lists B, B lists A in the
  same crew and both accept each other's requests; fellow participants agree on the seat set; no device is a participant under two
  crews; queued devices are in everyone's queue and nobody's participants.
  The same scenarios also run with managers fed **only** Worker-style snapshots (`apply_snapshot`) through an in-memory fake
  Worker (receipt-time freshness with TTL 90 s, lexicographic `(online_since_ms, seq)` acceptance with 409, per-crew
  availability filter, complete snapshots of all the caller's crews), both every round and at the real 30 s cadence.
- `apply_snapshot` (`tests/snapshot.rs`): departures forgotten immediately (participant → `Left`), a repeated pair does not
  refresh freshness, my own device ignored, devices present in another crew's snapshot are kept, empty snapshot forgets everyone,
  the local TTL still expires peers when snapshots stop; the property test also mixes random snapshots in.

## Implementation notes

Deviations from, and clarifications of, the first version of this spec (the adversarial review of 2026-10-08 changed the
participant, cap and ordering rules; the reasons are kept here).

- **Why mutual commitment (rule 6).** Originally a free candidate (`active_crew = None`) was a participant at once. A member of two
  crews that was still in `NeedsChoice` was then a clip target of *both* crews' sessions at the same time, and any device in
  `NeedsChoice` was listed by friends that it did not list back. Now a session starts as soon as there is a candidate, but friends
  become participants only after they announce the same crew. Cost: one presence exchange after `Started`.
- **Why the global ranking (rule 7).** With a per-device "keep my current participants" rule, 10 devices of one crew each saw a
  full session of 8 with a *different* set of 8 (every device counts itself in), so clips went to devices that rejected them.
  The ranking uses only announced data, so all devices compute the same seats. Stickiness comes from the announced
  `seated_since_ms` (seated devices rank first) instead of local history. `Presence.seated_since_ms` and `SessionState::Queued`
  were added for this. Transient disagreement lasts at most one exchange (e.g. two newcomers racing for the last seat both take
  it, then the lower-ranked one queues).
- **Seat kept across a game restart.** A seated device whose game closes keeps `seated_since` during its grace (it stays `Active`,
  alone, seeing nobody). Meanwhile the others give its seat to the head of the queue; if it comes back within the grace it ranks
  first again and the promoted device returns to the queue. After the grace its session ends and it comes back as a newcomer. A
  queued device whose game closes never takes a free seat while alone (it would later use it to bump someone).
- **Presence ordering (`on_presence`).** An announcement replaces the stored one if `(online_since_ms, seq)` is greater
  (lexicographic) or the stored one has expired. A restarted app (new, greater `online_since_ms`, `seq` back to 1) is accepted at
  once, and a delayed packet of its previous run is ignored. A peer whose clock went back (e.g. reboot with a per-boot clock) is
  locked out for at most the TTL. Limits: a delayed or replayed packet arriving *after* the stored one expired is accepted and can
  make a departed peer look present for up to one TTL; replay protection and authenticity are the transport's job.
- **`my_presence` before any time.** `online_since_ms` is latched at the first timed input (`tick`, `on_presence`, `choose_crew`).
  `my_presence()` called before that reports the current clock (0) without latching it, so a run never announces a smaller
  `online_since_ms` than its real start.
- **Immediate re-evaluation.** `set_membership` and `set_local_game` re-evaluate at once (at the last time seen), so a removed friend
  or a crew I left is never a clip target / accepted requester, even before the next tick. `on_presence` and
  `set_remembered_choices` take effect at the next tick (presences of one heartbeat are batched, so two crews coming online together
  produce one `NeedsChoice` instead of an arbitrary pick). Events of immediate evaluations and of `choose_crew` are returned by the
  next tick.
- **`check_request` and self.** A request whose `from` is my own device is rejected with `NotInSession` even though I am in
  `participants` (a peer request can never legitimately come from me).
- **Events.** Order inside one evaluation: `ChoiceDismissed`, `Ended`, `Started`, `Left`s, `Joined`s, `Overflow`. `Started` reports the
  commitment; fellow participants then arrive as `Joined` (same tick if they were already committed). `Ended` is not preceded by
  `Left` events. When I get or lose my seat, `Joined`/`Left` are emitted for every fellow participant. `ChoiceNeeded` is emitted only
  when the game or the *set* of eligible crews changes; the state's crew order is the one of that event (it is not re-sorted while
  the set is unchanged), so the state always equals the last event. A new `ChoiceNeeded` replaces a pending one without
  `ChoiceDismissed`. `Overflow` is emitted whenever the queue changes *as a set*, including when it becomes empty (a pure reorder is
  not reported; the state has the current order).
- **Events from `choose_crew`.** `choose_crew` switches the state immediately (so `state()` is already `Active`/`Queued`), but the
  resulting events are returned by the next `tick`. Choosing the crew I am already in only refreshes it (and remembers it).
- **Grace measurement.** The grace window starts at the first evaluation that observes the active crew ineligible (not at the
  exact instant the last friend expired), and ends when `now - since >= grace` (`2 × switch_grace_ms` under the anti-livelock rule,
  saturating). During grace the session stays with the participants that are still candidates (usually just self), and `Left` events
  are emitted normally. Losing the crew from the membership or switching to another game ends the session immediately.
- **Anti-livelock scope.** The "lowest id holds longer" rule resolves the two-sided swap (the common case: two friends in both
  crews). It is a heuristic, not a consensus protocol: exotic cyclic preferences among three or more crews may take a few grace
  periods to settle. A device that moved does not change its remembered choice.
- **Oversized local game.** `set_local_game` with an id longer than 64 bytes is treated as `None`. `set_remembered_choices`
  drops entries with such ids and keeps at most 256 (`MAX_REMEMBERED_CHOICES`); when full, `choose_crew` for a new game evicts the
  lexicographically first game (not LRU). Choices for crews I left are kept (they only apply while that crew is eligible).
- **Online-since of this device.** `my_presence().online_since_ms` is the first `now_ms` the manager saw and is stable afterwards.
- **Clock extremes.** Time is clamped to never go backwards, so a caller that once passes a huge `now_ms` (e.g. `u64::MAX`) freezes
  the manager's clock there (presences stop expiring). All arithmetic saturates; nothing panics.
- **Event buffer.** Events accumulate until `tick` drains them; a caller that never ticks while calling `choose_crew` /
  `set_membership` / `set_local_game` repeatedly grows the buffer (caller contract: tick ~1 Hz).
- **Crew id leak.** `my_presence()` carries the id of my committed crew; if the transport sends it to members of my *other* crews,
  they learn an opaque crew UUID (and correctly treat me as busy). The transport may redact it per audience in Phase C.
- **Extras.** `tracked_devices()` (number of tracked peers) is public to make the 256-device cap testable; devices that are in
  none of my crews after `set_membership` are forgotten.
- **Spec arithmetic.** The first version said "12 candidates → me + 7, overflow 4"; 12 candidates give an overflow of **5**
  (fixed above). The tests cover 11 candidates (overflow 4) and 12 candidates (overflow 5).
- **Dev-dependency.** `uuid` (already in the workspace) is a dev-dependency only, to build deterministic ids in tests.
- **`apply_snapshot` (task 17).** Added for the Worker transport (`duoclip-presence`). Details:
  - A device listed several times (once per shared crew) counts once, with its greatest `(online_since_ms, seq)` pair.
  - A pair *equal* to the stored one never refreshes freshness, even if the stored one already expired locally (the Worker
    returns a row unchanged until it expires; only a newer pair proves a new heartbeat). An *older* pair follows `on_presence`:
    stale while the stored one is fresh, accepted once it expired (the Worker itself accepts any pair after expiry).
  - Entries with an oversized game id are ignored **and do not count as present** (the device is forgotten): an entry that
    cannot be used is treated as absent, which keeps clips away from a device whose state is unknown. Non-members and my
    own device are ignored (`ignored` counts entries; `accepted`/`stale` count distinct devices).
  - Members committed to a crew I am not in are omitted by the Worker's availability filter, so they are forgotten: they could
    never be my candidates anyway. Consequence for the anti-livelock rule: "rivals" are only visible when I am also in their
    crew (true in the two-sided swap it targets, where both devices are in both crews).
  - With snapshots arriving one device at a time, two crews that come online in the same second may be seen one after the
    other, so a device can commit to the first instead of getting `NeedsChoice` (a `NeedsChoice` needs both crews in the
    first snapshot, e.g. when I open the game after both crews are playing). Agreement holds after a quiet period: the Worker
    scenarios of `tests/agreement.rs` check the invariants once nothing happened (no event, no change announcement) for a whole
    heartbeat interval plus one round, because a transition may be half-propagated at an arbitrary instant.
  - Transport contract with the Worker: `presence_ttl_ms` = 90 000 and a heartbeat every 30 s plus immediate change
    announcements; `switch_grace_ms` must outlast the information delay (`duoclip-presence` raises it to 65 s; with the default
    10 s a friend's quick game restart can end the session, see that crate's notes).
