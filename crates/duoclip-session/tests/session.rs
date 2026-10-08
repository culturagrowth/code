//! Scenario tests for the automatic session logic (see SPEC.md "Tests").

use std::collections::{BTreeMap, BTreeSet};

use duoclip_proto::{CrewId, DeviceId, RejectReason};
use duoclip_session::{
    Presence, SessionConfig, SessionError, SessionEvent, SessionManager, SessionState,
    MAX_GAME_ID_BYTES, MAX_TRACKED_DEVICES,
};
use uuid::Uuid;

const GAME: &str = "minecraft";
const TTL: u64 = 30_000;
const GRACE: u64 = 10_000;

fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}

fn crew(n: u128) -> CrewId {
    CrewId(Uuid::from_u128(n))
}

/// Builds a membership map from `(crew, member numbers)` pairs.
fn membership(crews: &[(CrewId, &[u128])]) -> BTreeMap<CrewId, BTreeSet<DeviceId>> {
    crews
        .iter()
        .map(|(c, members)| (*c, members.iter().map(|n| dev(*n)).collect()))
        .collect()
}

fn presence(n: u128, seq: u64, game: Option<&str>, active: Option<CrewId>, since: u64) -> Presence {
    Presence {
        device: dev(n),
        seq,
        game: game.map(str::to_owned),
        active_crew: active,
        seated_since_ms: None,
        online_since_ms: since,
    }
}

/// A presence of a friend seated (since `since`) in a session of `c`.
fn seated(n: u128, seq: u64, c: CrewId, since: u64) -> Presence {
    Presence {
        seated_since_ms: Some(since),
        ..presence(n, seq, Some(GAME), Some(c), since)
    }
}

/// A fresh manager for device 1 playing `GAME`, member of the given crews.
fn manager(crews: &[(CrewId, &[u128])]) -> SessionManager {
    let mut m = SessionManager::new(dev(1), SessionConfig::default()).unwrap();
    m.set_membership(membership(crews));
    m.set_local_game(Some(GAME.to_owned()));
    m
}

/// Friend `n` announces itself in `GAME`, free (not committed to any crew).
fn online(m: &mut SessionManager, n: u128, seq: u64, since: u64, now: u64) {
    assert!(m.on_presence(presence(n, seq, Some(GAME), None, since), now));
}

/// Friend `n` announces itself in `GAME`, seated in a session of crew `c`.
fn inside(m: &mut SessionManager, c: CrewId, n: u128, seq: u64, since: u64, now: u64) {
    assert!(m.on_presence(seated(n, seq, c, since), now));
}

fn active_parts(m: &SessionManager) -> (CrewId, Vec<DeviceId>, Vec<DeviceId>) {
    match m.state() {
        SessionState::Active {
            crew,
            participants,
            overflow,
            ..
        } => (*crew, participants.clone(), overflow.clone()),
        other => panic!("expected Active, got {other:?}"),
    }
}

fn started(c: CrewId) -> SessionEvent {
    SessionEvent::Started {
        crew: c,
        game: GAME.into(),
    }
}

fn joined(c: CrewId, n: u128) -> SessionEvent {
    SessionEvent::Joined {
        crew: c,
        device: dev(n),
    }
}

fn left(c: CrewId, n: u128) -> SessionEvent {
    SessionEvent::Left {
        crew: c,
        device: dev(n),
    }
}

#[test]
fn config_validation() {
    assert!(SessionConfig::default().validate().is_ok());
    let bad_ttl = SessionConfig {
        presence_ttl_ms: 999,
        ..SessionConfig::default()
    };
    assert_eq!(bad_ttl.validate(), Err(SessionError::TtlTooShort(999)));
    for size in [0, 1, 9, usize::MAX] {
        let cfg = SessionConfig {
            max_size: size,
            ..SessionConfig::default()
        };
        assert_eq!(cfg.validate(), Err(SessionError::InvalidMaxSize(size)));
        assert!(SessionManager::new(dev(1), cfg).is_err());
    }
    for size in 2..=8 {
        let cfg = SessionConfig {
            max_size: size,
            ..SessionConfig::default()
        };
        assert!(cfg.validate().is_ok());
    }
}

#[test]
fn single_eligible_crew_starts_automatically_on_either_day() {
    let (a, b) = (crew(100), crew(200));
    let members: &[(CrewId, &[u128])] = &[(a, &[1, 2, 3]), (b, &[1, 4, 5])];

    // Day 1: a friend of A is online and free: the session starts, they join once committed.
    let mut m = manager(members);
    online(&mut m, 2, 1, 10, 1_000);
    assert_eq!(m.tick(1_000), vec![started(a)]);
    assert_eq!(active_parts(&m), (a, vec![dev(1)], vec![]));
    assert_eq!(m.my_presence().active_crew, Some(a));
    inside(&mut m, a, 2, 2, 10, 2_000);
    assert_eq!(m.tick(2_000), vec![joined(a, 2)]);
    assert_eq!(active_parts(&m), (a, vec![dev(1), dev(2)], vec![]));

    // Another day: friends of B.
    let mut m = manager(members);
    inside(&mut m, b, 4, 1, 10, 1_000);
    assert_eq!(m.tick(1_000), vec![started(b), joined(b, 4)]);
    assert_eq!(active_parts(&m), (b, vec![dev(1), dev(4)], vec![]));
}

#[test]
fn idle_without_game_or_candidates() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    assert!(m.tick(0).is_empty());
    assert_eq!(m.state(), &SessionState::Idle);
    online(&mut m, 2, 1, 0, 10);
    m.set_local_game(None);
    assert!(m.tick(20).is_empty());
    assert_eq!(m.state(), &SessionState::Idle);
}

#[test]
fn uncommitted_candidates_are_not_participants() {
    // Isolation: a friend that has not committed to my crew (e.g. it is still choosing between
    // two crews) is never a clip target nor accepted as a requester.
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2, 3])]);
    online(&mut m, 2, 1, 10, 0);
    inside(&mut m, a, 3, 1, 10, 0);
    m.tick(0);
    assert_eq!(active_parts(&m).1, vec![dev(1), dev(3)]);
    assert_eq!(m.clip_targets(), Some((a, vec![dev(3)])));
    assert_eq!(m.check_request(dev(2), a), Err(RejectReason::NotInSession));
    assert_eq!(m.check_request(dev(3), a), Ok(()));
    // Device 3 switches to another crew (it is in both): it leaves at once and is busy.
    assert!(m.on_presence(seated(3, 2, b, 10), 100));
    assert_eq!(m.tick(100), vec![left(a, 3)]);
    assert_eq!(m.check_request(dev(3), a), Err(RejectReason::NotInSession));
}

#[test]
fn two_crews_need_a_choice_which_is_remembered() {
    let (a, b) = (crew(100), crew(200));
    let members: &[(CrewId, &[u128])] = &[(a, &[1, 2, 3]), (b, &[1, 4, 5])];
    let mut m = manager(members);
    online(&mut m, 2, 1, 10, 1_000);
    online(&mut m, 4, 1, 10, 1_000);

    let events = m.tick(1_000);
    assert_eq!(
        events,
        vec![SessionEvent::ChoiceNeeded {
            game: GAME.into(),
            crews: vec![a, b]
        }]
    );
    assert_eq!(
        m.state(),
        &SessionState::NeedsChoice {
            game: GAME.into(),
            crews: vec![a, b]
        }
    );
    // Asking again does not repeat the event, and I announce no crew while undecided.
    assert!(m.tick(2_000).is_empty());
    assert_eq!(m.my_presence().active_crew, None);
    online(&mut m, 2, 2, 10, 5_000);
    online(&mut m, 4, 2, 10, 5_000);

    m.choose_crew(b, 5_000).unwrap();
    assert_eq!(active_parts(&m).0, b);
    assert_eq!(m.remembered_choices().get(GAME), Some(&b));
    assert_eq!(
        m.tick(5_000),
        vec![
            SessionEvent::ChoiceDismissed { game: GAME.into() },
            started(b)
        ]
    );
    inside(&mut m, b, 4, 3, 10, 6_000);
    assert_eq!(m.tick(6_000), vec![joined(b, 4)]);

    // Next time (same game, persisted choice): Active(B) without asking.
    let saved = m.remembered_choices().clone();
    let mut m2 = manager(members);
    m2.set_remembered_choices(saved);
    online(&mut m2, 2, 1, 10, 1_000);
    online(&mut m2, 4, 1, 10, 1_000);
    assert_eq!(m2.tick(1_000), vec![started(b)]);
    assert_eq!(active_parts(&m2).0, b);
}

#[test]
fn choice_is_dismissed_when_the_crews_stop_being_eligible() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    online(&mut m, 2, 1, 10, 0);
    online(&mut m, 3, 1, 10, 0);
    m.tick(0);
    assert!(matches!(m.state(), SessionState::NeedsChoice { .. }));
    // Both expire: the prompt must close.
    assert_eq!(
        m.tick(TTL + 1),
        vec![SessionEvent::ChoiceDismissed { game: GAME.into() }]
    );
    assert_eq!(m.state(), &SessionState::Idle);

    // One crew left: dismissed, then rule 2 starts it automatically.
    online(&mut m, 2, 2, 10, 40_000);
    online(&mut m, 3, 2, 10, 40_000);
    m.tick(40_000);
    online(&mut m, 2, 3, 10, 60_000);
    assert_eq!(
        m.tick(40_000 + TTL + 1),
        vec![
            SessionEvent::ChoiceDismissed { game: GAME.into() },
            started(a)
        ]
    );

    // Closing the game while asked also dismisses.
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    online(&mut m, 2, 1, 10, 0);
    online(&mut m, 3, 1, 10, 0);
    m.tick(0);
    m.set_local_game(None);
    assert_eq!(
        m.tick(1),
        vec![SessionEvent::ChoiceDismissed { game: GAME.into() }]
    );
}

#[test]
fn needs_choice_orders_crews_by_candidates_then_id() {
    let (a, b, c) = (crew(100), crew(200), crew(300));
    let mut m = manager(&[(a, &[1, 2, 7]), (b, &[1, 3, 4, 5]), (c, &[1, 6])]);
    for n in 2..=6 {
        online(&mut m, n, 1, 10, 1_000);
    }
    m.tick(1_000);
    let expected = SessionState::NeedsChoice {
        game: GAME.into(),
        crews: vec![b, a, c],
    };
    assert_eq!(m.state(), &expected);
    // Counts change but the set does not: no new event and the order the user saw is kept.
    online(&mut m, 7, 1, 10, 2_000);
    online(&mut m, 6, 2, 10, 2_000);
    assert!(m.tick(2_000).is_empty());
    assert_eq!(m.state(), &expected);
}

#[test]
fn remembered_choice_not_eligible_falls_back_to_rules() {
    let (a, b, c) = (crew(100), crew(200), crew(300));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3]), (c, &[1, 4])]);
    // Remembers C for this game, but nobody from C is online.
    m.set_remembered_choices(BTreeMap::from([(GAME.to_owned(), c)]));
    online(&mut m, 2, 1, 10, 1_000);
    online(&mut m, 3, 1, 10, 1_000);
    m.tick(1_000);
    assert!(matches!(m.state(), SessionState::NeedsChoice { crews, .. } if crews == &vec![a, b]));

    // Remembered crew unknown to the membership: with a single eligible crew, rule 2 applies.
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    m.set_remembered_choices(BTreeMap::from([(GAME.to_owned(), crew(999))]));
    online(&mut m, 2, 1, 10, 1_000);
    m.tick(1_000);
    assert_eq!(active_parts(&m).0, a);
}

#[test]
fn remembered_choice_is_per_game() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    m.set_remembered_choices(BTreeMap::from([("other-game".to_owned(), b)]));
    online(&mut m, 2, 1, 10, 1_000);
    online(&mut m, 3, 1, 10, 1_000);
    m.tick(1_000);
    assert!(matches!(m.state(), SessionState::NeedsChoice { .. }));
}

#[test]
fn choose_crew_errors() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    online(&mut m, 2, 1, 10, 1_000);
    assert_eq!(
        m.choose_crew(crew(999), 1_000),
        Err(SessionError::UnknownCrew(crew(999)))
    );
    assert_eq!(m.choose_crew(b, 1_000), Err(SessionError::NoCandidates(b)));
    // A friend busy with another crew is no candidate either.
    assert!(m.on_presence(seated(3, 1, crew(777), 10), 1_000));
    assert_eq!(m.choose_crew(b, 1_000), Err(SessionError::NoCandidates(b)));
    m.set_local_game(None);
    assert_eq!(m.choose_crew(a, 1_000), Err(SessionError::NoLocalGame));
    assert!(m.remembered_choices().is_empty());
}

#[test]
fn explicit_switch_while_active() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    inside(&mut m, a, 2, 1, 10, 1_000);
    m.tick(1_000);
    online(&mut m, 3, 1, 10, 1_500);
    assert!(m.tick(1_500).is_empty(), "no steal, no ChoiceNeeded");
    m.choose_crew(b, 2_000).unwrap();
    assert_eq!(active_parts(&m).0, b);
    assert_eq!(m.check_request(dev(2), a), Err(RejectReason::NotInSession));
    assert_eq!(
        m.tick(2_000),
        vec![SessionEvent::Ended { crew: a }, started(b)]
    );
    inside(&mut m, b, 3, 2, 10, 2_500);
    assert_eq!(m.tick(2_500), vec![joined(b, 3)]);
    // Choosing the crew I am already in changes nothing (but is remembered).
    m.choose_crew(b, 3_000).unwrap();
    assert!(m.tick(3_000).is_empty());
    assert_eq!(m.remembered_choices().get(GAME), Some(&b));
}

#[test]
fn partial_attendance_join_and_ttl_leave() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3, 4, 5, 6])]);
    inside(&mut m, a, 2, 1, 10, 0);
    inside(&mut m, a, 3, 1, 20, 0);
    m.tick(0);
    assert_eq!(active_parts(&m), (a, vec![dev(1), dev(2), dev(3)], vec![]));

    // A 4th friend joins.
    inside(&mut m, a, 4, 1, 30, 5_000);
    inside(&mut m, a, 2, 2, 10, 5_000);
    inside(&mut m, a, 3, 2, 20, 5_000);
    assert_eq!(m.tick(5_000), vec![joined(a, 4)]);
    assert_eq!(active_parts(&m).1.len(), 4);

    // Device 3 stops announcing; the others keep going. Exactly at the TTL it is still fresh.
    for (seq, t) in [(3, 20_000), (4, 34_000)] {
        inside(&mut m, a, 2, seq, 10, t);
        inside(&mut m, a, 4, seq, 30, t);
    }
    assert!(m.tick(5_000 + TTL).is_empty());
    assert_eq!(m.tick(5_000 + TTL + 1), vec![left(a, 3)]);
    assert_eq!(active_parts(&m), (a, vec![dev(1), dev(2), dev(4)], vec![]));
}

#[test]
fn heartbeat_jitter_around_the_ttl_does_not_flap() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    // Heartbeats arrive right at the TTL edge (late by 0 ms) for a long time: no event at all.
    let mut seq = 1;
    let mut t = 0;
    for _ in 0..50 {
        t += TTL;
        assert!(m.tick(t).is_empty(), "flapped at {t}");
        seq += 1;
        inside(&mut m, a, 2, seq, 10, t);
    }
    // One heartbeat late by 1 ms: Left, then back with the next one, session never ended.
    assert_eq!(m.tick(t + TTL + 1), vec![left(a, 2)]);
    inside(&mut m, a, 2, seq + 1, 10, t + TTL + 2);
    assert_eq!(m.tick(t + TTL + 2), vec![joined(a, 2)]);
}

#[test]
fn different_game_is_not_a_candidate() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3])]);
    assert!(m.on_presence(presence(2, 1, Some("valorant"), Some(a), 10), 100));
    assert!(m.on_presence(presence(3, 1, None, Some(a), 10), 100));
    assert!(m.tick(100).is_empty());
    assert_eq!(m.state(), &SessionState::Idle);
    // The friend switches to my game: they join.
    inside(&mut m, a, 2, 2, 10, 200);
    m.tick(200);
    assert_eq!(active_parts(&m).1, vec![dev(1), dev(2)]);
    // ...and switches away again: they leave.
    assert!(m.on_presence(presence(2, 3, Some("valorant"), Some(a), 10), 300));
    assert_eq!(m.tick(300), vec![left(a, 2)]);
}

#[test]
fn friend_in_session_with_another_crew_is_busy() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 2])]);
    // Device 2 is in both crews but already in a session with B: only B is eligible for it.
    inside(&mut m, b, 2, 1, 10, 100);
    m.tick(100);
    assert_eq!(active_parts(&m), (b, vec![dev(1), dev(2)], vec![]));

    // A friend busy with a crew that is not mine at all is no candidate for A.
    let mut m = manager(&[(a, &[1, 2])]);
    inside(&mut m, crew(777), 2, 1, 10, 100);
    m.tick(100);
    assert_eq!(m.state(), &SessionState::Idle);
}

#[test]
fn stickiness_survives_short_heartbeat_gap_then_ends() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    assert_eq!(active_parts(&m).0, a);

    // Presence expires at TTL + 1; the grace window starts at the first tick that sees it.
    let t0 = TTL + 1;
    assert_eq!(m.tick(t0), vec![left(a, 2)]);
    assert_eq!(active_parts(&m), (a, vec![dev(1)], vec![]));
    assert!(m.tick(t0 + GRACE - 1).is_empty());
    assert_eq!(active_parts(&m).0, a);

    assert_eq!(m.tick(t0 + GRACE), vec![SessionEvent::Ended { crew: a }]);
    assert_eq!(m.state(), &SessionState::Idle);
}

#[test]
fn friend_returning_within_grace_resets_it() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    let t0 = TTL + 1;
    m.tick(t0); // Left, grace starts
    inside(&mut m, a, 2, 2, 10, t0 + GRACE - 1);
    assert_eq!(m.tick(t0 + GRACE - 1), vec![joined(a, 2)]);
    // Going quiet again needs a whole new grace period.
    let t1 = t0 + GRACE - 1 + TTL + 1;
    m.tick(t1);
    assert!(m.tick(t1 + GRACE - 1).is_empty());
    assert_eq!(m.tick(t1 + GRACE), vec![SessionEvent::Ended { crew: a }]);
}

#[test]
fn friend_restarting_the_game_within_grace_keeps_the_session() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    // The friend's game closes (game None) and reopens 5 s later.
    assert!(m.on_presence(presence(2, 2, None, Some(a), 10), 1_000));
    assert_eq!(m.tick(1_000), vec![left(a, 2)]);
    inside(&mut m, a, 2, 3, 10, 6_000);
    assert_eq!(m.tick(6_000), vec![joined(a, 2)]);
    assert_eq!(active_parts(&m).0, a);
}

#[test]
fn my_game_restart_keeps_the_session_but_another_game_ends_it() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    // My game closes and reopens within the grace: same session.
    m.set_local_game(None);
    assert_eq!(m.tick(1_000), vec![left(a, 2)]);
    assert_eq!(active_parts(&m).0, a);
    m.set_local_game(Some(GAME.into()));
    inside(&mut m, a, 2, 2, 10, 5_000);
    assert_eq!(m.tick(5_000), vec![joined(a, 2)]);

    // I switch to another game: the session ends immediately (no silent game change).
    assert!(m.on_presence(presence(2, 3, Some("valorant"), None, 10), 6_000));
    m.set_local_game(Some("valorant".into()));
    // Already ended before the tick: no clip may go to the old session.
    assert_eq!(m.clip_targets().map(|t| t.1), Some(vec![]));
    let events = m.tick(6_000);
    assert_eq!(
        events,
        vec![
            SessionEvent::Ended { crew: a },
            SessionEvent::Started {
                crew: a,
                game: "valorant".into()
            }
        ]
    );
    assert!(matches!(m.state(), SessionState::Active { game, .. } if game == "valorant"));
}

#[test]
fn after_grace_the_session_switches_to_the_other_crew() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    // B becomes eligible while A's only friend vanishes.
    inside(&mut m, b, 3, 1, 10, 20_000);
    inside(&mut m, b, 3, 2, 10, 29_000);
    let t0 = TTL + 1;
    m.tick(t0);
    assert_eq!(active_parts(&m).0, a, "still inside the grace window");
    inside(&mut m, b, 3, 3, 10, t0 + GRACE - 1);
    m.tick(t0 + GRACE - 1);
    assert_eq!(active_parts(&m).0, a);
    inside(&mut m, b, 3, 4, 10, t0 + GRACE);
    assert_eq!(
        m.tick(t0 + GRACE),
        vec![SessionEvent::Ended { crew: a }, started(b), joined(b, 3)]
    );
}

#[test]
fn lowest_id_holds_longer_against_rivals() {
    // Anti-livelock: friend 2 (also in A) is committed to B. I (device 1, lower id) hold A for
    // twice the grace so that 2 can come to A first.
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    inside(&mut m, b, 2, 2, 10, 1_000);
    assert_eq!(m.tick(1_000), vec![left(a, 2)]);
    for t in [1_000 + GRACE, 1_000 + 2 * GRACE - 1] {
        inside(&mut m, b, 2, t, 10, t);
        assert!(m.tick(t).is_empty());
        assert_eq!(active_parts(&m).0, a);
    }
    let t = 1_000 + 2 * GRACE;
    inside(&mut m, b, 2, t, 10, t);
    assert_eq!(
        m.tick(t),
        vec![SessionEvent::Ended { crew: a }, started(b), joined(b, 2)]
    );

    // A higher id uses the normal grace.
    let mut m = SessionManager::new(dev(5), SessionConfig::default()).unwrap();
    m.set_membership(membership(&[(a, &[5, 2]), (b, &[5, 2])]));
    m.set_local_game(Some(GAME.into()));
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    inside(&mut m, b, 2, 2, 10, 1_000);
    m.tick(1_000);
    inside(&mut m, b, 2, 3, 10, 1_000 + GRACE);
    assert_eq!(
        m.tick(1_000 + GRACE),
        vec![SessionEvent::Ended { crew: a }, started(b), joined(b, 2)]
    );
}

#[test]
fn second_eligible_crew_never_steals_an_active_session() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    online(&mut m, 3, 1, 10, 1_000);
    for t in [1_000, 2_000, 3_000] {
        let events = m.tick(t);
        assert!(events.is_empty(), "unexpected events {events:?}");
        assert_eq!(active_parts(&m).0, a);
    }
}

#[test]
fn size_cap_keeps_seated_participants_and_orders_the_queue() {
    let a = crew(100);
    let members: Vec<u128> = (1..=20).collect();
    let mut m = manager(&[(a, &members)]);
    // 11 committed (not yet seated) candidates, devices 2..=12: me + 11 = 12 devices, 8 seats.
    // online_since descends with the id, so the highest ids have priority. My online_since is 0.
    for n in 2..=12u128 {
        let since = 1_000 - u64::try_from(n).unwrap() * 10;
        assert!(m.on_presence(presence(n, 1, Some(GAME), Some(a), since), 0));
    }
    let events = m.tick(0);
    let (_, participants, overflow) = active_parts(&m);
    let expected_participants: Vec<DeviceId> =
        std::iter::once(1).chain((6..=12).rev()).map(dev).collect();
    assert_eq!(participants, expected_participants);
    let expected_overflow: Vec<DeviceId> = (2..=5).rev().map(dev).collect();
    assert_eq!(overflow, expected_overflow);
    assert!(events.contains(&SessionEvent::Overflow {
        crew: a,
        devices: expected_overflow.clone()
    }));
    assert_eq!(m.my_presence().seated_since_ms, Some(0));

    // The seated ones now announce their seat. A newcomer with the best online_since does not
    // bump anyone: it goes to the head of the queue.
    let reannounce = |m: &mut SessionManager, seq: u64, now: u64, skip: u128| {
        for n in 2..=12u128 {
            if n == skip {
                continue;
            }
            let since = 1_000 - u64::try_from(n).unwrap() * 10;
            let mut p = presence(n, seq, Some(GAME), Some(a), since);
            if n >= 6 {
                p.seated_since_ms = Some(0);
            }
            assert!(m.on_presence(p, now));
        }
    };
    reannounce(&mut m, 2, 1_000, 0);
    assert!(m.on_presence(presence(13, 1, Some(GAME), Some(a), 1), 1_000));
    let events = m.tick(1_000);
    let (_, participants2, overflow2) = active_parts(&m);
    assert_eq!(participants2, expected_participants);
    assert_eq!(overflow2, [13, 5, 4, 3, 2].map(dev).to_vec());
    assert_eq!(
        events,
        vec![SessionEvent::Overflow {
            crew: a,
            devices: overflow2.clone()
        }]
    );

    // A participant leaves (stops announcing): the head of the queue takes the seat.
    reannounce(&mut m, 3, TTL + 500, 12);
    assert!(m.on_presence(presence(13, 2, Some(GAME), Some(a), 1), TTL + 500));
    let events = m.tick(TTL + 1_001);
    let (_, participants3, overflow3) = active_parts(&m);
    assert_eq!(participants3.len(), 8);
    assert!(!participants3.contains(&dev(12)));
    assert_eq!(participants3.last(), Some(&dev(13)), "promoted, joins last");
    assert_eq!(overflow3, [5, 4, 3, 2].map(dev).to_vec());
    assert_eq!(
        events,
        vec![
            left(a, 12),
            joined(a, 13),
            SessionEvent::Overflow {
                crew: a,
                devices: overflow3.clone()
            }
        ]
    );
}

#[test]
fn twelve_candidates_give_five_overflow_and_ties_break_by_id() {
    let a = crew(100);
    let members: Vec<u128> = (1..=20).collect();
    let mut m = manager(&[(a, &members)]);
    // Same online_since for everyone (mine is 0, so I rank first): the device id breaks ties.
    for n in (2..=13u128).rev() {
        assert!(m.on_presence(presence(n, 1, Some(GAME), Some(a), 500), 0));
    }
    m.tick(0);
    let (_, participants, overflow) = active_parts(&m);
    assert_eq!(participants, (1..=8).map(dev).collect::<Vec<_>>());
    assert_eq!(overflow, (9..=13).map(dev).collect::<Vec<_>>());
}

#[test]
fn overflow_event_tracks_the_set_including_when_it_empties() {
    let a = crew(100);
    let cfg = SessionConfig {
        max_size: 2,
        ..SessionConfig::default()
    };
    let mut m = SessionManager::new(dev(1), cfg).unwrap();
    m.set_membership(membership(&[(a, &[1, 2, 3])]));
    m.set_local_game(Some(GAME.into()));
    inside(&mut m, a, 2, 1, 10, 0);
    assert!(m.on_presence(presence(3, 1, Some(GAME), Some(a), 20), 0));
    assert_eq!(
        m.tick(0),
        vec![
            started(a),
            joined(a, 2),
            SessionEvent::Overflow {
                crew: a,
                devices: vec![dev(3)]
            }
        ]
    );
    assert!(m.tick(1).is_empty(), "no duplicate on a repeated tick");
    // 3 goes away: the queue empties and that is reported too.
    assert!(m.on_presence(presence(3, 2, Some("other"), None, 20), 2));
    assert_eq!(
        m.tick(2),
        vec![SessionEvent::Overflow {
            crew: a,
            devices: vec![]
        }]
    );
}

#[test]
fn queued_when_the_session_is_full() {
    let a = crew(100);
    let cfg = SessionConfig {
        max_size: 3,
        ..SessionConfig::default()
    };
    let mut m = SessionManager::new(dev(1), cfg).unwrap();
    m.set_membership(membership(&[(a, &[1, 2, 3, 4, 5])]));
    m.set_local_game(Some(GAME.into()));
    // Two friends already seated; 4 and 5 committed but not seated, online before me (my
    // online_since is 1_000): 4 takes the last seat, 5 and I queue in that order.
    inside(&mut m, a, 2, 1, 10, 1_000);
    inside(&mut m, a, 3, 1, 20, 1_000);
    assert!(m.on_presence(presence(4, 1, Some(GAME), Some(a), 5), 1_000));
    assert!(m.on_presence(presence(5, 1, Some(GAME), Some(a), 6), 1_000));
    let events = m.tick(1_000);
    let queue = vec![dev(5), dev(1)];
    assert_eq!(
        events,
        vec![
            started(a),
            SessionEvent::Overflow {
                crew: a,
                devices: queue.clone()
            }
        ]
    );
    assert_eq!(
        m.state(),
        &SessionState::Queued {
            crew: a,
            game: GAME.into(),
            overflow: queue
        }
    );
    let p = m.my_presence();
    assert_eq!((p.active_crew, p.seated_since_ms), (Some(a), None));
    assert_eq!(m.clip_targets(), None);
    assert_eq!(m.check_request(dev(2), a), Err(RejectReason::NotInSession));

    // A seat frees (3 closes the game): 5, head of the queue, takes it; I stay queued.
    inside(&mut m, a, 2, 2, 10, 2_000);
    let mut p4 = seated(4, 2, a, 2_000);
    p4.online_since_ms = 5;
    assert!(m.on_presence(p4, 2_000));
    assert!(m.on_presence(presence(5, 2, Some(GAME), Some(a), 6), 2_000));
    assert!(m.on_presence(presence(3, 2, None, Some(a), 20), 2_000));
    assert_eq!(
        m.tick(2_000),
        vec![SessionEvent::Overflow {
            crew: a,
            devices: vec![dev(1)]
        }]
    );
    // Another seat frees (2 closes the game): my turn.
    assert!(m.on_presence(presence(2, 3, None, Some(a), 10), 3_000));
    assert_eq!(
        m.tick(3_000),
        vec![
            joined(a, 4),
            joined(a, 5),
            SessionEvent::Overflow {
                crew: a,
                devices: vec![]
            }
        ]
    );
    assert_eq!(active_parts(&m), (a, vec![dev(1), dev(4), dev(5)], vec![]));
    assert_eq!(m.my_presence().seated_since_ms, Some(3_000));
}

#[test]
fn smaller_configured_caps_are_respected() {
    let a = crew(100);
    for max_size in [2, 3] {
        let cfg = SessionConfig {
            max_size,
            ..SessionConfig::default()
        };
        let mut m = SessionManager::new(dev(1), cfg).unwrap();
        m.set_membership(membership(&[(a, &[1, 2, 3, 4, 5])]));
        m.set_local_game(Some(GAME.into()));
        for n in 2..=5 {
            let since = 10 * u64::try_from(n).unwrap();
            assert!(m.on_presence(presence(n, 1, Some(GAME), Some(a), since), 0));
        }
        m.tick(0);
        let (_, participants, overflow) = active_parts(&m);
        assert_eq!(participants.len(), max_size);
        assert_eq!(participants.len() + overflow.len(), 5);
        assert_eq!(participants[0], dev(1));
    }
}

#[test]
fn check_request_gate() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2, 3, 4]), (b, &[1, 5])]);
    // Idle: everything is rejected.
    assert_eq!(m.check_request(dev(2), a), Err(RejectReason::NotInSession));

    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    assert_eq!(m.check_request(dev(2), a), Ok(()));
    // Member of the crew but not in the session.
    assert_eq!(m.check_request(dev(3), a), Err(RejectReason::NotInSession));
    // Stranger.
    assert_eq!(m.check_request(dev(99), a), Err(RejectReason::NotInSession));
    // Participant, wrong crew.
    assert_eq!(m.check_request(dev(2), b), Err(RejectReason::NotInSession));
    // A request that claims to come from myself is not a peer request.
    assert_eq!(m.check_request(dev(1), a), Err(RejectReason::NotInSession));

    // NeedsChoice also rejects.
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 5])]);
    online(&mut m, 2, 1, 10, 0);
    online(&mut m, 5, 1, 10, 0);
    m.tick(0);
    assert!(matches!(m.state(), SessionState::NeedsChoice { .. }));
    assert_eq!(m.check_request(dev(2), a), Err(RejectReason::NotInSession));
}

#[test]
fn removing_a_friend_from_the_crew_takes_effect_immediately() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2, 3]), (b, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 0);
    inside(&mut m, a, 3, 1, 10, 0);
    m.tick(0);
    // Device 2 is removed from A (it stays in B): no tick needed to stop sending it clips.
    m.set_membership(membership(&[(a, &[1, 3]), (b, &[1, 2])]));
    assert_eq!(m.check_request(dev(2), a), Err(RejectReason::NotInSession));
    assert_eq!(m.clip_targets(), Some((a, vec![dev(3)])));
    assert_eq!(m.tick(1), vec![left(a, 2)]);
}

#[test]
fn clip_targets_excludes_me_and_is_a_stable_snapshot() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3, 4])]);
    assert_eq!(m.clip_targets(), None);
    inside(&mut m, a, 2, 1, 10, 0);
    inside(&mut m, a, 3, 1, 20, 0);
    m.tick(0);
    let snapshot = m.clip_targets().unwrap();
    assert_eq!(snapshot, (a, vec![dev(2), dev(3)]));
    assert_eq!(m.clip_targets().unwrap(), snapshot, "repeatable");

    // A friend who joins later is not part of the snapshot already taken.
    inside(&mut m, a, 4, 1, 30, 1_000);
    m.tick(1_000);
    assert_eq!(snapshot, (a, vec![dev(2), dev(3)]));
    assert_eq!(m.clip_targets().unwrap(), (a, vec![dev(2), dev(3), dev(4)]));
    assert!(!m.clip_targets().unwrap().1.contains(&dev(1)));
}

#[test]
fn stale_and_duplicate_seq_are_ignored() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    assert!(m.on_presence(seated(2, 5, a, 10), 100));
    assert!(!m.on_presence(seated(2, 5, a, 10), 110), "duplicate");
    assert!(!m.on_presence(presence(2, 4, None, None, 10), 120), "stale");
    m.tick(120);
    assert_eq!(
        active_parts(&m).1,
        vec![dev(1), dev(2)],
        "stale did not clear the game"
    );
    assert!(m.on_presence(seated(2, 6, a, 10), 130));
}

#[test]
fn restarted_peer_is_accepted_and_older_runs_are_ignored() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    // Run 1 of the peer's app (online since 10).
    assert!(m.on_presence(presence(2, 500, Some(GAME), None, 10), 0));
    // The app restarts (online since 5_000, seq back to 1): accepted at once.
    assert!(m.on_presence(presence(2, 1, Some("valorant"), None, 5_000), 6_000));
    // A delayed packet of run 1 (higher seq) must not override the fresh run 2 ...
    assert!(!m.on_presence(presence(2, 499, Some(GAME), None, 10), 6_500));
    m.tick(6_500);
    assert_eq!(m.state(), &SessionState::Idle, "still valorant");
    // ... and within run 2 the usual seq order applies.
    assert!(!m.on_presence(presence(2, 1, Some(GAME), None, 5_000), 7_000));
    assert!(m.on_presence(presence(2, 2, Some(GAME), None, 5_000), 7_000));

    // A peer whose clock reset (online_since went down) gets back in once the old one expired.
    assert!(!m.on_presence(presence(2, 1, Some(GAME), None, 3), 7_000 + TTL));
    assert!(m.on_presence(presence(2, 1, Some(GAME), None, 3), 7_000 + TTL + 1));
}

#[test]
fn non_members_and_my_own_device_are_ignored() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    assert!(!m.on_presence(presence(9, 1, Some(GAME), Some(a), 10), 100));
    assert!(!m.on_presence(presence(1, 1, Some(GAME), Some(a), 10), 100));
    assert_eq!(m.tracked_devices(), 0);
    m.tick(100);
    assert_eq!(m.state(), &SessionState::Idle);

    // A device removed from every crew is forgotten, and a crew I left stops matching.
    assert!(m.on_presence(presence(2, 1, Some(GAME), None, 10), 200));
    assert_eq!(m.tracked_devices(), 1);
    m.set_membership(membership(&[(a, &[1])]));
    assert_eq!(m.tracked_devices(), 0);
}

#[test]
fn time_going_backwards_is_clamped() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    inside(&mut m, a, 2, 1, 10, 50_000);
    m.tick(50_000);
    assert_eq!(active_parts(&m).0, a);
    // Time jumps back: treated as the last time seen, nothing expires or panics.
    assert!(m.tick(1_000).is_empty());
    assert!(m.tick(0).is_empty());
    assert_eq!(active_parts(&m).0, a);
    assert!(m.on_presence(seated(2, 2, a, 10), 5));
    assert!(m.choose_crew(a, 7).is_ok());
    // Extreme values do not overflow.
    m.tick(u64::MAX);
    m.tick(0);
    let mut p = seated(2, u64::MAX, a, u64::MAX);
    p.online_since_ms = u64::MAX;
    assert!(m.on_presence(p, u64::MAX));
    m.tick(u64::MAX);
    assert_eq!(active_parts(&m).0, a);
    // With a huge grace the doubled hold saturates instead of overflowing.
    let cfg = SessionConfig {
        switch_grace_ms: u64::MAX,
        ..SessionConfig::default()
    };
    let mut m = SessionManager::new(dev(1), cfg).unwrap();
    m.set_membership(membership(&[(a, &[1, 2]), (crew(5), &[1, 2])]));
    m.set_local_game(Some(GAME.into()));
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    inside(&mut m, crew(5), 2, 2, 10, 1);
    m.tick(u64::MAX);
    assert_eq!(active_parts(&m).0, a);
}

#[test]
fn oversized_game_ids_are_rejected() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3])]);
    let long = "x".repeat(MAX_GAME_ID_BYTES + 1);
    assert!(!m.on_presence(presence(2, 1, Some(&long), Some(a), 10), 100));
    assert_eq!(m.tracked_devices(), 0);
    // Exactly the limit is fine, and matches my own game of the same value.
    let exact = "y".repeat(MAX_GAME_ID_BYTES);
    m.set_local_game(Some(exact.clone()));
    assert!(m.on_presence(presence(3, 1, Some(&exact), Some(a), 10), 100));
    m.tick(100);
    assert_eq!(active_parts(&m).1, vec![dev(1), dev(3)]);
    // An oversized local game is treated as no game.
    m.set_local_game(Some(long));
    m.tick(110);
    assert_eq!(m.my_presence().game, None);
}

#[test]
fn more_than_256_devices_are_bounded() {
    let a = crew(100);
    let members: Vec<u128> = (1..=400).collect();
    let mut m = manager(&[(a, &members)]);
    for n in 2..=400u128 {
        let t = u64::try_from(n).unwrap();
        assert!(m.on_presence(presence(n, 1, Some(GAME), Some(a), t), t));
    }
    assert_eq!(m.tracked_devices(), MAX_TRACKED_DEVICES);
    m.tick(400);
    let (_, participants, overflow) = active_parts(&m);
    assert_eq!(participants.len(), 8);
    // Oldest seen were evicted: the newest 256 remain, so the first candidates are 145+.
    assert_eq!(participants[1], dev(145));
    assert_eq!(overflow.len(), MAX_TRACKED_DEVICES - 7);
}

#[test]
fn membership_change_removing_the_active_crew_ends_it_without_grace() {
    let (a, b) = (crew(100), crew(200));
    let mut m = manager(&[(a, &[1, 2]), (b, &[1, 3])]);
    inside(&mut m, a, 2, 1, 10, 0);
    m.tick(0);
    online(&mut m, 3, 1, 10, 100);
    m.tick(100);
    m.set_membership(membership(&[(b, &[1, 3])]));
    // Effective before the tick.
    assert_eq!(active_parts(&m).0, b);
    assert_eq!(
        m.tick(200),
        vec![SessionEvent::Ended { crew: a }, started(b)]
    );

    m.set_membership(BTreeMap::new());
    assert_eq!(m.state(), &SessionState::Idle);
    assert_eq!(m.tick(300), vec![SessionEvent::Ended { crew: b }]);
}

#[test]
fn my_presence_reports_game_active_crew_and_increasing_seq() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    m.tick(7_000);
    let p1 = m.my_presence();
    assert_eq!(p1.device, dev(1));
    assert_eq!(p1.seq, 1);
    assert_eq!(p1.game.as_deref(), Some(GAME));
    assert_eq!(p1.active_crew, None);
    assert_eq!(p1.seated_since_ms, None);
    assert_eq!(p1.online_since_ms, 7_000);

    online(&mut m, 2, 1, 10, 8_000);
    m.tick(8_000);
    let p2 = m.my_presence();
    assert_eq!(p2.seq, 2);
    assert_eq!(p2.active_crew, Some(a));
    assert_eq!(p2.seated_since_ms, Some(8_000));
    assert_eq!(p2.online_since_ms, 7_000, "stable across calls");
}
