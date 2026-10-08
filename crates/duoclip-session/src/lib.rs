//! duoclip-session: automatic per-group play sessions.
//!
//! The **session** is the set of devices that take part in a clip right now. It forms
//! automatically: members of the same crew, with the app open, playing the same game, at most
//! [`MAX_SESSION_SIZE`] devices (self included).
//!
//! This crate is pure decision logic: no I/O, no threads and no clock reads. The caller feeds it
//! membership, presence announcements and its own monotonic time in milliseconds; it returns the
//! session state and events. It never panics on untrusted (peer-supplied) input, and every
//! container is ordered (`BTree*`), so state and event order are a deterministic function of the
//! inputs. See `SPEC.md` for the full contract.
//!
//! Agreement between devices comes from two rules (see `SPEC.md`):
//! - a peer only becomes a **participant** once it has itself *committed* to the same crew (it
//!   announces `active_crew == Some(crew)`), so "A lists B" implies "B is in a session with A's
//!   crew", and a device is never a participant of two crews at once;
//! - the size cap is decided by a **ranking every device computes identically** from the
//!   announced presences (seated devices first, by `seated_since_ms`; then queued ones by
//!   `online_since_ms`; then device id), so all devices agree on who is seated and who is queued.

#![forbid(unsafe_code)]

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};

use duoclip_proto::{CrewId, DeviceId, RejectReason};

/// Hard upper bound of devices in one session (self included).
pub const MAX_SESSION_SIZE: usize = 8;
/// Longest accepted game id, in bytes. Longer announcements are ignored.
pub const MAX_GAME_ID_BYTES: usize = 64;
/// Most devices whose presence is tracked at once; the oldest seen is evicted beyond this.
pub const MAX_TRACKED_DEVICES: usize = 256;
/// Most remembered per-game crew choices kept in memory.
pub const MAX_REMEMBERED_CHOICES: usize = 256;

/// Errors returned by [`SessionConfig::validate`], [`SessionManager::new`] and
/// [`SessionManager::choose_crew`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// `presence_ttl_ms` is below the 1000 ms minimum.
    #[error("presence_ttl_ms must be at least 1000 (got {0})")]
    TtlTooShort(u64),
    /// `max_size` is outside `2..=8`.
    #[error("max_size must be in 2..=8 (got {0})")]
    InvalidMaxSize(usize),
    /// The crew is not one of my crews.
    #[error("crew {0} is not one of my crews")]
    UnknownCrew(CrewId),
    /// No local game is running, so no session can form.
    #[error("no local game is running")]
    NoLocalGame,
    /// The crew has no candidate right now.
    #[error("crew {0} has no candidate right now")]
    NoCandidates(CrewId),
}

/// Tunables of the session logic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionConfig {
    /// A peer's presence is considered fresh for this long after it was received (ms).
    pub presence_ttl_ms: u64,
    /// How long an active session survives while its crew is no longer eligible (ms).
    pub switch_grace_ms: u64,
    /// Maximum devices in a session, self included (`2..=8`).
    pub max_size: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            presence_ttl_ms: 30_000,
            switch_grace_ms: 10_000,
            max_size: MAX_SESSION_SIZE,
        }
    }
}

impl SessionConfig {
    /// Checks `presence_ttl_ms >= 1000` and `max_size` in `2..=8`.
    pub fn validate(&self) -> Result<(), SessionError> {
        if self.presence_ttl_ms < 1000 {
            return Err(SessionError::TtlTooShort(self.presence_ttl_ms));
        }
        if !(2..=MAX_SESSION_SIZE).contains(&self.max_size) {
            return Err(SessionError::InvalidMaxSize(self.max_size));
        }
        Ok(())
    }
}

/// What a device announces periodically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presence {
    /// The announcing device.
    pub device: DeviceId,
    /// Increasing per sender within one run of the app (see `online_since_ms`).
    pub seq: u64,
    /// games-db id of the foreground game, or `None`.
    pub game: Option<String>,
    /// The crew the sender is committed to (in a session with, seated or queued), if any.
    pub active_crew: Option<CrewId>,
    /// `Some(t)` while the sender holds a seat in the session of `active_crew` (it is
    /// [`SessionState::Active`]), `t` being when it got the seat in the sender's clock. `None`
    /// when it is queued or not in a session. Used to rank devices under the size cap.
    pub seated_since_ms: Option<u64>,
    /// When the sender's app came online, in the sender's clock. Orders queued devices and
    /// identifies a run of the app (a restart announces a new value).
    pub online_since_ms: u64,
}

/// The current session decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// No session.
    Idle,
    /// Several crews are eligible and no remembered choice applies: the user must pick one.
    NeedsChoice {
        /// The local game.
        game: String,
        /// Eligible crews, by number of candidates descending, then crew id (as of the moment
        /// this set of crews was first reported; see `SPEC.md`).
        crews: Vec<CrewId>,
    },
    /// A session is running and I hold one of its seats.
    Active {
        /// The crew of the session.
        crew: CrewId,
        /// The game of the session.
        game: String,
        /// Seated devices: self first, then join order (as observed by this device).
        participants: Vec<DeviceId>,
        /// Committed devices that did not fit under the size cap, in queue order.
        overflow: Vec<DeviceId>,
    },
    /// I am committed to a session on `crew` but it is full: I wait for a free seat. Nobody
    /// sends me clips and I accept none.
    Queued {
        /// The crew of the session.
        crew: CrewId,
        /// The game of the session.
        game: String,
        /// The queue, including me, in queue order.
        overflow: Vec<DeviceId>,
    },
}

/// A change of the session, reported by [`SessionManager::tick`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// A session started (I committed to `crew`). Seats are reported by `Joined`/`Overflow`.
    Started {
        /// The crew of the session.
        crew: CrewId,
        /// The game of the session.
        game: String,
    },
    /// The session ended (no `Left` is emitted for its participants).
    Ended {
        /// The crew of the ended session.
        crew: CrewId,
    },
    /// A friend became one of my fellow participants (I am seated and so are they).
    Joined {
        /// The crew of the session.
        crew: CrewId,
        /// The device that joined.
        device: DeviceId,
    },
    /// A friend stopped being one of my fellow participants (presence expired or stopped
    /// matching, or one of us lost the seat).
    Left {
        /// The crew of the session.
        crew: CrewId,
        /// The device that left.
        device: DeviceId,
    },
    /// The user must pick a crew (replaces any previous pending choice).
    ChoiceNeeded {
        /// The local game.
        game: String,
        /// The eligible crews (see [`SessionState::NeedsChoice`]).
        crews: Vec<CrewId>,
    },
    /// The pending choice is no longer asked (it was made, or the crews stopped being eligible).
    ChoiceDismissed {
        /// The game of the dismissed choice.
        game: String,
    },
    /// The overflow queue changed as a set (it may now be empty).
    Overflow {
        /// The crew of the session.
        crew: CrewId,
        /// The queued devices, in queue order (includes me when I am queued).
        devices: Vec<DeviceId>,
    },
}

/// What [`SessionManager::apply_snapshot`] did with a snapshot (for diagnostics).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapshotOutcome {
    /// Distinct devices whose presence was recorded (newer run/sequence pair).
    pub accepted: usize,
    /// Distinct devices listed with a pair that is not newer than the stored one (kept as they
    /// were: a repeated pair never refreshes freshness).
    pub stale: usize,
    /// Entries ignored: my own device, devices in none of my crews, oversized game ids.
    pub ignored: usize,
    /// Tracked devices absent from the snapshot, forgotten immediately.
    pub forgotten: usize,
}

/// The last presence received from a peer, stamped with the receiver's clock.
#[derive(Clone, Debug)]
struct Peer {
    presence: Presence,
    seen_at_ms: u64,
}

/// Ranking key under the size cap: seated devices first (by seat time), then queued ones (by
/// online time), then device id. Every device computes the same key for the same presence.
type RankKey = (u8, u64, DeviceId);

fn rank_key(device: DeviceId, seated_since: Option<u64>, online_since: u64) -> RankKey {
    match seated_since {
        Some(t) => (0, t, device),
        None => (1, online_since, device),
    }
}

/// Decides, from membership and presence, which session (if any) this device is in.
#[derive(Clone, Debug)]
pub struct SessionManager {
    me: DeviceId,
    config: SessionConfig,
    membership: BTreeMap<CrewId, BTreeSet<DeviceId>>,
    local_game: Option<String>,
    peers: BTreeMap<DeviceId, Peer>,
    remembered: BTreeMap<String, CrewId>,
    state: SessionState,
    /// Since when the active crew has been observed ineligible.
    grace_since: Option<u64>,
    /// When I got my seat in the current session (`Some` only while `Active`).
    seated_since: Option<u64>,
    /// Highest `now_ms` seen so far; time never goes backwards.
    last_now: u64,
    online_since: Option<u64>,
    seq: u64,
    events: Vec<SessionEvent>,
}

impl SessionManager {
    /// Creates a manager for device `me`. Fails if `config` is invalid.
    pub fn new(me: DeviceId, config: SessionConfig) -> Result<Self, SessionError> {
        config.validate()?;
        Ok(Self {
            me,
            config,
            membership: BTreeMap::new(),
            local_game: None,
            peers: BTreeMap::new(),
            remembered: BTreeMap::new(),
            state: SessionState::Idle,
            grace_since: None,
            seated_since: None,
            last_now: 0,
            online_since: None,
            seq: 0,
            events: Vec::new(),
        })
    }

    /// Replaces the membership (crew -> members) and re-evaluates immediately (at the last time
    /// seen), so a removed friend or a crew I left stops being a participant / clip target right
    /// away. An active session on a crew I left ends with no grace. Devices that are in none of
    /// my crews are forgotten. The resulting events are returned by the next tick.
    pub fn set_membership(&mut self, crews: BTreeMap<CrewId, BTreeSet<DeviceId>>) {
        self.membership = crews;
        // Devices that are in none of my crews can never matter again: forget them.
        let known: BTreeSet<DeviceId> = self.membership.values().flatten().copied().collect();
        self.peers.retain(|device, _| known.contains(device));
        self.evaluate(self.last_now);
    }

    /// Sets the game in the foreground (games-db id), or `None`, and re-evaluates immediately
    /// (at the last time seen). Ids longer than [`MAX_GAME_ID_BYTES`] are treated as `None`.
    /// Switching to a *different* game ends the session at once; `None` (game closed or
    /// restarting) keeps it for the grace period. Events are returned by the next tick.
    pub fn set_local_game(&mut self, game: Option<String>) {
        self.local_game = game.filter(|g| g.len() <= MAX_GAME_ID_BYTES);
        self.evaluate(self.last_now);
    }

    /// Records a peer announcement, stamping it with `now_ms` (the receiver's clock). Takes
    /// effect at the next tick.
    ///
    /// Returns `false` (ignored) for: my own device, devices in none of my crews, game ids
    /// longer than [`MAX_GAME_ID_BYTES`], and announcements not newer than the stored one while
    /// that one is still fresh. "Newer" compares `(online_since_ms, seq)`: a restarted app
    /// (new `online_since_ms`) replaces its previous run at once, and a delayed packet of an
    /// older run is ignored. Once the stored one has expired any announcement is accepted (so a
    /// peer whose clock reset after a reboot is locked out for at most the TTL).
    /// At most [`MAX_TRACKED_DEVICES`] devices are tracked (the oldest seen is evicted).
    pub fn on_presence(&mut self, p: Presence, now_ms: u64) -> bool {
        let now = self.advance(now_ms);
        if p.device == self.me || !self.is_member_of_any(&p.device) {
            return false;
        }
        if p.game.as_ref().is_some_and(|g| g.len() > MAX_GAME_ID_BYTES) {
            return false;
        }
        match self.peers.get(&p.device) {
            Some(old) => {
                let newer =
                    (p.online_since_ms, p.seq) > (old.presence.online_since_ms, old.presence.seq);
                if !newer && self.is_fresh(old, now) {
                    return false;
                }
            }
            None => {
                if self.peers.len() >= MAX_TRACKED_DEVICES {
                    let oldest = self
                        .peers
                        .iter()
                        .min_by_key(|(device, peer)| (peer.seen_at_ms, **device))
                        .map(|(device, _)| *device);
                    if let Some(oldest) = oldest {
                        self.peers.remove(&oldest);
                    }
                }
            }
        }
        self.peers.insert(
            p.device,
            Peer {
                presence: p,
                seen_at_ms: now,
            },
        );
        true
    }

    /// Applies a complete presence snapshot (the Worker's `POST /v1/presence` response: every
    /// fresh and available member of ALL my crews, merged into one list), stamped with `now_ms`.
    /// Takes effect at the next tick, like [`on_presence`](Self::on_presence).
    ///
    /// The snapshot is authoritative for departures: every tracked device absent from it (and
    /// not me) is forgotten at once, so a participant becomes `Left` on the next tick instead of
    /// waiting for the local TTL. Listed devices go through the `on_presence` rules, except
    /// that a pair `(online_since_ms, seq)` equal to the stored one never refreshes freshness
    /// (the Worker repeats a row until it expires; only a newer pair proves a new heartbeat).
    /// A device listed several times (one entry per shared crew) counts once, with its greatest
    /// pair. Entries with oversized game ids are ignored and do not count as present. The local
    /// TTL keeps expiring peers when snapshots stop arriving.
    pub fn apply_snapshot(&mut self, members: Vec<Presence>, now_ms: u64) -> SnapshotOutcome {
        let now = self.advance(now_ms);
        let mut outcome = SnapshotOutcome::default();
        let mut latest: BTreeMap<DeviceId, Presence> = BTreeMap::new();
        for p in members {
            let oversized = p.game.as_ref().is_some_and(|g| g.len() > MAX_GAME_ID_BYTES);
            if p.device == self.me || oversized {
                outcome.ignored = outcome.ignored.saturating_add(1);
                continue;
            }
            match latest.get(&p.device) {
                Some(kept) if (p.online_since_ms, p.seq) <= (kept.online_since_ms, kept.seq) => {}
                _ => {
                    latest.insert(p.device, p);
                }
            }
        }

        // Departures: whoever is not listed has left (or is busy in a crew I am not in).
        let before = self.peers.len();
        self.peers.retain(|device, _| latest.contains_key(device));
        outcome.forgotten = before - self.peers.len();

        for (device, p) in latest {
            if !self.is_member_of_any(&device) {
                outcome.ignored = outcome.ignored.saturating_add(1);
                continue;
            }
            let repeated = self.peers.get(&device).is_some_and(|old| {
                (p.online_since_ms, p.seq) == (old.presence.online_since_ms, old.presence.seq)
            });
            if !repeated && self.on_presence(p, now) {
                outcome.accepted = outcome.accepted.saturating_add(1);
            } else {
                outcome.stale = outcome.stale.saturating_add(1);
            }
        }
        outcome
    }

    /// User picks a crew (in `NeedsChoice`, or switches explicitly while in a session). The
    /// choice is remembered for the current game. Errors if the crew is not one of mine, there is
    /// no local game, or the crew has no candidate. The state changes immediately; the resulting
    /// events are returned by the next tick.
    pub fn choose_crew(&mut self, crew: CrewId, now_ms: u64) -> Result<(), SessionError> {
        let now = self.advance(now_ms);
        if !self.membership.contains_key(&crew) {
            return Err(SessionError::UnknownCrew(crew));
        }
        let game = self.local_game.clone().ok_or(SessionError::NoLocalGame)?;
        if self.candidates(crew, now).is_empty() {
            return Err(SessionError::NoCandidates(crew));
        }
        self.remember(game.clone(), crew);
        match self.session() {
            Some((active, active_game)) if active == crew && active_game == game => {
                self.grace_since = None;
                self.refresh(crew, game, now);
            }
            Some((active, _)) => {
                self.end_session(active);
                self.start_session(crew, game, now);
            }
            None => {
                self.dismiss_choice();
                self.start_session(crew, game, now);
            }
        }
        Ok(())
    }

    /// Re-evaluates everything at `now_ms` (call ~1 Hz and after every input) and returns the
    /// events since the last tick. A `now_ms` going backwards is treated as the last one seen.
    pub fn tick(&mut self, now_ms: u64) -> Vec<SessionEvent> {
        let now = self.advance(now_ms);
        self.evaluate(now);
        std::mem::take(&mut self.events)
    }

    /// The current session state (as of the last evaluation).
    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// What to announce: my game, my committed crew, my seat time, `online_since_ms`, and a
    /// `seq` incremented on every call (the first call returns 1). Announce it periodically and
    /// also right after any tick that returned events (so peers see a commitment quickly).
    pub fn my_presence(&mut self) -> Presence {
        self.seq = self.seq.saturating_add(1);
        // Not latched here: before the first timed input the clock is unknown (0), and latching
        // 0 would make this run look *older* than a previous run to peers.
        let online_since_ms = self.online_since.unwrap_or(self.last_now);
        Presence {
            device: self.me,
            seq: self.seq,
            game: self.local_game.clone(),
            active_crew: self.session().map(|(crew, _)| crew),
            seated_since_ms: self.seated_since,
            online_since_ms,
        }
    }

    /// Snapshot for a `ClipRequest`: the active crew and the OTHER participants, or `None`
    /// unless `Active` (seated). The result is an owned copy: a friend who joins later is not
    /// part of an already-sent request, so the caller keeps this snapshot per clip.
    pub fn clip_targets(&self) -> Option<(CrewId, Vec<DeviceId>)> {
        match &self.state {
            SessionState::Active {
                crew, participants, ..
            } => Some((
                *crew,
                participants
                    .iter()
                    .copied()
                    .filter(|d| *d != self.me)
                    .collect(),
            )),
            _ => None,
        }
    }

    /// Gate for an incoming `ClipRequest` (crew taken from the envelope): `Ok` only if I am
    /// `Active` (seated) on `crew` and `from` is a current participant other than me; otherwise
    /// `Err(RejectReason::NotInSession)`.
    pub fn check_request(&self, from: DeviceId, crew: CrewId) -> Result<(), RejectReason> {
        match &self.state {
            SessionState::Active {
                crew: active,
                participants,
                ..
            } if *active == crew && from != self.me && participants.contains(&from) => Ok(()),
            _ => Err(RejectReason::NotInSession),
        }
    }

    /// The remembered crew choice per game id (the app persists this itself).
    pub fn remembered_choices(&self) -> &BTreeMap<String, CrewId> {
        &self.remembered
    }

    /// Replaces the remembered choices (e.g. loaded from disk). Entries with game ids longer
    /// than [`MAX_GAME_ID_BYTES`] are dropped and at most [`MAX_REMEMBERED_CHOICES`] are kept.
    /// Takes effect at the next tick (it never ends a running session).
    pub fn set_remembered_choices(&mut self, choices: BTreeMap<String, CrewId>) {
        self.remembered = choices
            .into_iter()
            .filter(|(game, _)| game.len() <= MAX_GAME_ID_BYTES)
            .take(MAX_REMEMBERED_CHOICES)
            .collect();
    }

    /// Number of devices whose presence is currently tracked (at most [`MAX_TRACKED_DEVICES`]).
    pub fn tracked_devices(&self) -> usize {
        self.peers.len()
    }

    // ----- internals -----

    /// Moves the internal clock forward (never backwards) and returns the effective time.
    fn advance(&mut self, now_ms: u64) -> u64 {
        self.last_now = self.last_now.max(now_ms);
        self.online_since.get_or_insert(self.last_now);
        self.last_now
    }

    /// The crew and game of the session I am committed to (seated or queued).
    fn session(&self) -> Option<(CrewId, String)> {
        match &self.state {
            SessionState::Active { crew, game, .. } | SessionState::Queued { crew, game, .. } => {
                Some((*crew, game.clone()))
            }
            _ => None,
        }
    }

    fn is_member_of_any(&self, device: &DeviceId) -> bool {
        self.membership.values().any(|m| m.contains(device))
    }

    fn is_fresh(&self, peer: &Peer, now: u64) -> bool {
        now.saturating_sub(peer.seen_at_ms) <= self.config.presence_ttl_ms
    }

    fn remember(&mut self, game: String, crew: CrewId) {
        if !self.remembered.contains_key(&game) && self.remembered.len() >= MAX_REMEMBERED_CHOICES {
            self.remembered.pop_first();
        }
        self.remembered.insert(game, crew);
    }

    /// Fresh peers that are members of `crew` and play my local game.
    fn same_game_members(&self, crew: CrewId, now: u64) -> impl Iterator<Item = &Peer> + '_ {
        let game = self.local_game.as_ref();
        self.membership
            .get(&crew)
            .into_iter()
            .flatten()
            .filter(move |device| **device != self.me)
            .filter_map(move |device| self.peers.get(device))
            .filter(move |peer| self.is_fresh(peer, now))
            .filter(move |peer| game.is_some() && peer.presence.game.as_ref() == game)
    }

    /// Candidates for `crew` at `now`: fresh members playing my game that are free or already
    /// committed to `crew` (someone committed to another crew is busy).
    fn candidates(&self, crew: CrewId, now: u64) -> Vec<&Peer> {
        self.same_game_members(crew, now)
            .filter(|peer| peer.presence.active_crew.is_none_or(|c| c == crew))
            .collect()
    }

    /// Eligible crews with their candidate counts: most candidates first, then crew id.
    fn eligible_crews(&self, now: u64) -> Vec<(CrewId, usize)> {
        let mut out: Vec<(CrewId, usize)> = self
            .membership
            .keys()
            .filter_map(|crew| {
                let n = self.candidates(*crew, now).len();
                (n > 0).then_some((*crew, n))
            })
            .collect();
        out.sort_unstable_by_key(|(crew, n)| (Reverse(*n), *crew));
        out
    }

    /// How long the session on `crew` survives while ineligible. Anti-livelock tie-break: when
    /// members of the crew are in my game but committed to another crew ("rivals"), and my id
    /// is lower than all of theirs, I hold twice as long, so the rivals (which use the normal
    /// grace) come to my crew instead of both sides swapping crews at the same moment forever.
    fn grace_len(&self, crew: CrewId, now: u64) -> u64 {
        let lowest_rival = self
            .same_game_members(crew, now)
            .filter(|peer| peer.presence.active_crew.is_some_and(|c| c != crew))
            .map(|peer| peer.presence.device)
            .min();
        match lowest_rival {
            Some(rival) if self.me < rival => self.config.switch_grace_ms.saturating_mul(2),
            _ => self.config.switch_grace_ms,
        }
    }

    fn evaluate(&mut self, now: u64) {
        let Some((crew, game)) = self.session() else {
            self.select(now);
            return;
        };
        let other_game = self.local_game.as_ref().is_some_and(|g| *g != game);
        if !self.membership.contains_key(&crew) || other_game {
            // I left the crew, or switched to another game: a new session, no grace.
            self.end_session(crew);
            self.select(now);
            return;
        }
        if self.local_game.is_some() && !self.candidates(crew, now).is_empty() {
            self.grace_since = None;
            self.refresh(crew, game, now);
            return;
        }
        let since = *self.grace_since.get_or_insert(now);
        if now.saturating_sub(since) >= self.grace_len(crew, now) {
            self.end_session(crew);
            self.select(now);
        } else {
            self.refresh(crew, game, now);
        }
    }

    /// Rules 1-3: pick the session from scratch (I am not committed to any crew here).
    fn select(&mut self, now: u64) {
        let Some(game) = self.local_game.clone() else {
            self.dismiss_choice();
            self.state = SessionState::Idle;
            return;
        };
        let eligible = self.eligible_crews(now);
        let remembered = self
            .remembered
            .get(&game)
            .copied()
            .filter(|r| eligible.iter().any(|(c, _)| c == r));
        let chosen = match eligible.as_slice() {
            [] => {
                self.dismiss_choice();
                self.state = SessionState::Idle;
                return;
            }
            [(crew, _)] => *crew,
            _ => match remembered {
                Some(crew) => crew,
                None => {
                    self.ask_choice(game, eligible.iter().map(|(c, _)| *c).collect());
                    return;
                }
            },
        };
        self.dismiss_choice();
        self.start_session(chosen, game, now);
    }

    /// Enters (or stays in) `NeedsChoice`. The event and the state's crew order change only when
    /// the game or the *set* of crews changes, so the state always equals the last event.
    fn ask_choice(&mut self, game: String, crews: Vec<CrewId>) {
        if let SessionState::NeedsChoice {
            game: g, crews: c, ..
        } = &self.state
        {
            let same_set =
                c.iter().collect::<BTreeSet<_>>() == crews.iter().collect::<BTreeSet<_>>();
            if *g == game && same_set {
                return;
            }
        }
        self.events.push(SessionEvent::ChoiceNeeded {
            game: game.clone(),
            crews: crews.clone(),
        });
        self.state = SessionState::NeedsChoice { game, crews };
    }

    /// Leaves `NeedsChoice` (if in it), reporting `ChoiceDismissed`. The state becomes `Idle`
    /// until the caller sets the next one.
    fn dismiss_choice(&mut self) {
        if let SessionState::NeedsChoice { game, .. } = &self.state {
            self.events
                .push(SessionEvent::ChoiceDismissed { game: game.clone() });
            self.state = SessionState::Idle;
        }
    }

    fn start_session(&mut self, crew: CrewId, game: String, now: u64) {
        self.events.push(SessionEvent::Started {
            crew,
            game: game.clone(),
        });
        self.grace_since = None;
        self.seated_since = None;
        self.state = SessionState::Queued {
            crew,
            game: game.clone(),
            overflow: Vec::new(),
        };
        self.refresh(crew, game, now);
    }

    fn end_session(&mut self, crew: CrewId) {
        self.events.push(SessionEvent::Ended { crew });
        self.state = SessionState::Idle;
        self.grace_since = None;
        self.seated_since = None;
    }

    /// Recomputes seats of the session on `crew` (I am committed to it) and emits `Left` /
    /// `Joined` / `Overflow` events for the differences.
    fn refresh(&mut self, crew: CrewId, game: String, now: u64) {
        let (old_others, old_overflow) = match &self.state {
            SessionState::Active {
                crew: c,
                participants,
                overflow,
                ..
            } if *c == crew => (
                participants
                    .iter()
                    .copied()
                    .filter(|d| *d != self.me)
                    .collect::<Vec<_>>(),
                overflow.clone(),
            ),
            SessionState::Queued {
                crew: c, overflow, ..
            } if *c == crew => (Vec::new(), overflow.clone()),
            _ => (Vec::new(), Vec::new()),
        };

        // Only peers committed to this crew compete for seats; everyone ranks the same way.
        let my_online_since = self.online_since.unwrap_or(self.last_now);
        let mut ranked: Vec<RankKey> = self
            .candidates(crew, now)
            .into_iter()
            .filter(|peer| peer.presence.active_crew == Some(crew))
            .map(|peer| {
                rank_key(
                    peer.presence.device,
                    peer.presence.seated_since_ms,
                    peer.presence.online_since_ms,
                )
            })
            .collect();
        ranked.push(rank_key(self.me, self.seated_since, my_online_since));
        ranked.sort_unstable();
        // A device only takes a *free* seat while it plays the session's game: a queued device
        // whose game is closed (grace) must not grab a seat it would later use to bump someone.
        let can_take_seat = self.seated_since.is_some() || self.local_game.is_some();
        let mut seated: Vec<DeviceId> = Vec::new();
        let mut overflow: Vec<DeviceId> = Vec::new();
        for (_, _, device) in ranked {
            if seated.len() < self.config.max_size && (device != self.me || can_take_seat) {
                seated.push(device);
            } else {
                overflow.push(device);
            }
        }
        let i_am_seated = seated.contains(&self.me);

        // My fellow participants keep their join order; newcomers follow in rank order.
        let mut others: Vec<DeviceId> = Vec::new();
        if i_am_seated {
            others.extend(old_others.iter().copied().filter(|d| seated.contains(d)));
            for device in &seated {
                if *device != self.me && !others.contains(device) {
                    others.push(*device);
                }
            }
        }

        for device in &old_others {
            if !others.contains(device) {
                self.events.push(SessionEvent::Left {
                    crew,
                    device: *device,
                });
            }
        }
        for device in &others {
            if !old_others.contains(device) {
                self.events.push(SessionEvent::Joined {
                    crew,
                    device: *device,
                });
            }
        }
        let old_set: BTreeSet<&DeviceId> = old_overflow.iter().collect();
        let new_set: BTreeSet<&DeviceId> = overflow.iter().collect();
        if old_set != new_set {
            self.events.push(SessionEvent::Overflow {
                crew,
                devices: overflow.clone(),
            });
        }

        if i_am_seated {
            self.seated_since.get_or_insert(now);
            let mut participants = Vec::with_capacity(others.len() + 1);
            participants.push(self.me);
            participants.extend(others);
            self.state = SessionState::Active {
                crew,
                game,
                participants,
                overflow,
            };
        } else {
            self.seated_since = None;
            self.state = SessionState::Queued {
                crew,
                game,
                overflow,
            };
        }
    }
}
