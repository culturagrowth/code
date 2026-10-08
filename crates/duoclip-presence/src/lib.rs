//! duoclip-presence: adapter between the Worker presence heartbeat (`POST /v1/presence`) and
//! [`duoclip_session::SessionManager`].
//!
//! Pure, portable logic: no I/O, no threads, no clock reads, and no panics on data from the
//! network. It decides **when** to send a heartbeat ([`PresenceClient::poll`]), **what** to send
//! ([`HeartbeatRequest`]) and **how** to apply the Worker's answer to the session
//! ([`PresenceClient::on_success`] / [`PresenceClient::on_failure`]). HTTP, Ed25519 request
//! signing and timers belong to the network layer (Phase C), which feeds this crate its local
//! monotonic time in milliseconds (`now_ms`). See `SPEC.md`.
//!
//! **Clocks.** The run id (`online_since_ms`) comes from the DuoClip global clock (AppClock) at
//! start. The session manager runs on "session time" = run id + (`now_ms` - first `now_ms` seen),
//! so the seat times it announces are on the AppClock scale and comparable across devices, and
//! the run id it ranks itself with is exactly the one announced.
//!
//! **Cadence.** While active (a local game, or any session state but `Idle`): one heartbeat per
//! interval (30 s) plus one right after each change of game, committed crew or seat. While idle:
//! a single announcement, then silence. The Worker's response carries complete snapshots of all
//! my crews, so peers are learned with the same request (no separate polling).

#![forbid(unsafe_code)]

mod wire;

use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{
    SessionConfig, SessionError, SessionEvent, SessionManager, SessionState, SnapshotOutcome,
};
use serde::{Deserialize, Serialize};

pub use wire::{
    is_valid_game_id, parse_canonical_uuid, CrewSnapshot, HeartbeatRequest, HeartbeatResponse,
    MemberPresence, MAX_SAFE_INTEGER,
};

/// Shortest heartbeat interval accepted from the Worker (ms).
pub const MIN_INTERVAL_MS: u64 = 10_000;
/// Longest heartbeat interval accepted from the Worker (ms), before the TTL bound (see
/// [`PresenceConfig::max_interval_ms`]).
pub const MAX_INTERVAL_MS: u64 = 300_000;
/// First retry delay after a network error or a 5xx (ms); doubles up to `max_backoff_ms`.
pub const INITIAL_BACKOFF_MS: u64 = 1_000;
/// Minimum spacing between a request and a change announcement that follows it (ms): a safety
/// floor against request storms, invisible in practice.
pub const MIN_CHANGE_GAP_MS: u64 = 1_000;
/// Added to `2 × max interval` to get the minimum session switch grace (ms).
pub const GRACE_MARGIN_MS: u64 = 5_000;

/// Errors of this crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PresenceError {
    /// The session configuration is invalid.
    #[error("invalid session config: {0}")]
    Session(#[from] SessionError),
    /// The presence configuration is invalid.
    #[error("invalid presence config: {0}")]
    InvalidConfig(&'static str),
    /// The AppClock value at start is not a JavaScript safe integer.
    #[error("app clock {0} ms is above the JavaScript safe integer range")]
    ClockOutOfRange(u64),
    /// The response body is not the expected JSON.
    #[error("malformed presence response: {0}")]
    MalformedResponse(String),
    /// The response body has `ok` other than `true`.
    #[error("presence response is not ok")]
    NotOk,
    /// An integer field is above [`MAX_SAFE_INTEGER`].
    #[error("{0} is not a JavaScript safe integer")]
    UnsafeInteger(&'static str),
    /// A request field would be rejected by the Worker.
    #[error("invalid request field: {0}")]
    InvalidRequest(&'static str),
}

/// Tunables of the heartbeat client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresenceConfig {
    /// Heartbeat period while active (ms), until the Worker sends its own (clamped).
    pub interval_ms: u64,
    /// Freshness of a peer's presence (ms); also the session's `presence_ttl_ms`. Must match
    /// the Worker TTL (90 s).
    pub presence_ttl_ms: u64,
    /// Ceiling of the exponential backoff after network errors / 5xx (ms).
    pub max_backoff_ms: u64,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            interval_ms: 30_000,
            presence_ttl_ms: 90_000,
            max_backoff_ms: 30_000,
        }
    }
}

impl PresenceConfig {
    /// Checks `interval_ms` in `10_000..=300_000`, `presence_ttl_ms >= 30_000` and
    /// `max_backoff_ms >= 1_000`.
    pub fn validate(&self) -> Result<(), PresenceError> {
        if !(MIN_INTERVAL_MS..=MAX_INTERVAL_MS).contains(&self.interval_ms) {
            return Err(PresenceError::InvalidConfig(
                "interval_ms must be in 10000..=300000",
            ));
        }
        if self.presence_ttl_ms < 3 * MIN_INTERVAL_MS {
            return Err(PresenceError::InvalidConfig(
                "presence_ttl_ms must be at least 30000",
            ));
        }
        if self.max_backoff_ms < INITIAL_BACKOFF_MS {
            return Err(PresenceError::InvalidConfig(
                "max_backoff_ms must be at least 1000",
            ));
        }
        Ok(())
    }

    /// Longest effective interval: `min(300_000, presence_ttl_ms / 3)`, so that a peer survives
    /// one lost heartbeat before its presence expires (30 s with the 90 s TTL).
    pub fn max_interval_ms(&self) -> u64 {
        MAX_INTERVAL_MS
            .min(self.presence_ttl_ms / 3)
            .max(MIN_INTERVAL_MS)
    }

    /// Clamps a requested interval into `MIN_INTERVAL_MS..=max_interval_ms()`.
    pub fn clamp_interval(&self, interval_ms: u64) -> u64 {
        interval_ms.clamp(MIN_INTERVAL_MS, self.max_interval_ms())
    }

    /// Smallest session `switch_grace_ms` that works with this cadence:
    /// `2 × max_interval_ms() + GRACE_MARGIN_MS` (65 s by default). A peer's change is seen up to
    /// one interval late, so a shorter grace would end a session on a friend's quick game
    /// restart, and the anti-livelock rule (lower id holds 2 × grace) needs the extra interval to
    /// see the move.
    pub fn min_switch_grace_ms(&self) -> u64 {
        self.max_interval_ms()
            .saturating_mul(2)
            .saturating_add(GRACE_MARGIN_MS)
    }
}

/// What the app stores between runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedState {
    /// The run id (`online_since_ms`) of the last run.
    pub last_online_since_ms: u64,
}

/// Why a heartbeat failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeartbeatFailure {
    /// `409 stale_presence`: the Worker holds a fresh row with a newer or equal run/sequence
    /// pair (typically a previous run with a larger run id). Retried at the next interval.
    Stale409,
    /// Any other HTTP status (a 409 here is treated as [`Self::Stale409`]).
    Http(u16),
    /// No response (connection error, timeout).
    Network,
    /// A `200` whose body could not be parsed ([`HeartbeatResponse::from_json`] failed).
    InvalidResponse,
}

/// The parts of a heartbeat that trigger an immediate announcement when they change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Announcement {
    game: Option<String>,
    active_crew: Option<CrewId>,
    seated_since_ms: Option<u64>,
}

/// Heartbeat scheduler and response adapter; owns the [`SessionManager`].
#[derive(Clone, Debug)]
pub struct PresenceClient {
    session: SessionManager,
    cfg: PresenceConfig,
    /// Run id announced as `online_since_ms` (AppClock ms at start, never below the last run's).
    online_since_ms: u64,
    /// First `now_ms` seen: session time = `online_since_ms + (now_ms - base_now_ms)`.
    base_now_ms: Option<u64>,
    /// Sequence of the last request built (0 before the first).
    seq: u64,
    /// Effective heartbeat interval (clamped).
    interval_ms: u64,
    in_flight: bool,
    /// What the Worker holds for this run (`None` = unknown: nothing sent yet or last failed).
    announced: Option<Announcement>,
    /// `now_ms` of the last request built.
    last_sent_ms: Option<u64>,
    /// When the next periodic heartbeat is due (set by a success).
    next_periodic_ms: Option<u64>,
    /// Nothing is sent before this (`now_ms`), after a failure.
    retry_at_ms: Option<u64>,
    /// Consecutive network / 5xx failures (drives the exponential backoff).
    network_failures: u32,
    last_snapshot: Option<SnapshotOutcome>,
}

impl PresenceClient {
    /// Builds the client and its [`SessionManager`], with `presence_ttl_ms` =
    /// `cfg.presence_ttl_ms` and `switch_grace_ms` raised to at least
    /// [`PresenceConfig::min_switch_grace_ms`]. The run id (`online_since_ms`) is
    /// `max(app_clock_ms_at_start, persisted.last_online_since_ms + 1)`, so a restart never
    /// announces a smaller run id (the Worker would answer 409 for up to 90 s). A persisted value
    /// at or above [`MAX_SAFE_INTEGER`] is treated as corrupt and ignored.
    pub fn new(
        me: DeviceId,
        session: SessionConfig,
        cfg: PresenceConfig,
        app_clock_ms_at_start: u64,
        persisted: Option<PersistedState>,
    ) -> Result<Self, PresenceError> {
        cfg.validate()?;
        if app_clock_ms_at_start > MAX_SAFE_INTEGER {
            return Err(PresenceError::ClockOutOfRange(app_clock_ms_at_start));
        }
        let after_last = persisted
            .map(|p| p.last_online_since_ms)
            .filter(|last| *last < MAX_SAFE_INTEGER)
            .map_or(0, |last| last + 1);
        let online_since_ms = app_clock_ms_at_start.max(after_last);
        let session_cfg = SessionConfig {
            presence_ttl_ms: cfg.presence_ttl_ms,
            switch_grace_ms: session.switch_grace_ms.max(cfg.min_switch_grace_ms()),
            ..session
        };
        let mut manager = SessionManager::new(me, session_cfg)?;
        // Latch the session's run id (its first timed input) to exactly the announced one, so
        // it ranks itself in the queue with the same value its peers see.
        let _ = manager.tick(online_since_ms);
        let interval_ms = cfg.clamp_interval(cfg.interval_ms);
        Ok(Self {
            session: manager,
            cfg,
            online_since_ms,
            base_now_ms: None,
            seq: 0,
            interval_ms,
            in_flight: false,
            announced: None,
            last_sent_ms: None,
            next_periodic_ms: None,
            retry_at_ms: None,
            network_failures: 0,
            last_snapshot: None,
        })
    }

    /// What the app must store for the next run.
    pub fn persisted_state(&self) -> PersistedState {
        PersistedState {
            last_online_since_ms: self.online_since_ms,
        }
    }

    /// The session manager (state, clip targets, request gate).
    pub fn session(&self) -> &SessionManager {
        &self.session
    }

    /// The session manager, for membership, local game and remembered choices. Calls that take
    /// a time (`choose_crew`, `tick`, `on_presence`) must be given [`Self::session_time`]; prefer
    /// [`Self::choose_crew`] and [`Self::tick`].
    pub fn session_mut(&mut self) -> &mut SessionManager {
        &mut self.session
    }

    /// `choose_crew` on the session, at the session time of `now_ms`.
    pub fn choose_crew(&mut self, crew: CrewId, now_ms: u64) -> Result<(), SessionError> {
        let t = self.session_time(now_ms);
        self.session.choose_crew(crew, t)
    }

    /// The session time (AppClock scale) of the local `now_ms`: the run id plus the local time
    /// elapsed since the first `now_ms` this client saw.
    pub fn session_time(&mut self, now_ms: u64) -> u64 {
        let base = *self.base_now_ms.get_or_insert(now_ms);
        self.online_since_ms
            .saturating_add(now_ms.saturating_sub(base))
    }

    /// The run id announced as `online_since_ms`.
    pub fn online_since_ms(&self) -> u64 {
        self.online_since_ms
    }

    /// The effective heartbeat interval (ms).
    pub fn interval_ms(&self) -> u64 {
        self.interval_ms
    }

    /// Whether a request was built and its outcome not reported yet.
    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// What the last applied snapshot did (diagnostics).
    pub fn last_snapshot(&self) -> Option<SnapshotOutcome> {
        self.last_snapshot
    }

    /// Returns the request to send now, or `None`. Call it after [`Self::tick`] (about 1 Hz) and
    /// after every change made through [`Self::session_mut`] / [`Self::choose_crew`]. Due when:
    /// - active (a local game, or the session is not `Idle`): every interval, and right after a
    ///   change of game, committed crew or seat compared with what the Worker holds (at most one
    ///   change announcement per [`MIN_CHANGE_GAP_MS`]);
    /// - idle: one announcement with `game = None` after becoming idle (or at start), then none.
    ///
    /// Never while a request is in flight or a failure backoff runs. `seq` increases on every
    /// request built; a `seq` beyond [`MAX_SAFE_INTEGER`] is never built.
    pub fn poll(&mut self, now_ms: u64) -> Option<HeartbeatRequest> {
        let _ = self.session_time(now_ms);
        if self.in_flight || self.retry_at_ms.is_some_and(|t| now_ms < t) {
            return None;
        }
        // Only game / crew / seat are read; the session's own sequence number is not used.
        let p = self.session.my_presence();
        let current = Announcement {
            // A game id the Worker would refuse is announced as "no game" (never sent).
            game: p.game.clone().filter(|g| is_valid_game_id(g)),
            active_crew: p.active_crew,
            seated_since_ms: p.seated_since_ms.map(|t| t.min(MAX_SAFE_INTEGER)),
        };
        let active = p.game.is_some() || *self.session.state() != SessionState::Idle;
        let changed = self.announced.as_ref() != Some(&current)
            && self
                .last_sent_ms
                .is_none_or(|t| now_ms.saturating_sub(t) >= MIN_CHANGE_GAP_MS);
        let periodic = active && self.next_periodic_ms.is_none_or(|t| now_ms >= t);
        if !(changed || periodic) {
            return None;
        }
        let seq = self.seq.checked_add(1).filter(|s| *s <= MAX_SAFE_INTEGER)?;
        self.seq = seq;
        self.in_flight = true;
        self.last_sent_ms = Some(now_ms);
        let request = HeartbeatRequest {
            game: current.game.clone(),
            active_crew: current.active_crew.map(|c| c.to_string()),
            seated_since_ms: current.seated_since_ms,
            seq,
            online_since_ms: self.online_since_ms,
        };
        self.announced = Some(current);
        Some(request)
    }

    /// Applies a `200` response: every valid member of every crew snapshot becomes a
    /// `Presence`, the merged list goes to `SessionManager::apply_snapshot` (departures are
    /// forgotten at once), then the session ticks. Adopts the Worker's `heartbeat_interval_ms`
    /// (clamped, see [`PresenceConfig::clamp_interval`]). Returns the session events. A response
    /// with `ok != true` is handled as [`HeartbeatFailure::InvalidResponse`].
    pub fn on_success(&mut self, resp: HeartbeatResponse, now_ms: u64) -> Vec<SessionEvent> {
        if !resp.ok {
            return self.on_failure(HeartbeatFailure::InvalidResponse, now_ms);
        }
        let t = self.session_time(now_ms);
        self.in_flight = false;
        self.retry_at_ms = None;
        self.network_failures = 0;
        self.interval_ms = self.cfg.clamp_interval(resp.heartbeat_interval_ms);
        self.next_periodic_ms = Some(now_ms.saturating_add(self.interval_ms));
        self.last_snapshot = Some(self.session.apply_snapshot(resp.presences(), t));
        self.session.tick(t)
    }

    /// Reports a failed request. `409` (stale pair): keep the run id and retry at the next
    /// interval (no loop). Network error / 5xx: exponential backoff from 1 s up to
    /// `max_backoff_ms`. Other statuses and unparsable bodies: retry at the next interval.
    /// Until then nothing is sent; the local TTL keeps expiring peers meanwhile. Returns the
    /// session events of a tick.
    pub fn on_failure(&mut self, failure: HeartbeatFailure, now_ms: u64) -> Vec<SessionEvent> {
        let t = self.session_time(now_ms);
        self.in_flight = false;
        // Unknown what the Worker holds now: announce again as soon as allowed.
        self.announced = None;
        let delay = match failure {
            HeartbeatFailure::Network | HeartbeatFailure::Http(500..=599) => {
                self.network_failures = self.network_failures.saturating_add(1);
                self.backoff_ms()
            }
            HeartbeatFailure::Stale409
            | HeartbeatFailure::Http(_)
            | HeartbeatFailure::InvalidResponse => {
                self.network_failures = 0;
                self.interval_ms
            }
        };
        self.retry_at_ms = Some(now_ms.saturating_add(delay));
        self.session.tick(t)
    }

    /// Periodic housekeeping (call about 1 Hz): `SessionManager::tick` at the session time.
    pub fn tick(&mut self, now_ms: u64) -> Vec<SessionEvent> {
        let t = self.session_time(now_ms);
        self.session.tick(t)
    }

    /// `1 s × 2^(failures - 1)`, capped at `max_backoff_ms`.
    fn backoff_ms(&self) -> u64 {
        let doublings = self.network_failures.saturating_sub(1);
        let factor = 1u64.checked_shl(doublings).unwrap_or(u64::MAX);
        INITIAL_BACKOFF_MS
            .saturating_mul(factor)
            .min(self.cfg.max_backoff_ms)
    }
}
