//! End-to-end simulation without network: `PresenceClient`s talk JSON to an in-memory fake of
//! the Worker presence contract (`worker/SPEC.md`, "Presence contract (task 10)"):
//! - body validation like `parsePresence` (all five fields, nullable ones as `null`, safe
//!   integers, canonical lowercase uuids, 403 for an active crew the caller is not in);
//! - freshness by Worker receipt time, TTL 90 s;
//! - lexicographic `(online_since_ms, seq)` acceptance while fresh, `409` otherwise (no update);
//! - availability per crew: `active_crew` null or equal to that crew;
//! - one complete snapshot per crew of the caller, sorted by crew id, members by device id.
//!
//! Each device has its own local monotonic clock (an arbitrary offset) and an AppClock; the
//! Worker has its own clock. Checks: agreement and isolation like `duoclip-session`'s
//! `tests/agreement.rs`, and the request rate (≈ 3600 / 30 per hour per active device plus
//! change announcements, ≈ 0 while idle).

use std::collections::{BTreeMap, BTreeSet};

use duoclip_presence::{
    parse_canonical_uuid, HeartbeatFailure, HeartbeatResponse, PersistedState, PresenceClient,
    PresenceConfig, MAX_SAFE_INTEGER,
};
use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{SessionConfig, SessionEvent, SessionState};
use serde_json::{json, Value};
use uuid::Uuid;

const G: &str = "minecraft";
const STEP_MS: u64 = 1_000;
const WORKER_TTL_MS: u64 = 90_000;
const HOUR_MS: u64 = 3_600_000;
/// AppClock around 2026 (ms since the Unix epoch).
const APP_EPOCH: u64 = 1_790_000_000_000;

fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}

fn crew(n: u128) -> CrewId {
    CrewId(Uuid::from_u128(n))
}

/// One `device_presence` row.
#[derive(Clone, Debug)]
struct Row {
    game: Option<String>,
    active_crew: Option<String>,
    seated_since_ms: Option<u64>,
    seq: u64,
    online_since_ms: u64,
    seen_at_ms: u64,
}

struct FakeWorker {
    crews: BTreeMap<CrewId, BTreeSet<DeviceId>>,
    rows: BTreeMap<DeviceId, Row>,
    down: bool,
    stale_409: u64,
}

impl FakeWorker {
    fn fresh(row: &Row, now: u64) -> bool {
        row.seen_at_ms >= now.saturating_sub(WORKER_TTL_MS)
    }

    /// `POST /v1/presence` signed by `device`, at Worker time `now`: `Ok(json)` or `Err(status)`.
    fn post(&mut self, device: DeviceId, body: &str, now: u64) -> Result<String, u16> {
        let Ok(Value::Object(b)) = serde_json::from_str::<Value>(body) else {
            return Err(400);
        };
        let safe = |v: Option<&Value>| v.and_then(Value::as_u64).filter(|n| *n <= MAX_SAFE_INTEGER);
        // parsePresence: every field is required; nullable ones must be explicit nulls.
        let game = match b.get("game") {
            Some(Value::Null) => None,
            Some(Value::String(g)) if duoclip_presence::is_valid_game_id(g) => Some(g.clone()),
            _ => return Err(400),
        };
        let active_crew = match b.get("active_crew") {
            Some(Value::Null) => None,
            Some(Value::String(c)) if parse_canonical_uuid(c).is_some() => Some(c.clone()),
            _ => return Err(400),
        };
        let seated_since_ms = match b.get("seated_since_ms") {
            Some(Value::Null) => None,
            v => Some(safe(v).ok_or(400u16)?),
        };
        let seq = safe(b.get("seq")).ok_or(400u16)?;
        let online_since_ms = safe(b.get("online_since_ms")).ok_or(400u16)?;
        if let Some(c) = &active_crew {
            let id = CrewId(parse_canonical_uuid(c).ok_or(400u16)?);
            if !self.crews.get(&id).is_some_and(|m| m.contains(&device)) {
                return Err(403);
            }
        }
        let mut seen_at_ms = now;
        if let Some(old) = self.rows.get(&device) {
            let newer = (online_since_ms, seq) > (old.online_since_ms, old.seq);
            if Self::fresh(old, now) && !newer {
                self.stale_409 += 1;
                return Err(409);
            }
            seen_at_ms = seen_at_ms.max(old.seen_at_ms);
        }
        self.rows.insert(
            device,
            Row {
                game,
                active_crew,
                seated_since_ms,
                seq,
                online_since_ms,
                seen_at_ms,
            },
        );
        let crews: Vec<Value> = self
            .crews
            .iter()
            .filter(|(_, members)| members.contains(&device))
            .map(|(c, members)| {
                let crew_id = c.to_string();
                let rows: Vec<Value> = members
                    .iter()
                    .filter_map(|d| self.rows.get(d).map(|r| (d, r)))
                    .filter(|(_, r)| Self::fresh(r, now))
                    .filter(|(_, r)| r.active_crew.as_ref().is_none_or(|a| *a == crew_id))
                    .map(|(d, r)| {
                        json!({
                            "device_id": d.to_string(),
                            "display_name": format!("PC {d}"),
                            "game": r.game,
                            "active_crew": r.active_crew,
                            "seated_since_ms": r.seated_since_ms,
                            "seq": r.seq,
                            "online_since_ms": r.online_since_ms,
                            "seen_at_ms": r.seen_at_ms,
                            "expires_at": r.seen_at_ms + WORKER_TTL_MS,
                        })
                    })
                    .collect();
                json!({ "crew_id": crew_id, "members": rows })
            })
            .collect();
        Ok(json!({
            "ok": true,
            "seen_at_ms": seen_at_ms,
            "expires_at": seen_at_ms + WORKER_TTL_MS,
            "heartbeat_interval_ms": 30_000,
            "crews": crews,
        })
        .to_string())
    }
}

struct Device {
    id: DeviceId,
    client: PresenceClient,
    /// Local monotonic clock = world time + offset (arbitrary per device).
    local_offset: u64,
    /// AppClock = world time + APP_EPOCH + skew (small, per device).
    app_skew: u64,
    remembered: BTreeMap<String, CrewId>,
    game: Option<String>,
    requests: u64,
    stale_409: u64,
    ended: u64,
}

struct World {
    crews: BTreeMap<CrewId, BTreeSet<DeviceId>>,
    worker: FakeWorker,
    devices: Vec<Device>,
    /// World time (also the Worker clock).
    t: u64,
}

impl World {
    /// `crews`: crew number -> member device numbers. Every device starts playing `G`.
    fn new(crews: &[(u128, &[u128])], devices: &[u128]) -> Self {
        let crews: BTreeMap<CrewId, BTreeSet<DeviceId>> = crews
            .iter()
            .map(|(c, m)| (crew(*c), m.iter().map(|n| dev(*n)).collect()))
            .collect();
        let mut world = Self {
            worker: FakeWorker {
                crews: crews.clone(),
                rows: BTreeMap::new(),
                down: false,
                stale_409: 0,
            },
            crews,
            devices: Vec::new(),
            t: 10_000,
        };
        for (i, n) in devices.iter().enumerate() {
            let i = u64::try_from(i).unwrap();
            let device = Device {
                id: dev(*n),
                client: world.start_client(
                    dev(*n),
                    APP_EPOCH + world.t + 37 * i,
                    None,
                    &BTreeMap::new(),
                    Some(G),
                ),
                local_offset: 1_000_000 * i + 123,
                app_skew: 37 * i,
                remembered: BTreeMap::new(),
                game: Some(G.to_owned()),
                requests: 0,
                stale_409: 0,
                ended: 0,
            };
            world.devices.push(device);
        }
        world
    }

    /// A newly started app: its crews, remembered choices and game, AppClock `app_clock_ms`.
    fn start_client(
        &self,
        me: DeviceId,
        app_clock_ms: u64,
        persisted: Option<PersistedState>,
        remembered: &BTreeMap<String, CrewId>,
        game: Option<&str>,
    ) -> PresenceClient {
        let mut c = PresenceClient::new(
            me,
            SessionConfig::default(),
            PresenceConfig::default(),
            app_clock_ms,
            persisted,
        )
        .unwrap();
        c.session_mut().set_membership(
            self.crews
                .iter()
                .filter(|(_, m)| m.contains(&me))
                .map(|(c, m)| (*c, m.clone()))
                .collect(),
        );
        c.session_mut().set_remembered_choices(remembered.clone());
        c.session_mut().set_local_game(game.map(str::to_owned));
        c
    }

    fn index(&self, d: DeviceId) -> usize {
        self.devices.iter().position(|x| x.id == d).unwrap()
    }

    fn remember(&mut self, n: u128, c: u128) {
        let i = self.index(dev(n));
        let choices = BTreeMap::from([(G.to_owned(), crew(c))]);
        self.devices[i].remembered = choices.clone();
        self.devices[i]
            .client
            .session_mut()
            .set_remembered_choices(choices);
    }

    fn set_game(&mut self, n: u128, game: Option<&str>) {
        let i = self.index(dev(n));
        self.devices[i].game = game.map(str::to_owned);
        self.devices[i]
            .client
            .session_mut()
            .set_local_game(game.map(str::to_owned));
    }

    /// Restarts the app of device `n`. `keep_persisted`: the app saved `PersistedState`.
    /// `clock_back_ms`: its AppClock now reads that much less than before.
    fn restart(&mut self, n: u128, keep_persisted: bool, clock_back_ms: u64) {
        let i = self.index(dev(n));
        let d = &self.devices[i];
        let persisted = keep_persisted.then(|| d.client.persisted_state());
        let app_clock = APP_EPOCH + self.t + d.app_skew - clock_back_ms;
        let c = self.start_client(d.id, app_clock, persisted, &d.remembered, d.game.as_deref());
        self.devices[i].client = c;
    }

    /// One second of world time: every device ticks, polls, and gets its answer at once.
    fn step(&mut self) -> bool {
        self.t += STEP_MS;
        let mut busy = false;
        for d in &mut self.devices {
            let local = self.t + d.local_offset;
            let mut events = d.client.tick(local);
            if let Some(req) = d.client.poll(local) {
                d.requests += 1;
                let body = req.to_json().expect("requests built by poll are valid");
                let result = if self.worker.down {
                    Err(0)
                } else {
                    self.worker.post(d.id, &body, self.t)
                };
                events.extend(match result {
                    Ok(json) => match HeartbeatResponse::from_json(json.as_bytes()) {
                        Ok(resp) => d.client.on_success(resp, local),
                        Err(_) => d
                            .client
                            .on_failure(HeartbeatFailure::InvalidResponse, local),
                    },
                    Err(0) => d.client.on_failure(HeartbeatFailure::Network, local),
                    Err(409) => {
                        d.stale_409 += 1;
                        d.client.on_failure(HeartbeatFailure::Stale409, local)
                    }
                    Err(code) => d.client.on_failure(HeartbeatFailure::Http(code), local),
                });
            }
            d.ended += events
                .iter()
                .filter(|e| matches!(e, SessionEvent::Ended { .. }))
                .count() as u64;
            busy |= !events.is_empty();
        }
        busy
    }

    fn run(&mut self, ms: u64) {
        for _ in 0..ms / STEP_MS {
            self.step();
        }
    }

    /// Runs until no device reported an event for two full intervals (61 s): everyone then
    /// heartbeated with its current state and read everyone's current rows.
    fn settle(&mut self) {
        let mut quiet = 0;
        for _ in 0..3_600 {
            quiet = if self.step() { 0 } else { quiet + 1 };
            if quiet >= 61 {
                return;
            }
        }
        panic!("never settled");
    }

    fn state(&self, i: usize) -> &SessionState {
        self.devices[i].client.session().state()
    }

    fn crew_of(&self, i: usize) -> Option<CrewId> {
        match self.state(i) {
            SessionState::Active { crew, .. } | SessionState::Queued { crew, .. } => Some(*crew),
            _ => None,
        }
    }

    fn participants(&self, i: usize) -> BTreeSet<DeviceId> {
        match self.state(i) {
            SessionState::Active { participants, .. } => participants.iter().copied().collect(),
            _ => BTreeSet::new(),
        }
    }

    /// The agreement invariants of `duoclip-session`'s `tests/agreement.rs`.
    fn assert_agreement(&self, context: &str) {
        let mut listed_under: BTreeMap<DeviceId, BTreeSet<CrewId>> = BTreeMap::new();
        for (i, d) in self.devices.iter().enumerate() {
            let me = d.id;
            match self.state(i) {
                SessionState::Active {
                    crew,
                    participants,
                    overflow,
                    ..
                } => {
                    assert_eq!(participants.first(), Some(&me), "{context}: self first");
                    let mine = self.participants(i);
                    for p in participants {
                        assert!(self.crews[crew].contains(p), "{context}: {p} not in {crew}");
                        listed_under.entry(*p).or_default().insert(*crew);
                        if *p == me {
                            continue;
                        }
                        let j = self.index(*p);
                        assert_eq!(self.crew_of(j), Some(*crew), "{context}: {me} lists {p}");
                        assert_eq!(mine, self.participants(j), "{context}: {me}/{p} seats");
                        let them = self.devices[j].client.session();
                        assert!(them.check_request(me, *crew).is_ok(), "{context}");
                        assert!(d.client.session().check_request(*p, *crew).is_ok());
                    }
                    assert!(overflow.iter().all(|q| !mine.contains(q)));
                }
                SessionState::Queued { overflow, .. } => {
                    assert!(overflow.contains(&me), "{context}: queued but not in queue");
                    assert!(d.client.session().clip_targets().is_none());
                }
                _ => {}
            }
        }
        for (p, crews) in &listed_under {
            assert_eq!(crews.len(), 1, "{context}: {p} in sessions of {crews:?}");
        }
        for (i, d) in self.devices.iter().enumerate() {
            if !matches!(self.state(i), SessionState::Active { .. }) {
                assert!(
                    !listed_under.contains_key(&d.id),
                    "{context}: {} listed",
                    d.id
                );
            }
        }
    }

    fn session_set(&self, n: u128) -> BTreeSet<u128> {
        self.participants(self.index(dev(n)))
            .iter()
            .map(|d| d.0.as_u128())
            .collect()
    }

    fn requests(&self) -> Vec<u64> {
        self.devices.iter().map(|d| d.requests).collect()
    }
}

/// Crew 1 = {1, 2, 3, 4, 5}, crew 2 = {4, 5, 6, 7, 8, 9}; 4 and 5 are in both.
fn two_crews() -> World {
    World::new(
        &[(1, &[1, 2, 3, 4, 5]), (2, &[4, 5, 6, 7, 8, 9])],
        &[1, 2, 3, 4, 5, 6, 7, 8, 9],
    )
}

#[test]
fn nine_devices_in_two_crews_converge_and_stay_isolated() {
    let mut w = two_crews();
    w.remember(4, 1);
    w.remember(5, 2);
    w.settle();
    w.assert_agreement("settled");
    assert_eq!(w.session_set(1), BTreeSet::from([1, 2, 3, 4]));
    assert_eq!(w.session_set(6), BTreeSet::from([5, 6, 7, 8, 9]));
    for n in 1..=9 {
        let expected = if n <= 4 { crew(1) } else { crew(2) };
        assert_eq!(w.crew_of(w.index(dev(n))), Some(expected), "device {n}");
    }
    assert_eq!(w.worker.stale_409, 0);

    // Stable for an hour: no session ends, agreement holds throughout.
    let ended: u64 = w.devices.iter().map(|d| d.ended).sum();
    for minute in 0..60 {
        w.run(60_000);
        w.assert_agreement(&format!("minute {minute}"));
    }
    assert_eq!(w.devices.iter().map(|d| d.ended).sum::<u64>(), ended);

    // Device 4 switches to crew 2 explicitly: both crews follow.
    let i4 = w.index(dev(4));
    let local = w.t + w.devices[i4].local_offset;
    w.devices[i4].client.choose_crew(crew(2), local).unwrap();
    w.settle();
    w.assert_agreement("after the switch");
    assert_eq!(w.session_set(1), BTreeSet::from([1, 2, 3]));
    assert_eq!(w.session_set(6), BTreeSet::from([4, 5, 6, 7, 8, 9]));
}

#[test]
fn conflicting_remembered_choices_converge() {
    for (first, second) in [(1, 2), (2, 1)] {
        let mut w = World::new(&[(1, &[1, 2]), (2, &[1, 2])], &[1, 2]);
        w.remember(1, first);
        w.remember(2, second);
        w.run(600_000);
        w.settle();
        w.assert_agreement("converged");
        assert_eq!(w.crew_of(0), w.crew_of(1));
        assert_eq!(w.session_set(1).len(), 2);
        let ended: u64 = w.devices.iter().map(|d| d.ended).sum();
        w.run(HOUR_MS);
        assert_eq!(
            w.devices.iter().map(|d| d.ended).sum::<u64>(),
            ended,
            "no more swapping"
        );
    }
}

#[test]
fn the_size_cap_is_agreed_through_the_worker() {
    let ids: Vec<u128> = (1..=9).collect();
    let mut w = World::new(&[(1, &ids)], &ids);
    w.settle();
    w.assert_agreement("9 devices, 8 seats");
    let seated = (0..9)
        .filter(|i| matches!(w.state(*i), SessionState::Active { .. }))
        .count();
    assert_eq!(seated, 8);
    // A seated device leaves the game for good: the queued one gets the seat for everyone.
    let leaver = (0..9)
        .find(|i| matches!(w.state(*i), SessionState::Active { .. }))
        .unwrap();
    let n = w.devices[leaver].id.0.as_u128();
    w.set_game(n, None);
    w.run(120_000);
    w.settle();
    w.assert_agreement("seat freed");
    let seated: Vec<usize> = (0..9)
        .filter(|i| *i != leaver && matches!(w.state(*i), SessionState::Active { .. }))
        .collect();
    assert_eq!(seated.len(), 8);
}

#[test]
fn a_friend_restarting_the_game_does_not_end_the_session() {
    // Only two friends: while device 2's game is closed, crew 1 has no candidate for device 1.
    let mut w = World::new(&[(1, &[1, 2])], &[1, 2]);
    w.settle();
    assert_eq!(w.session_set(1).len(), 2);
    let ended = w.devices[0].ended;
    // Device 2 closes its game for 5 s, at every phase of the 30 s heartbeat cycle. Device 1
    // learns of it up to one interval late, and of the return up to one interval later: with
    // the default 10 s grace its session would end; the client raises the grace to 65 s.
    for _ in 0..30 {
        w.set_game(2, None);
        w.run(5_000);
        w.set_game(2, Some(G));
        w.run(36_000);
    }
    w.settle();
    w.assert_agreement("after restarts");
    assert_eq!(w.session_set(1).len(), 2);
    assert_eq!(w.devices[0].ended, ended, "device 1's session never ended");
}

#[test]
fn restarted_apps_rejoin_without_409() {
    let mut w = World::new(&[(1, &[1, 2, 3])], &[1, 2, 3]);
    w.settle();
    // App 2 restarts with its persisted state while its AppClock is 10 min behind: the run id
    // still grows, so the Worker accepts it at once.
    w.restart(2, true, 600_000);
    w.restart(3, true, 0);
    w.run(5_000);
    assert_eq!(w.worker.stale_409, 0);
    w.settle();
    w.assert_agreement("after restarts");
    assert_eq!(w.session_set(1).len(), 3);
}

#[test]
fn a_lost_persisted_state_with_a_clock_behind_is_locked_out_for_at_most_the_ttl() {
    let mut w = World::new(&[(1, &[1, 2])], &[1, 2]);
    w.settle();
    let before = w.devices[1].requests;
    // App 2 restarts without its persisted state and with its clock 10 min behind: its run id
    // is smaller than the fresh row of the previous run, so the Worker answers 409 until that
    // row expires (90 s). The client retries once per interval (no loop) and then gets in.
    w.restart(2, false, 600_000);
    w.run(85_000);
    // During the lockout: one attempt per interval, each answered 409 (no loop).
    let attempts = w.devices[1].requests - before;
    assert!((2..=3).contains(&attempts), "{attempts} attempts");
    assert_eq!(w.devices[1].stale_409, attempts);
    w.run(65_000);
    let stale = w.devices[1].stale_409;
    assert!((2..=4).contains(&stale), "409s: {stale}");
    w.settle();
    w.assert_agreement("after the lockout");
    assert_eq!(w.session_set(1).len(), 2);
}

#[test]
fn an_outage_expires_peers_locally_and_backs_off() {
    let mut w = World::new(&[(1, &[1, 2, 3])], &[1, 2, 3]);
    w.settle();
    let before = w.requests();
    w.worker.down = true;
    w.run(180_000);
    // 3 minutes down: backoff 1, 2, 4, 8, 16, then every 30 s, about 10 attempts.
    for (b, a) in before.iter().zip(w.requests()) {
        assert!(a - b <= 12, "{} requests during the outage", a - b);
    }
    // Peers expired locally (TTL 90 s), so nobody is anyone's participant.
    for i in 0..3 {
        assert!(w.participants(i).len() <= 1);
    }
    w.worker.down = false;
    w.settle();
    w.assert_agreement("after the outage");
    assert_eq!(w.session_set(1).len(), 3);
}

#[test]
fn request_rate_is_one_per_interval_while_active_and_zero_while_idle() {
    let mut w = two_crews();
    w.remember(4, 1);
    w.remember(5, 2);
    w.settle();
    // One hour active and stable: ≈ 3600 / 30 = 120 requests per device.
    let before = w.requests();
    w.run(HOUR_MS);
    for (b, a) in before.iter().zip(w.requests()) {
        let n = a - b;
        assert!((119..=121).contains(&n), "{n} requests in an active hour");
    }

    // One hour of play with changes: device 1 switches game 6 times (each switch and the
    // resulting commitments are announced at once, on top of the interval).
    let before = w.requests();
    for k in 0..6 {
        w.set_game(1, Some(if k % 2 == 0 { "valorant" } else { G }));
        w.run(600_000);
    }
    let after = w.requests();
    let n1 = after[0] - before[0];
    assert!((120..=145).contains(&n1), "{n1} requests with changes");
    for (b, a) in before.iter().zip(&after).skip(1) {
        assert!((119..=145).contains(&(a - b)), "{} requests", a - b);
    }

    // Everyone closes the game: sessions end after the grace, one idle announcement each, then
    // silence for the rest of the hour.
    for n in 1..=9 {
        w.set_game(n, None);
    }
    let before = w.requests();
    w.run(HOUR_MS);
    for (b, a) in before.iter().zip(w.requests()) {
        assert!(a - b <= 6, "{} requests while going idle", a - b);
    }
    for i in 0..9 {
        assert_eq!(w.state(i), &SessionState::Idle);
    }
    let before = w.requests();
    w.run(HOUR_MS);
    assert_eq!(w.requests(), before, "idle devices send nothing");
}
