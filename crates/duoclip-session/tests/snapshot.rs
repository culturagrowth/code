//! `SessionManager::apply_snapshot`: Worker-style complete snapshots (see SPEC.md).

use std::collections::{BTreeMap, BTreeSet};

use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{
    Presence, SessionConfig, SessionEvent, SessionManager, SessionState, SnapshotOutcome,
    MAX_GAME_ID_BYTES,
};
use uuid::Uuid;

const GAME: &str = "minecraft";
const TTL: u64 = 90_000;

fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}

fn crew(n: u128) -> CrewId {
    CrewId(Uuid::from_u128(n))
}

fn membership(crews: &[(CrewId, &[u128])]) -> BTreeMap<CrewId, BTreeSet<DeviceId>> {
    crews
        .iter()
        .map(|(c, members)| (*c, members.iter().map(|n| dev(*n)).collect()))
        .collect()
}

/// Device 1 playing `GAME` with the Worker TTL.
fn manager(crews: &[(CrewId, &[u128])]) -> SessionManager {
    let cfg = SessionConfig {
        presence_ttl_ms: TTL,
        ..SessionConfig::default()
    };
    let mut m = SessionManager::new(dev(1), cfg).unwrap();
    m.set_membership(membership(crews));
    m.set_local_game(Some(GAME.to_owned()));
    m
}

/// Friend `n`, run `since`, sequence `seq`, committed to `c` and seated (if `c` is `Some`).
fn friend(n: u128, seq: u64, c: Option<CrewId>, since: u64) -> Presence {
    Presence {
        device: dev(n),
        seq,
        game: Some(GAME.to_owned()),
        active_crew: c,
        seated_since_ms: c.map(|_| since),
        online_since_ms: since,
    }
}

fn participants(m: &SessionManager) -> Vec<DeviceId> {
    match m.state() {
        SessionState::Active { participants, .. } => participants.clone(),
        other => panic!("expected Active, got {other:?}"),
    }
}

fn left(c: CrewId, n: u128) -> SessionEvent {
    SessionEvent::Left {
        crew: c,
        device: dev(n),
    }
}

#[test]
fn departures_are_forgotten_at_once_and_participants_leave() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3])]);
    let out = m.apply_snapshot(
        vec![friend(2, 1, Some(a), 10), friend(3, 1, Some(a), 10)],
        1_000,
    );
    assert_eq!(
        out,
        SnapshotOutcome {
            accepted: 2,
            ..SnapshotOutcome::default()
        }
    );
    m.tick(1_000);
    assert_eq!(participants(&m), vec![dev(1), dev(2), dev(3)]);

    // Device 3 is missing from the next snapshot: forgotten now, not after the 90 s TTL.
    let out = m.apply_snapshot(vec![friend(2, 2, Some(a), 10)], 2_000);
    assert_eq!(
        out,
        SnapshotOutcome {
            accepted: 1,
            forgotten: 1,
            ..SnapshotOutcome::default()
        }
    );
    assert_eq!(m.tracked_devices(), 1);
    assert_eq!(m.tick(2_000), vec![left(a, 3)]);
    assert_eq!(participants(&m), vec![dev(1), dev(2)]);
}

#[test]
fn a_repeated_pair_does_not_refresh_freshness() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    m.apply_snapshot(vec![friend(2, 5, Some(a), 10)], 1_000);
    m.tick(1_000);
    assert_eq!(participants(&m).len(), 2);
    // The Worker keeps returning the same row (the friend stopped heartbeating but its row is
    // still fresh there): the pair is not newer, so the stored receipt time stays 1_000.
    for t in [30_000, 60_000, 90_000] {
        let out = m.apply_snapshot(vec![friend(2, 5, Some(a), 10)], t);
        assert_eq!(
            out,
            SnapshotOutcome {
                stale: 1,
                ..SnapshotOutcome::default()
            }
        );
        m.tick(t);
    }
    assert_eq!(participants(&m).len(), 2, "still fresh at exactly the TTL");
    // Past 1_000 + TTL the friend expires locally even though it is still listed.
    let out = m.apply_snapshot(vec![friend(2, 5, Some(a), 10)], 91_001);
    assert_eq!(out.stale, 1);
    assert_eq!(m.tick(91_001), vec![left(a, 2)]);
    // A newer pair is a real heartbeat: accepted and fresh again.
    let out = m.apply_snapshot(vec![friend(2, 6, Some(a), 10)], 92_000);
    assert_eq!(out.accepted, 1);
    m.tick(92_000);
    assert_eq!(participants(&m).len(), 2);
}

#[test]
fn an_older_pair_is_stale_while_fresh_and_a_new_run_is_accepted() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    m.apply_snapshot(vec![friend(2, 5, Some(a), 10)], 1_000);
    assert_eq!(
        m.apply_snapshot(vec![friend(2, 4, Some(a), 10)], 2_000)
            .stale,
        1
    );
    // A restarted app: greater run id, sequence back to 1.
    assert_eq!(
        m.apply_snapshot(vec![friend(2, 1, Some(a), 20)], 3_000)
            .accepted,
        1
    );
}

#[test]
fn my_own_device_is_ignored() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    // The Worker includes the caller in its own crew snapshot.
    let out = m.apply_snapshot(
        vec![friend(1, 9, Some(a), 10), friend(2, 1, None, 10)],
        1_000,
    );
    assert_eq!(
        out,
        SnapshotOutcome {
            accepted: 1,
            ignored: 1,
            ..SnapshotOutcome::default()
        }
    );
    assert_eq!(m.tracked_devices(), 1);
    m.tick(1_000);
    assert!(matches!(m.state(), SessionState::Active { .. }));
}

#[test]
fn devices_listed_in_another_crews_snapshot_are_kept() {
    let (a, b) = (crew(100), crew(200));
    // Device 3 is in both of my crews and committed to B: the Worker omits it from A's
    // snapshot (busy) but lists it in B's. The union keeps it.
    let mut m = manager(&[(a, &[1, 2, 3]), (b, &[1, 3, 4])]);
    m.set_remembered_choices(BTreeMap::from([(GAME.to_owned(), b)]));
    let snapshot = vec![
        friend(2, 1, None, 10),    // crew A
        friend(3, 1, Some(b), 10), // crew B
        friend(4, 1, Some(b), 10), // crew B
    ];
    m.apply_snapshot(snapshot, 1_000);
    m.tick(1_000);
    assert_eq!(participants(&m), vec![dev(1), dev(3), dev(4)]);

    // A device listed under both crews (free, so available to both) counts once.
    let out = m.apply_snapshot(
        vec![
            friend(2, 2, None, 10),
            friend(3, 2, Some(b), 10),
            friend(4, 2, Some(b), 10),
            friend(2, 2, None, 10),
        ],
        2_000,
    );
    assert_eq!(
        out,
        SnapshotOutcome {
            accepted: 3,
            ..SnapshotOutcome::default()
        }
    );
    assert_eq!(m.tracked_devices(), 3);
    assert_eq!(m.tick(2_000), Vec::new());
}

#[test]
fn duplicates_keep_the_greatest_pair() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    m.apply_snapshot(
        vec![
            friend(2, 7, Some(a), 10),
            friend(2, 3, None, 10),
            friend(2, 1, None, 5),
        ],
        1_000,
    );
    m.tick(1_000);
    // The seq-7 entry won: device 2 is committed to A and seated with me.
    assert_eq!(participants(&m), vec![dev(1), dev(2)]);
}

#[test]
fn an_empty_snapshot_forgets_everyone() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3])]);
    m.apply_snapshot(
        vec![friend(2, 1, Some(a), 10), friend(3, 1, Some(a), 10)],
        1_000,
    );
    m.tick(1_000);
    let out = m.apply_snapshot(Vec::new(), 2_000);
    assert_eq!(
        out,
        SnapshotOutcome {
            forgotten: 2,
            ..SnapshotOutcome::default()
        }
    );
    assert_eq!(m.tracked_devices(), 0);
    assert_eq!(m.tick(2_000), vec![left(a, 2), left(a, 3)]);
    // My crew is no longer eligible: the session survives the grace, then ends.
    assert!(matches!(m.state(), SessionState::Active { .. }));
    assert_eq!(m.tick(12_000), vec![SessionEvent::Ended { crew: a }]);
    assert_eq!(m.state(), &SessionState::Idle);
}

#[test]
fn the_local_ttl_still_expires_peers_when_responses_stop() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    m.apply_snapshot(vec![friend(2, 1, Some(a), 10)], 1_000);
    m.tick(1_000);
    // No snapshot arrives any more (network down).
    assert!(m.tick(1_000 + TTL).is_empty());
    assert_eq!(m.tick(1_001 + TTL), vec![left(a, 2)]);
}

#[test]
fn oversized_games_and_strangers_are_ignored() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2, 3])]);
    m.apply_snapshot(
        vec![friend(2, 1, Some(a), 10), friend(3, 1, Some(a), 10)],
        1_000,
    );
    m.tick(1_000);
    let oversized = Presence {
        game: Some("x".repeat(MAX_GAME_ID_BYTES + 1)),
        ..friend(3, 2, Some(a), 10)
    };
    let stranger = friend(99, 1, None, 10);
    let out = m.apply_snapshot(vec![friend(2, 2, Some(a), 10), oversized, stranger], 2_000);
    // The oversized entry does not count as present: device 3 is forgotten.
    assert_eq!(
        out,
        SnapshotOutcome {
            accepted: 1,
            ignored: 2,
            forgotten: 1,
            ..SnapshotOutcome::default()
        }
    );
    assert_eq!(m.tick(2_000), vec![left(a, 3)]);
}

#[test]
fn snapshots_take_effect_at_the_next_tick_and_never_panic_on_extremes() {
    let a = crew(100);
    let mut m = manager(&[(a, &[1, 2])]);
    let extreme = Presence {
        seq: u64::MAX,
        online_since_ms: u64::MAX,
        seated_since_ms: Some(u64::MAX),
        ..friend(2, 0, Some(a), 0)
    };
    m.apply_snapshot(vec![extreme], u64::MAX);
    assert_eq!(m.state(), &SessionState::Idle, "not before the tick");
    m.tick(u64::MAX);
    assert_eq!(participants(&m).len(), 2);
    // Time going backwards is clamped.
    m.apply_snapshot(Vec::new(), 0);
    assert_eq!(m.tick(0), vec![left(a, 2)]);
}
