//! Multi-device simulation: several `SessionManager`s exchange their `my_presence()` every round
//! (all-to-all, like a broadcast transport) and must agree on who is in which session.
//!
//! Agreement invariants (checked after a few quiet rounds):
//! - if A lists B as a participant, B lists A, in the same crew, and both accept each other's
//!   clip requests;
//! - fellow participants have the same participant *set* (everyone agrees on the seats);
//! - a device is a participant under at most one crew, across every device's view;
//! - every participant of a session on crew X is a member of X;
//! - a queued device is nobody's participant and is in the overflow of the seated ones.

use std::collections::{BTreeMap, BTreeSet};

use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{Presence, SessionConfig, SessionEvent, SessionManager, SessionState};
use uuid::Uuid;

const G: &str = "minecraft";
const ROUND_MS: u64 = 1_000;

fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}

fn crew(n: u128) -> CrewId {
    CrewId(Uuid::from_u128(n))
}

struct Sim {
    crews: BTreeMap<CrewId, BTreeSet<DeviceId>>,
    ids: Vec<DeviceId>,
    managers: Vec<SessionManager>,
    games: Vec<Option<String>>,
    max_size: usize,
    now: u64,
}

impl Sim {
    fn new(crews: &[(CrewId, &[u128])], devices: &[u128], max_size: usize) -> Self {
        let crews: BTreeMap<CrewId, BTreeSet<DeviceId>> = crews
            .iter()
            .map(|(c, m)| (*c, m.iter().map(|n| dev(*n)).collect()))
            .collect();
        let mut sim = Self {
            crews,
            ids: devices.iter().map(|n| dev(*n)).collect(),
            managers: Vec::new(),
            games: vec![Some(G.to_owned()); devices.len()],
            max_size,
            now: 1_000,
        };
        sim.managers = (0..devices.len()).map(|i| sim.fresh_manager(i)).collect();
        sim
    }

    /// A newly started app for device `i` (its own crews only, its current game).
    fn fresh_manager(&self, i: usize) -> SessionManager {
        let me = self.ids[i];
        let cfg = SessionConfig {
            max_size: self.max_size,
            ..SessionConfig::default()
        };
        let mut m = SessionManager::new(me, cfg).unwrap();
        m.set_membership(
            self.crews
                .iter()
                .filter(|(_, members)| members.contains(&me))
                .map(|(c, members)| (*c, members.clone()))
                .collect(),
        );
        m.set_local_game(self.games[i].clone());
        m.tick(self.now);
        m
    }

    fn index(&self, d: DeviceId) -> usize {
        self.ids.iter().position(|x| *x == d).unwrap()
    }

    fn set_game(&mut self, i: usize, game: Option<&str>) {
        self.games[i] = game.map(str::to_owned);
        self.managers[i].set_local_game(self.games[i].clone());
    }

    fn restart(&mut self, i: usize) {
        self.managers[i] = self.fresh_manager(i);
    }

    /// One heartbeat: everyone announces, everyone hears everyone, everyone ticks.
    fn round(&mut self) -> Vec<Vec<SessionEvent>> {
        self.now += ROUND_MS;
        let now = self.now;
        let announced: Vec<Presence> = self.managers.iter_mut().map(|m| m.my_presence()).collect();
        for m in &mut self.managers {
            for p in &announced {
                let _ = m.on_presence(p.clone(), now);
            }
        }
        self.managers.iter_mut().map(|m| m.tick(now)).collect()
    }

    fn rounds(&mut self, n: usize) -> usize {
        let mut ended = 0;
        for _ in 0..n {
            for events in self.round() {
                ended += events
                    .iter()
                    .filter(|e| matches!(e, SessionEvent::Ended { .. }))
                    .count();
            }
        }
        ended
    }

    fn state(&self, i: usize) -> &SessionState {
        self.managers[i].state()
    }

    fn crew_of(&self, i: usize) -> Option<CrewId> {
        match self.state(i) {
            SessionState::Active { crew, .. } | SessionState::Queued { crew, .. } => Some(*crew),
            _ => None,
        }
    }

    fn participants(&self, i: usize) -> Vec<DeviceId> {
        match self.state(i) {
            SessionState::Active { participants, .. } => participants.clone(),
            _ => Vec::new(),
        }
    }

    fn assert_agreement(&self, context: &str) {
        let mut listed_under: BTreeMap<DeviceId, BTreeSet<CrewId>> = BTreeMap::new();
        for (i, me) in self.ids.iter().enumerate() {
            match self.state(i) {
                SessionState::Active {
                    crew,
                    participants,
                    overflow,
                    ..
                } => {
                    assert_eq!(participants.first(), Some(me), "{context}: self first");
                    assert!(participants.len() <= self.max_size, "{context}: cap");
                    let mine: BTreeSet<DeviceId> = participants.iter().copied().collect();
                    for d in participants {
                        assert!(
                            self.crews[crew].contains(d),
                            "{context}: {d} listed in {crew} but not a member"
                        );
                        listed_under.entry(*d).or_default().insert(*crew);
                        if d == me {
                            continue;
                        }
                        let j = self.index(*d);
                        let theirs: BTreeSet<DeviceId> = self.participants(j).into_iter().collect();
                        assert_eq!(
                            self.crew_of(j),
                            Some(*crew),
                            "{context}: {me} lists {d} in {crew} but {d} is {:?}",
                            self.state(j)
                        );
                        assert_eq!(
                            mine, theirs,
                            "{context}: {me} and {d} disagree on the seats"
                        );
                        assert!(self.managers[j].check_request(*me, *crew).is_ok());
                        assert!(self.managers[i].check_request(*d, *crew).is_ok());
                    }
                    for q in overflow {
                        assert!(!mine.contains(q), "{context}: {q} seated and queued");
                    }
                }
                SessionState::Queued { crew, overflow, .. } => {
                    assert!(overflow.contains(me), "{context}: queued but not in queue");
                    assert!(self.managers[i].clip_targets().is_none());
                    // Every seated device of my crew playing my game sees me queued.
                    for (j, _) in self.ids.iter().enumerate() {
                        if let SessionState::Active {
                            crew: c, overflow, ..
                        } = self.state(j)
                        {
                            // (A seated device whose own game is closed sees nobody.)
                            if c == crew
                                && self.games[j].is_some()
                                && self.games[j] == self.games[i]
                            {
                                assert!(
                                    overflow.contains(me),
                                    "{context}: {me} queued but not in the queue of {}",
                                    self.ids[j]
                                );
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        for (d, crews) in &listed_under {
            assert_eq!(crews.len(), 1, "{context}: {d} is in sessions of {crews:?}");
        }
        // A queued or idle device is nobody's participant.
        for (i, me) in self.ids.iter().enumerate() {
            if !matches!(self.state(i), SessionState::Active { .. }) {
                assert!(
                    !listed_under.contains_key(me),
                    "{context}: {me} is listed but is {:?}",
                    self.state(i)
                );
            }
        }
    }
}

#[test]
fn two_crews_with_an_overlapping_member_agree() {
    let (a, b) = (crew(100), crew(200));
    // Device 3 is in both crews.
    let mut sim = Sim::new(
        &[(a, &[1, 2, 3]), (b, &[3, 4, 5, 6])],
        &[1, 2, 3, 4, 5, 6],
        8,
    );
    sim.rounds(3);
    sim.assert_agreement("initial");
    // 3 must choose; meanwhile both crews run without it, and nobody targets it.
    assert!(matches!(sim.state(2), SessionState::NeedsChoice { .. }));
    assert_eq!(sim.participants(0), vec![dev(1), dev(2)]);
    assert_eq!(sim.participants(3).len(), 3);

    // 3 picks B: it joins B, A is unaffected.
    sim.managers[2].choose_crew(b, sim.now).unwrap();
    sim.rounds(2);
    sim.assert_agreement("after choosing B");
    assert_eq!(sim.participants(2).len(), 4);
    assert_eq!(sim.crew_of(0), Some(a));
    assert_eq!(sim.participants(0).len(), 2);

    // 3 switches explicitly to A: B drops it, A gains it.
    sim.managers[2].choose_crew(a, sim.now).unwrap();
    sim.rounds(2);
    sim.assert_agreement("after switching to A");
    assert_eq!(sim.participants(0).len(), 3);
    assert_eq!(sim.participants(3).len(), 3);

    // Everyone stays put afterwards.
    assert_eq!(sim.rounds(60), 0, "no session ended while stable");
    sim.assert_agreement("stable");
}

#[test]
fn conflicting_remembered_choices_converge_instead_of_swapping_forever() {
    let (a, b) = (crew(100), crew(200));
    for (first, second) in [(a, b), (b, a)] {
        let mut sim = Sim::new(&[(a, &[1, 2]), (b, &[1, 2])], &[1, 2], 8);
        sim.managers[0].set_remembered_choices(BTreeMap::from([(G.to_owned(), first)]));
        sim.managers[1].set_remembered_choices(BTreeMap::from([(G.to_owned(), second)]));
        sim.rounds(40);
        sim.assert_agreement("converged");
        assert_eq!(sim.crew_of(0), sim.crew_of(1), "same crew");
        assert_eq!(sim.participants(0).len(), 2);
        assert_eq!(sim.rounds(120), 0, "no more swapping");
    }
}

#[test]
fn the_size_cap_is_agreed_by_everyone() {
    let a = crew(100);
    let ids: Vec<u128> = (1..=10).collect();
    let mut sim = Sim::new(&[(a, &ids)], &ids, 8);
    sim.rounds(3);
    sim.assert_agreement("10 devices, 8 seats");
    let seated: Vec<usize> = (0..10)
        .filter(|i| matches!(sim.state(*i), SessionState::Active { .. }))
        .collect();
    let queued: Vec<usize> = (0..10)
        .filter(|i| matches!(sim.state(*i), SessionState::Queued { .. }))
        .collect();
    assert_eq!((seated.len(), queued.len()), (8, 2));

    // A seated device closes its game: the head of the queue is promoted for everyone and
    // the other seated devices keep their seats. (The leaver itself stays `Active` alone during
    // its grace, seeing nobody.)
    let leaver = seated[3];
    let head = queued
        .iter()
        .copied()
        .min_by_key(|q| match sim.state(*q) {
            SessionState::Queued { overflow, .. } => {
                overflow.iter().position(|d| *d == sim.ids[*q]).unwrap()
            }
            _ => unreachable!(),
        })
        .unwrap();
    sim.set_game(leaver, None);
    sim.rounds(3);
    sim.assert_agreement("after a seat freed");
    for i in &seated {
        assert!(matches!(sim.state(*i), SessionState::Active { .. }));
    }
    assert!(matches!(sim.state(head), SessionState::Active { .. }));
    assert_eq!(sim.participants(head).len(), 8);
    assert!(!sim.participants(head).contains(&sim.ids[leaver]));

    // It reopens the game within its grace: it still holds the older seat, so it gets it back
    // and the promoted device returns to the head of the queue, for everyone.
    sim.set_game(leaver, Some(G));
    sim.rounds(3);
    sim.assert_agreement("leaver back within grace");
    assert_eq!(sim.participants(leaver).len(), 8);
    assert!(matches!(sim.state(head), SessionState::Queued { .. }));

    // Closing it for longer than the grace ends its session: back as a newcomer, it queues.
    sim.set_game(leaver, None);
    sim.rounds(25);
    assert_eq!(sim.state(leaver), &SessionState::Idle);
    sim.set_game(leaver, Some(G));
    sim.rounds(3);
    sim.assert_agreement("leaver back after grace");
    assert!(matches!(sim.state(leaver), SessionState::Queued { .. }));
    assert!(matches!(sim.state(head), SessionState::Active { .. }));
}

#[test]
fn max_size_two_pairs_up_consistently() {
    let a = crew(100);
    let ids: Vec<u128> = (1..=5).collect();
    let mut sim = Sim::new(&[(a, &ids)], &ids, 2);
    sim.rounds(3);
    sim.assert_agreement("pairs");
    let seated = (0..5)
        .filter(|i| matches!(sim.state(*i), SessionState::Active { .. }))
        .count();
    assert_eq!(seated, 2);
}

#[test]
fn restarted_apps_rejoin_quickly() {
    let a = crew(100);
    let mut sim = Sim::new(&[(a, &[1, 2, 3])], &[1, 2, 3], 8);
    sim.rounds(3);
    // Both friends restart their apps at the same time (seq back to 1, new online_since).
    sim.restart(1);
    sim.restart(2);
    sim.rounds(3);
    sim.assert_agreement("after restarts");
    assert_eq!(sim.participants(0).len(), 3);
}

/// Tiny seeded xorshift64* PRNG (no external dependency).
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D) % n
    }
}

#[test]
fn random_perturbations_always_settle_into_agreement() {
    let (a, b) = (crew(100), crew(200));
    // Devices 3 and 4 are in both crews.
    let crews: &[(CrewId, &[u128])] = &[(a, &[1, 2, 3, 4]), (b, &[3, 4, 5, 6])];
    let games = [Some(G), Some("valorant"), None];
    for max_size in [3, 8] {
        for seed in 1..=40u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let mut sim = Sim::new(crews, &[1, 2, 3, 4, 5, 6], max_size);
            sim.rounds(2);
            for step in 0..25 {
                let i = usize::try_from(rng.below(6)).unwrap();
                match rng.below(4) {
                    0 => {
                        let g = games[usize::try_from(rng.below(3)).unwrap()];
                        sim.set_game(i, g);
                    }
                    1 => sim.restart(i),
                    _ => {
                        let pick = match sim.state(i) {
                            SessionState::NeedsChoice { crews, .. } => {
                                Some(crews[usize::try_from(rng.below(2)).unwrap() % crews.len()])
                            }
                            _ => None,
                        };
                        if let Some(c) = pick {
                            sim.managers[i].choose_crew(c, sim.now).unwrap();
                        }
                    }
                }
                sim.rounds(3);
                sim.assert_agreement(&format!("max {max_size} seed {seed} step {step}"));
            }
        }
    }
}
