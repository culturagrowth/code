//! Randomised robustness test: arbitrary input sequences never panic, invariants always hold,
//! and the outcome is a deterministic function of the inputs.

use std::collections::{BTreeMap, BTreeSet};

use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{
    Presence, SessionConfig, SessionEvent, SessionManager, SessionState, MAX_SESSION_SIZE,
};
use uuid::Uuid;

/// Tiny seeded xorshift64* PRNG (no external dependency).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish value in `0..n` (`n > 0`).
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const GAMES: [&str; 3] = ["g0", "g1", "g2"];

fn dev(n: u64) -> DeviceId {
    DeviceId(Uuid::from_u128(u128::from(n)))
}

fn crew(n: u64) -> CrewId {
    CrewId(Uuid::from_u128(u128::from(n) + 1_000))
}

/// One recorded input, so the exact same sequence can be replayed.
#[derive(Clone, Debug)]
enum Input {
    Membership(BTreeMap<CrewId, BTreeSet<DeviceId>>),
    LocalGame(Option<String>),
    Presence(Presence, u64),
    Snapshot(Vec<Presence>, u64),
    Choose(CrewId, u64),
    Tick(u64),
    MyPresence,
    Remembered(BTreeMap<String, CrewId>),
    Request(DeviceId, CrewId),
}

fn random_game(rng: &mut Rng) -> Option<String> {
    match rng.below(8) {
        0 => None,
        1 => Some("x".repeat(65)), // oversized
        n => Some(GAMES[usize::try_from(n % 3).unwrap()].to_owned()),
    }
}

fn random_membership(rng: &mut Rng) -> BTreeMap<CrewId, BTreeSet<DeviceId>> {
    let mut out = BTreeMap::new();
    for c in 0..rng.below(4) {
        let members = (0..rng.below(14)).map(|_| dev(rng.below(20))).collect();
        out.insert(crew(c), members);
    }
    out
}

fn random_presence(rng: &mut Rng) -> Presence {
    Presence {
        device: dev(rng.below(24)),
        seq: rng.below(6),
        game: random_game(rng),
        active_crew: (rng.below(2) == 0).then(|| crew(rng.below(5))),
        seated_since_ms: (rng.below(2) == 0).then(|| rng.below(1_000)),
        online_since_ms: rng.below(1_000),
    }
}

/// `snapshots`: some ticks are replaced by Worker-style snapshots (`apply_snapshot`). Without it
/// the generated sequence is the original one.
fn random_inputs(seed: u64, steps: usize, snapshots: bool) -> Vec<Input> {
    let mut rng = Rng::new(seed);
    let mut now = 0u64;
    let mut inputs = vec![
        Input::Membership(random_membership(&mut rng)),
        Input::LocalGame(Some(GAMES[0].to_owned())),
    ];
    for _ in 0..steps {
        // Mostly forward, sometimes backwards, sometimes huge.
        now = match rng.below(20) {
            0 => now.saturating_sub(rng.below(40_000)),
            1 => u64::MAX - rng.below(3),
            _ => now.saturating_add(rng.below(6_000)),
        };
        inputs.push(match rng.below(12) {
            0 => Input::Membership(random_membership(&mut rng)),
            1 => Input::LocalGame(random_game(&mut rng)),
            2..=6 => Input::Presence(random_presence(&mut rng), now),
            7 => Input::Choose(crew(rng.below(5)), now),
            8 => Input::MyPresence,
            9 => Input::Remembered(
                (0..rng.below(4))
                    .map(|i| {
                        (
                            GAMES[usize::try_from(i % 3).unwrap()].to_owned(),
                            crew(rng.below(5)),
                        )
                    })
                    .collect(),
            ),
            10 => Input::Request(dev(rng.below(24)), crew(rng.below(5))),
            _ if snapshots => Input::Snapshot(
                (0..rng.below(10))
                    .map(|_| random_presence(&mut rng))
                    .collect(),
                now,
            ),
            _ => Input::Tick(now),
        });
        // Always follow with a tick so the state is re-evaluated often.
        if rng.below(2) == 0 {
            inputs.push(Input::Tick(now));
        }
    }
    inputs
}

/// Everything observable from outside, for comparing two runs.
type Trace = Vec<(
    Vec<SessionEvent>,
    SessionState,
    Option<(CrewId, Vec<DeviceId>)>,
)>;

fn check_invariants(m: &SessionManager, me: DeviceId, max_size: usize) {
    if let SessionState::Active {
        participants,
        overflow,
        ..
    } = m.state()
    {
        assert!(participants.len() <= max_size, "too many participants");
        assert_eq!(participants.first(), Some(&me), "self must be first");
        let set: BTreeSet<_> = participants.iter().collect();
        assert_eq!(set.len(), participants.len(), "duplicate participant");
        assert!(
            overflow.iter().all(|d| !set.contains(d)),
            "device both participant and overflow"
        );
        assert!(!overflow.contains(&me));
        let ov: BTreeSet<_> = overflow.iter().collect();
        assert_eq!(ov.len(), overflow.len(), "duplicate overflow");
    }
    if let SessionState::Queued { overflow, .. } = m.state() {
        assert!(overflow.contains(&me), "queued but not in the queue");
        let ov: BTreeSet<_> = overflow.iter().collect();
        assert_eq!(ov.len(), overflow.len(), "duplicate overflow");
        assert!(m.clip_targets().is_none());
    }
    assert!(m.tracked_devices() <= duoclip_session::MAX_TRACKED_DEVICES);
}

/// The session as reconstructed from the events alone. After every tick it must equal the
/// state: events never contradict the state, and nothing changes without an event.
#[derive(Default)]
struct Model {
    session: Option<CrewId>,
    others: BTreeSet<DeviceId>,
    overflow: BTreeSet<DeviceId>,
    choice: Option<(String, BTreeSet<CrewId>)>,
}

impl Model {
    fn apply(&mut self, events: &[SessionEvent], me: DeviceId) {
        for e in events {
            match e {
                SessionEvent::Started { crew, .. } => {
                    assert_eq!(self.session, None, "Started while in a session");
                    assert_eq!(self.choice, None, "Started while a choice is pending");
                    self.session = Some(*crew);
                }
                SessionEvent::Ended { crew } => {
                    assert_eq!(self.session, Some(*crew), "Ended a session not running");
                    self.session = None;
                    self.others.clear();
                    self.overflow.clear();
                }
                SessionEvent::Joined { crew, device } => {
                    assert_eq!(self.session, Some(*crew));
                    assert_ne!(*device, me);
                    assert!(self.others.insert(*device), "joined twice");
                }
                SessionEvent::Left { crew, device } => {
                    assert_eq!(self.session, Some(*crew));
                    assert!(self.others.remove(device), "left without joining");
                }
                SessionEvent::Overflow { crew, devices } => {
                    assert_eq!(self.session, Some(*crew));
                    let set: BTreeSet<DeviceId> = devices.iter().copied().collect();
                    assert_ne!(set, self.overflow, "Overflow without a change");
                    self.overflow = set;
                }
                SessionEvent::ChoiceNeeded { game, crews } => {
                    assert_eq!(self.session, None, "ChoiceNeeded while in a session");
                    assert!(crews.len() >= 2);
                    self.choice = Some((game.clone(), crews.iter().copied().collect()));
                }
                SessionEvent::ChoiceDismissed { game } => {
                    assert_eq!(
                        self.choice.as_ref().map(|c| &c.0),
                        Some(game),
                        "dismissed a choice never asked"
                    );
                    self.choice = None;
                }
            }
        }
    }

    fn check(&self, state: &SessionState, me: DeviceId) {
        match state {
            SessionState::Idle => {
                assert_eq!(self.session, None);
                assert_eq!(self.choice, None);
            }
            SessionState::NeedsChoice { game, crews } => {
                assert_eq!(self.session, None);
                let set: BTreeSet<CrewId> = crews.iter().copied().collect();
                assert_eq!(self.choice, Some((game.clone(), set)));
            }
            SessionState::Active {
                crew,
                participants,
                overflow,
                ..
            } => {
                assert_eq!(self.session, Some(*crew));
                assert_eq!(self.choice, None);
                let others: BTreeSet<DeviceId> =
                    participants.iter().copied().filter(|d| *d != me).collect();
                assert_eq!(self.others, others, "participants differ from Joined/Left");
                let ov: BTreeSet<DeviceId> = overflow.iter().copied().collect();
                assert_eq!(self.overflow, ov, "overflow differs from Overflow events");
            }
            SessionState::Queued { crew, overflow, .. } => {
                assert_eq!(self.session, Some(*crew));
                assert_eq!(self.choice, None);
                assert!(self.others.is_empty(), "queued but has fellow participants");
                let ov: BTreeSet<DeviceId> = overflow.iter().copied().collect();
                assert_eq!(self.overflow, ov, "overflow differs from Overflow events");
            }
        }
    }
}

fn run(inputs: &[Input], max_size: usize) -> Trace {
    let me = dev(1);
    let cfg = SessionConfig {
        max_size,
        ..SessionConfig::default()
    };
    let mut m = SessionManager::new(me, cfg).unwrap();
    let mut model = Model::default();
    let mut members: BTreeMap<CrewId, BTreeSet<DeviceId>> = BTreeMap::new();
    let mut trace = Trace::new();
    for input in inputs {
        let mut events = Vec::new();
        match input.clone() {
            Input::Membership(x) => {
                members = x.clone();
                m.set_membership(x);
            }
            Input::LocalGame(g) => m.set_local_game(g),
            Input::Presence(p, now) => {
                let _ = m.on_presence(p, now);
            }
            Input::Snapshot(members, now) => {
                let listed: BTreeSet<DeviceId> = members.iter().map(|p| p.device).collect();
                let out = m.apply_snapshot(members, now);
                assert!(
                    m.tracked_devices() <= listed.len(),
                    "kept an unlisted device"
                );
                assert!(out.accepted + out.stale <= listed.len());
            }
            Input::Choose(c, now) => {
                let _ = m.choose_crew(c, now);
            }
            Input::Tick(now) => {
                events = m.tick(now);
                model.apply(&events, me);
                model.check(m.state(), me);
                // Re-evaluating with no new input reports nothing new.
                assert_eq!(
                    m.tick(now),
                    Vec::new(),
                    "duplicate events on a repeated tick"
                );
            }
            Input::MyPresence => {
                let _ = m.my_presence();
            }
            Input::Remembered(r) => m.set_remembered_choices(r),
            Input::Request(from, c) => {
                // Must agree with the participant list.
                let allowed = m.check_request(from, c).is_ok();
                let expected = matches!(
                    m.state(),
                    SessionState::Active { crew, participants, .. }
                        if *crew == c && from != me && participants.contains(&from)
                );
                assert_eq!(allowed, expected);
            }
        }
        check_invariants(&m, me, max_size);
        // Isolation: participants are members of the session's crew under the CURRENT
        // membership, at every moment (not only after a tick).
        if let SessionState::Active {
            crew, participants, ..
        } = m.state()
        {
            for d in participants.iter().filter(|d| **d != me) {
                assert!(members.get(crew).is_some_and(|ms| ms.contains(d)));
            }
        }
        if let Some((_, targets)) = m.clip_targets() {
            assert!(!targets.contains(&me));
        }
        trace.push((events, m.state().clone(), m.clip_targets()));
    }
    trace
}

#[test]
fn random_sequences_hold_invariants_and_are_deterministic() {
    let mut saw_active = false;
    let mut saw_choice = false;
    let mut saw_queued = false;
    for seed in 1..=200u64 {
        for max_size in [2, 5, MAX_SESSION_SIZE] {
            let inputs = random_inputs(seed.wrapping_mul(0x9E37_79B9), 150, false);
            let first = run(&inputs, max_size);
            let second = run(&inputs, max_size);
            assert_eq!(first, second, "seed {seed} not deterministic");
            for (_, state, _) in &first {
                saw_active |= matches!(state, SessionState::Active { .. });
                saw_choice |= matches!(state, SessionState::NeedsChoice { .. });
                saw_queued |= matches!(state, SessionState::Queued { .. });
            }
        }
    }
    // Make sure the generator actually exercises the interesting states.
    assert!(saw_active, "random inputs never produced an active session");
    assert!(saw_choice, "random inputs never produced a NeedsChoice");
    assert!(saw_queued, "random inputs never produced a Queued");
}

#[test]
fn random_sequences_with_worker_snapshots_hold_invariants_and_are_deterministic() {
    let mut saw_active = false;
    for seed in 1..=200u64 {
        for max_size in [2, 5, MAX_SESSION_SIZE] {
            let inputs = random_inputs(seed.wrapping_mul(0x51_7CC1_B727), 150, true);
            let first = run(&inputs, max_size);
            let second = run(&inputs, max_size);
            assert_eq!(first, second, "seed {seed} not deterministic");
            saw_active |= first
                .iter()
                .any(|(_, state, _)| matches!(state, SessionState::Active { .. }));
        }
    }
    assert!(saw_active, "random inputs never produced an active session");
}
