//! `PresenceClient`: run id across restarts, poll policy, failures, response application.

use std::collections::{BTreeMap, BTreeSet};

use duoclip_presence::{
    CrewSnapshot, HeartbeatFailure, HeartbeatRequest, HeartbeatResponse, MemberPresence,
    PersistedState, PresenceClient, PresenceConfig, PresenceError, MAX_SAFE_INTEGER,
};
use duoclip_proto::{CrewId, DeviceId};
use duoclip_session::{SessionConfig, SessionEvent, SessionState};
use uuid::Uuid;

const GAME: &str = "minecraft";
/// AppClock at start (ms since the Unix epoch, around 2026).
const APP: u64 = 1_790_000_000_000;

fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}

fn crew(n: u128) -> CrewId {
    CrewId(Uuid::from_u128(n))
}

/// Device 1, member of crew 100 with devices 2..=4.
fn client_with(session: SessionConfig) -> PresenceClient {
    let mut c = PresenceClient::new(dev(1), session, PresenceConfig::default(), APP, None).unwrap();
    let members: BTreeSet<DeviceId> = (1..=4).map(dev).collect();
    c.session_mut()
        .set_membership(BTreeMap::from([(crew(100), members)]));
    c
}

fn client() -> PresenceClient {
    client_with(SessionConfig::default())
}

/// A member row as the Worker returns it.
fn row(n: u128, seq: u64, active: Option<CrewId>, seated: Option<u64>) -> MemberPresence {
    MemberPresence {
        device_id: dev(n).to_string(),
        display_name: format!("PC {n}"),
        game: Some(GAME.into()),
        active_crew: active.map(|c| c.to_string()),
        seated_since_ms: seated,
        seq,
        online_since_ms: APP - 1_000,
        seen_at_ms: 0,
        expires_at: 90_000,
    }
}

fn response(members: Vec<MemberPresence>) -> HeartbeatResponse {
    HeartbeatResponse {
        ok: true,
        seen_at_ms: 0,
        expires_at: 90_000,
        heartbeat_interval_ms: 30_000,
        crews: vec![CrewSnapshot {
            crew_id: crew(100).to_string(),
            members,
        }],
    }
}

fn empty() -> HeartbeatResponse {
    response(Vec::new())
}

/// Polls at `now` expecting a request, and answers it with `resp` at the same time.
fn exchange(c: &mut PresenceClient, now: u64, resp: HeartbeatResponse) -> HeartbeatRequest {
    let req = c
        .poll(now)
        .unwrap_or_else(|| panic!("expected a request at {now}"));
    c.on_success(resp, now);
    req
}

#[test]
fn online_since_is_monotonic_across_runs() {
    let new = |clock: u64, last: Option<u64>| {
        PresenceClient::new(
            dev(1),
            SessionConfig::default(),
            PresenceConfig::default(),
            clock,
            last.map(|l| PersistedState {
                last_online_since_ms: l,
            }),
        )
    };
    assert_eq!(new(1_000, None).unwrap().online_since_ms(), 1_000);
    assert_eq!(new(5_000, Some(1_000)).unwrap().online_since_ms(), 5_000);
    // The clock is behind the last run (it went backwards): last + 1, never smaller.
    assert_eq!(new(500, Some(1_000)).unwrap().online_since_ms(), 1_001);
    assert_eq!(new(1_000, Some(1_000)).unwrap().online_since_ms(), 1_001);
    // Corrupt persisted values (not safe integers) are ignored.
    assert_eq!(new(5_000, Some(u64::MAX)).unwrap().online_since_ms(), 5_000);
    assert_eq!(
        new(5_000, Some(MAX_SAFE_INTEGER))
            .unwrap()
            .online_since_ms(),
        5_000
    );
    assert_eq!(
        new(5_000, Some(MAX_SAFE_INTEGER - 1))
            .unwrap()
            .online_since_ms(),
        MAX_SAFE_INTEGER
    );
    assert_eq!(
        new(MAX_SAFE_INTEGER + 1, None).unwrap_err(),
        PresenceError::ClockOutOfRange(MAX_SAFE_INTEGER + 1)
    );

    // A chain of restarts with a clock stuck in the past keeps increasing.
    let mut last = None;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let mut c = new(APP, last).unwrap();
        let req = c.poll(0).unwrap();
        assert_eq!(req.online_since_ms, c.online_since_ms());
        // The session ranks itself with the same run id it announces.
        assert_eq!(
            c.session_mut().my_presence().online_since_ms,
            c.online_since_ms()
        );
        ids.push(c.online_since_ms());
        last = Some(c.persisted_state().last_online_since_ms);
    }
    assert_eq!(ids, vec![APP, APP + 1, APP + 2]);
}

#[test]
fn config_validation_clamping_and_grace_floor() {
    let cfg = PresenceConfig::default();
    assert_eq!(cfg.validate(), Ok(()));
    for bad in [
        PresenceConfig {
            interval_ms: 9_999,
            ..cfg.clone()
        },
        PresenceConfig {
            interval_ms: 300_001,
            ..cfg.clone()
        },
        PresenceConfig {
            presence_ttl_ms: 29_999,
            ..cfg.clone()
        },
        PresenceConfig {
            max_backoff_ms: 999,
            ..cfg.clone()
        },
    ] {
        assert!(matches!(
            bad.validate(),
            Err(PresenceError::InvalidConfig(_))
        ));
        assert!(PresenceClient::new(dev(1), SessionConfig::default(), bad, APP, None).is_err());
    }
    let bad_session = SessionConfig {
        max_size: 9,
        ..SessionConfig::default()
    };
    assert!(matches!(
        PresenceClient::new(dev(1), bad_session, cfg.clone(), APP, None),
        Err(PresenceError::Session(_))
    ));
    // The Worker can lower the interval to 10 s but not raise it above TTL / 3 (30 s).
    assert_eq!(cfg.max_interval_ms(), 30_000);
    assert_eq!(cfg.clamp_interval(0), 10_000);
    assert_eq!(cfg.clamp_interval(20_000), 20_000);
    assert_eq!(cfg.clamp_interval(300_000), 30_000);
    assert_eq!(cfg.min_switch_grace_ms(), 65_000);
}

#[test]
fn the_switch_grace_is_raised_to_outlast_the_heartbeat_interval() {
    let mut c = client(); // SessionConfig::default() has a 10 s grace
    c.session_mut().set_local_game(Some(GAME.into()));
    exchange(
        &mut c,
        0,
        response(vec![row(2, 1, Some(crew(100)), Some(5))]),
    );
    assert!(matches!(c.session().state(), SessionState::Active { .. }));
    // The friend disappears from the snapshot (it left): it leaves at once, my session stays
    // for 65 s (a friend restarting its game is only seen up to one interval later).
    c.poll(30_000).unwrap();
    let events = c.on_success(empty(), 30_000);
    assert_eq!(
        events,
        vec![SessionEvent::Left {
            crew: crew(100),
            device: dev(2)
        }]
    );
    assert!(c.tick(94_999).is_empty());
    assert_eq!(
        c.tick(95_000),
        vec![SessionEvent::Ended { crew: crew(100) }]
    );
}

#[test]
fn idle_sends_a_single_announcement_then_nothing() {
    let mut c = client();
    // At start (no game): one announcement, which also replaces a fresh row of a previous run.
    let req = exchange(&mut c, 0, empty());
    assert_eq!(
        (req.game, req.active_crew, req.seated_since_ms, req.seq),
        (None, None, None, 1)
    );
    for t in [1_000, 30_000, 31_000, 600_000, 3_600_000] {
        assert_eq!(c.tick(t), Vec::new());
        assert_eq!(c.poll(t), None, "idle must stay silent at {t}");
    }
    // A game starts: announced at once, then every interval.
    c.session_mut().set_local_game(Some(GAME.into()));
    let req = exchange(&mut c, 3_600_500, empty());
    assert_eq!(req.game.as_deref(), Some(GAME));
    assert_eq!(req.seq, 2);
    assert_eq!(c.poll(3_630_499), None);
    exchange(&mut c, 3_630_500, empty());
    // The game closes and no session runs: one idle announcement, then silence again.
    c.session_mut().set_local_game(None);
    let req = exchange(&mut c, 3_631_500, empty());
    assert_eq!(req.game, None);
    assert_eq!(c.poll(3_700_000), None);
    assert_eq!(c.poll(9_000_000), None);
}

#[test]
fn active_devices_heartbeat_every_interval() {
    let mut c = client();
    c.session_mut().set_local_game(Some(GAME.into()));
    exchange(&mut c, 0, empty());
    assert_eq!(c.poll(29_999), None);
    // The next one is due one interval after the last *response*.
    let req = c.poll(30_000).unwrap();
    c.on_success(empty(), 30_100);
    assert_eq!(req.seq, 2);
    assert_eq!(c.poll(60_099), None);
    assert!(c.poll(60_100).is_some());
}

#[test]
fn at_most_one_request_in_flight_and_seq_strictly_increases() {
    let mut c = client();
    c.session_mut().set_local_game(Some(GAME.into()));
    let first = c.poll(0).unwrap();
    assert!(c.in_flight());
    // Neither the interval nor a change sends a second request while one is in flight.
    c.session_mut().set_local_game(Some("valorant".into()));
    assert_eq!(c.poll(10_000), None);
    assert_eq!(c.poll(120_000), None);
    c.on_failure(HeartbeatFailure::Network, 120_000);
    assert!(!c.in_flight());
    let mut seqs = vec![first.seq];
    let mut now = 121_000;
    for i in 0..10 {
        let req = c.poll(now).unwrap();
        seqs.push(req.seq);
        if i % 3 == 0 {
            c.on_failure(HeartbeatFailure::Stale409, now);
        } else {
            c.on_success(empty(), now);
        }
        now += 30_000;
    }
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");
    assert_eq!(seqs, (1..=11).collect::<Vec<u64>>());
}

#[test]
fn game_crew_and_seat_changes_are_announced_immediately() {
    let mut c = client_with(SessionConfig {
        max_size: 2,
        ..SessionConfig::default()
    });
    c.session_mut().set_local_game(Some(GAME.into()));
    exchange(&mut c, 0, empty());

    // Game change: announced at once (well before the interval).
    c.session_mut().set_local_game(Some("valorant".into()));
    let req = exchange(&mut c, 5_000, empty());
    assert_eq!(req.game.as_deref(), Some("valorant"));
    c.session_mut().set_local_game(Some(GAME.into()));
    exchange(&mut c, 6_000, empty());

    // Two friends are seated in a session of crew 100 (max 2): I commit, but queue.
    let full = || {
        response(vec![
            row(2, 1, Some(crew(100)), Some(5)),
            row(3, 1, Some(crew(100)), Some(6)),
        ])
    };
    c.poll(36_000).unwrap();
    let events = c.on_success(full(), 36_000);
    assert!(events.contains(&SessionEvent::Started {
        crew: crew(100),
        game: GAME.into()
    }));
    assert!(matches!(c.session().state(), SessionState::Queued { .. }));
    // The commitment goes out right away (after the 1 s safety gap), not at the interval.
    assert_eq!(c.poll(36_500), None);
    let req = c.poll(37_000).unwrap();
    assert_eq!(req.active_crew, Some(crew(100).to_string()));
    assert_eq!(req.seated_since_ms, None);
    c.on_success(full(), 37_000);

    // Device 3 leaves the session: its seat is mine, and the seat is announced at once.
    c.poll(67_000).unwrap();
    c.on_success(
        response(vec![
            row(2, 2, Some(crew(100)), Some(5)),
            row(3, 2, None, None),
        ]),
        67_000,
    );
    assert!(matches!(c.session().state(), SessionState::Active { .. }));
    let req = c.poll(68_000).unwrap();
    assert_eq!(req.active_crew, Some(crew(100).to_string()));
    let seated = req.seated_since_ms.expect("seat announced");
    // Seat times are on the AppClock scale: run id + local time elapsed (67 s).
    assert_eq!(seated, APP + 67_000);
}

/// Time from a failure at `now` until `poll` returns the next request.
fn wait_for_request(c: &mut PresenceClient, now: u64) -> u64 {
    let mut t = now + 1;
    while c.poll(t).is_none() {
        t += 1;
        assert!(t < now + 1_000_000, "no retry");
    }
    t
}

#[test]
fn network_errors_and_5xx_back_off_exponentially() {
    // A long outage: 1, 2, 4, 8, 16 s, then capped at 30 s. 5xx counts like a network error.
    let mut c = client();
    c.session_mut().set_local_game(Some(GAME.into()));
    let mut now = 0;
    c.poll(now).unwrap();
    c.on_failure(HeartbeatFailure::Network, now);
    let mut delays = Vec::new();
    for failure in [
        HeartbeatFailure::Http(503),
        HeartbeatFailure::Network,
        HeartbeatFailure::Http(500),
        HeartbeatFailure::Network,
        HeartbeatFailure::Http(599),
        HeartbeatFailure::Network,
        HeartbeatFailure::Network,
    ] {
        let t = wait_for_request(&mut c, now);
        delays.push(t - now);
        c.on_failure(failure, t);
        now = t;
    }
    assert_eq!(
        delays,
        vec![1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
    );
    // A success resets the backoff.
    let t = wait_for_request(&mut c, now);
    c.on_success(empty(), t);
    let next = t + 30_000;
    c.poll(next).unwrap();
    c.on_failure(HeartbeatFailure::Network, next);
    assert_eq!(wait_for_request(&mut c, next) - next, 1_000);
}

#[test]
fn a_409_does_not_loop_and_keeps_the_run_id() {
    let mut c = client();
    c.session_mut().set_local_game(Some(GAME.into()));
    let first = c.poll(0).unwrap();
    c.on_failure(HeartbeatFailure::Stale409, 50);
    // Nothing until the next interval, even if something changes meanwhile.
    c.session_mut().set_local_game(Some("valorant".into()));
    for t in (100..30_050).step_by(500) {
        assert_eq!(c.poll(t), None, "409 must not loop ({t})");
    }
    let retry = c.poll(30_050).unwrap();
    assert_eq!(retry.online_since_ms, first.online_since_ms);
    assert!(retry.seq > first.seq);
    // A 409 reported as a plain HTTP status behaves the same.
    c.on_failure(HeartbeatFailure::Http(409), 30_050);
    assert_eq!(c.poll(60_049), None);
    assert!(c.poll(60_050).is_some());
}

#[test]
fn other_client_errors_and_bad_bodies_wait_one_interval() {
    for failure in [
        HeartbeatFailure::Http(400),
        HeartbeatFailure::Http(403),
        HeartbeatFailure::Http(429),
        HeartbeatFailure::InvalidResponse,
    ] {
        let mut c = client();
        c.session_mut().set_local_game(Some(GAME.into()));
        c.poll(0).unwrap();
        c.on_failure(failure, 0);
        assert_eq!(c.poll(29_999), None, "{failure:?}");
        assert!(c.poll(30_000).is_some(), "{failure:?}");
    }
    // A 200 with ok = false is handled like an unparsable body.
    let mut c = client();
    c.session_mut().set_local_game(Some(GAME.into()));
    c.poll(0).unwrap();
    let mut resp = empty();
    resp.ok = false;
    c.on_success(resp, 0);
    assert!(!c.in_flight());
    assert_eq!(c.poll(29_999), None);
    assert!(c.poll(30_000).is_some());
}

#[test]
fn the_worker_interval_is_adopted_within_bounds() {
    for (asked, used) in [
        (5_000, 10_000),
        (15_000, 15_000),
        (300_000, 30_000),
        (0, 10_000),
    ] {
        let mut c = client();
        c.session_mut().set_local_game(Some(GAME.into()));
        c.poll(0).unwrap();
        let mut resp = empty();
        resp.heartbeat_interval_ms = asked;
        c.on_success(resp, 0);
        assert_eq!(c.interval_ms(), used);
        assert_eq!(c.poll(used - 1), None);
        assert!(c.poll(used).is_some());
    }
}

#[test]
fn responses_update_the_session_and_departures_are_immediate() {
    let mut c = client();
    c.session_mut().set_local_game(Some(GAME.into()));
    c.poll(0).unwrap();
    let mut resp = response(vec![
        row(2, 1, Some(crew(100)), Some(5)),
        row(3, 1, Some(crew(100)), Some(6)),
    ]);
    // My own row and a malformed one are in the snapshot too.
    resp.crews[0].members.push(MemberPresence {
        seq: 9,
        ..row(1, 9, None, None)
    });
    resp.crews[0].members.push(MemberPresence {
        device_id: "NOT-A-UUID".into(),
        ..row(4, 1, None, None)
    });
    let events = c.on_success(resp, 0);
    assert_eq!(events.len(), 3, "{events:?}"); // Started + 2 Joined
    let outcome = c.last_snapshot().unwrap();
    assert_eq!((outcome.accepted, outcome.ignored), (2, 1));
    c.poll(30_000).unwrap();
    let events = c.on_success(response(vec![row(2, 2, Some(crew(100)), Some(5))]), 30_000);
    assert_eq!(
        events,
        vec![SessionEvent::Left {
            crew: crew(100),
            device: dev(3)
        }]
    );
    assert_eq!(c.last_snapshot().unwrap().forgotten, 1);
}

#[test]
fn a_game_id_the_worker_would_refuse_is_announced_as_no_game() {
    let mut c = client();
    c.session_mut().set_local_game(Some("bad\u{202E}id".into()));
    let req = c.poll(0).unwrap();
    assert_eq!(req.game, None);
    assert!(req.to_json().is_ok());
}

#[test]
fn choose_crew_and_tick_use_the_session_time() {
    let (a, b) = (crew(100), crew(200));
    let mut c = PresenceClient::new(
        dev(1),
        SessionConfig::default(),
        PresenceConfig::default(),
        APP,
        None,
    )
    .unwrap();
    c.session_mut().set_membership(BTreeMap::from([
        (a, BTreeSet::from([dev(1), dev(2)])),
        (b, BTreeSet::from([dev(1), dev(3)])),
    ]));
    c.session_mut().set_local_game(Some(GAME.into()));
    // Local clock: an arbitrary monotonic counter (here, 7 s since boot).
    let now = 7_000;
    c.poll(now).unwrap();
    let mut resp = response(vec![row(2, 1, None, None)]);
    resp.crews.push(CrewSnapshot {
        crew_id: b.to_string(),
        members: vec![row(3, 1, None, None)],
    });
    let events = c.on_success(resp, now);
    assert!(matches!(events[0], SessionEvent::ChoiceNeeded { .. }));
    c.choose_crew(b, now + 2_000).unwrap();
    assert_eq!(c.session_time(now + 2_000), APP + 2_000);
    let events = c.tick(now + 2_000);
    assert!(events.contains(&SessionEvent::Started {
        crew: b,
        game: GAME.into()
    }));
    let req = c.poll(now + 2_000).unwrap();
    assert_eq!(req.active_crew, Some(b.to_string()));
    assert_eq!(req.seated_since_ms, Some(APP + 2_000));
}
