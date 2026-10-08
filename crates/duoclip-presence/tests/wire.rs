//! Wire format of `POST /v1/presence` against the Worker contract (`worker/src/validate.ts`
//! `parsePresence`, `worker/src/routes.ts` `heartbeat`).

use duoclip_presence::{
    is_valid_game_id, parse_canonical_uuid, HeartbeatRequest, HeartbeatResponse, PresenceError,
    MAX_SAFE_INTEGER,
};
use duoclip_proto::{CrewId, DeviceId};
use uuid::Uuid;

const CREW: &str = "abcdef01-2345-6789-abcd-ef0123456789";
const DEV_A: &str = "00000000-0000-0000-0000-00000000000a";
const DEV_B: &str = "00000000-0000-0000-0000-00000000000b";

#[test]
fn request_json_matches_the_worker_exactly() {
    let crew = CrewId(Uuid::from_u128(0xABCD_EF01_2345_6789_ABCD_EF01_2345_6789));
    let req = HeartbeatRequest {
        game: Some("minecraft".into()),
        active_crew: Some(crew.to_string()),
        seated_since_ms: Some(1_790_000_000_123),
        seq: 7,
        online_since_ms: 1_790_000_000_000,
    };
    assert_eq!(
        req.to_json().unwrap(),
        format!(
            r#"{{"game":"minecraft","active_crew":"{CREW}","seated_since_ms":1790000000123,"seq":7,"online_since_ms":1790000000000}}"#
        )
    );
    // Nullable fields are sent as explicit nulls (the Worker requires all five fields).
    let idle = HeartbeatRequest {
        game: None,
        active_crew: None,
        seated_since_ms: None,
        seq: 1,
        online_since_ms: 5,
    };
    assert_eq!(
        idle.to_json().unwrap(),
        r#"{"game":null,"active_crew":null,"seated_since_ms":null,"seq":1,"online_since_ms":5}"#
    );
}

#[test]
fn requests_with_values_the_worker_rejects_are_refused() {
    let base = HeartbeatRequest {
        game: Some("g".into()),
        active_crew: None,
        seated_since_ms: None,
        seq: MAX_SAFE_INTEGER,
        online_since_ms: MAX_SAFE_INTEGER,
    };
    assert!(base.to_json().is_ok(), "2^53 - 1 is still safe");
    for (field, req) in [
        (
            "seq",
            HeartbeatRequest {
                seq: MAX_SAFE_INTEGER + 1,
                ..base.clone()
            },
        ),
        (
            "online_since_ms",
            HeartbeatRequest {
                online_since_ms: u64::MAX,
                ..base.clone()
            },
        ),
        (
            "seated_since_ms",
            HeartbeatRequest {
                seated_since_ms: Some(MAX_SAFE_INTEGER + 1),
                ..base.clone()
            },
        ),
    ] {
        assert_eq!(req.to_json(), Err(PresenceError::UnsafeInteger(field)));
    }
    let upper = HeartbeatRequest {
        active_crew: Some(CREW.to_uppercase()),
        ..base.clone()
    };
    assert_eq!(
        upper.to_json(),
        Err(PresenceError::InvalidRequest("active_crew"))
    );
    let blank = HeartbeatRequest {
        game: Some("  ".into()),
        ..base
    };
    assert_eq!(blank.to_json(), Err(PresenceError::InvalidRequest("game")));
}

#[test]
fn game_ids_follow_the_worker_validation() {
    assert!(is_valid_game_id("minecraft"));
    assert!(is_valid_game_id(&"x".repeat(64)));
    assert!(is_valid_game_id("jogo-ção")); // non-ASCII is fine within 64 bytes
    assert!(!is_valid_game_id(""));
    assert!(!is_valid_game_id(" \t"));
    assert!(!is_valid_game_id("\u{FEFF}"));
    assert!(!is_valid_game_id(&"x".repeat(65)));
    assert!(!is_valid_game_id(&"ç".repeat(33))); // 66 bytes
    assert!(!is_valid_game_id("a\u{0}b"));
    assert!(!is_valid_game_id("a\nb"));
    assert!(!is_valid_game_id("a\u{202E}b")); // bidi override
    assert!(!is_valid_game_id("a\u{2066}b")); // bidi isolate
}

#[test]
fn only_canonical_uuids_are_accepted() {
    assert_eq!(
        parse_canonical_uuid(CREW),
        Some(Uuid::from_u128(0xABCD_EF01_2345_6789_ABCD_EF01_2345_6789))
    );
    for bad in [
        CREW.to_uppercase(),
        CREW.replace('-', ""),
        format!("{{{CREW}}}"),
        format!("urn:uuid:{CREW}"),
        String::new(),
        "zzzzzzzz-0000-0000-0000-000000000000".into(),
    ] {
        assert_eq!(parse_canonical_uuid(&bad), None, "{bad}");
    }
}

fn member(device: &str, extra: &str) -> String {
    format!(
        r#"{{"device_id":"{device}","display_name":"PC","game":"minecraft","active_crew":"{CREW}","seated_since_ms":10,"seq":3,"online_since_ms":5,"seen_at_ms":100,"expires_at":90100{extra}}}"#
    )
}

fn body(crews: &str) -> String {
    format!(
        r#"{{"ok":true,"seen_at_ms":100,"expires_at":90100,"heartbeat_interval_ms":30000,"crews":[{crews}]}}"#
    )
}

#[test]
fn a_worker_response_parses_and_unknown_fields_are_ignored() {
    // Exactly the shape of routes.ts `heartbeat`, plus unknown fields at every level.
    let json = format!(
        r#"{{"ok":true,"seen_at_ms":100,"expires_at":90100,"heartbeat_interval_ms":30000,"future":[1,2],"crews":[{{"crew_id":"{CREW}","note":"x","members":[{},{}]}}]}}"#,
        member(DEV_A, r#","mood":"happy""#),
        member(DEV_B, "").replace(r#""game":"minecraft""#, r#""game":null"#)
    );
    let resp = HeartbeatResponse::from_json(json.as_bytes()).unwrap();
    assert!(resp.ok);
    assert_eq!(resp.heartbeat_interval_ms, 30_000);
    assert_eq!(resp.crews.len(), 1);
    assert_eq!(resp.crews[0].members.len(), 2);
    let presences = resp.presences();
    assert_eq!(presences.len(), 2);
    let a = &presences[0];
    assert_eq!(a.device, DeviceId(Uuid::from_u128(0xa)));
    assert_eq!(a.game.as_deref(), Some("minecraft"));
    assert_eq!(
        a.active_crew,
        Some(CrewId(parse_canonical_uuid(CREW).unwrap()))
    );
    assert_eq!(
        (a.seated_since_ms, a.seq, a.online_since_ms),
        (Some(10), 3, 5)
    );
    assert_eq!(presences[1].game, None);
}

#[test]
fn empty_crews_and_devices_without_crews_parse() {
    let resp = HeartbeatResponse::from_json(
        body(&format!(r#"{{"crew_id":"{CREW}","members":[]}}"#)).as_bytes(),
    )
    .unwrap();
    assert_eq!(resp.crews.len(), 1);
    assert!(resp.presences().is_empty());
    let none = HeartbeatResponse::from_json(body("").as_bytes()).unwrap();
    assert!(none.crews.is_empty());
}

#[test]
fn bad_entries_are_skipped_individually() {
    let good = member(DEV_A, "");
    let bad_members = [
        member(&DEV_B.to_uppercase(), ""), // non-canonical id
        member("not-a-uuid", ""),          // bad id
        member(DEV_B, "").replace(CREW, &CREW.to_uppercase()), // bad active crew
        member(DEV_B, "").replace("minecraft", &"x".repeat(65)), // oversized game
        member(DEV_B, "").replace("minecraft", "a\\u0000b"), // control char
        member(DEV_B, "").replace(r#""seq":3"#, r#""seq":9007199254740992"#), // unsafe integer
        member(DEV_B, "").replace(r#""seq":3"#, r#""seq":-1"#), // negative
        member(DEV_B, "").replace(r#""seq":3"#, r#""seq":1.5"#), // not an integer
        member(DEV_B, "").replace(r#""seq":3"#, r#""seq":"3""#), // wrong type
        member(DEV_B, "").replace(r#","seq":3"#, ""), // missing field
        member(DEV_B, "").replace(
            r#""seen_at_ms":100"#,
            r#""seen_at_ms":18446744073709551615"#,
        ),
        r#""just a string""#.to_owned(),
        "null".to_owned(),
    ];
    for bad in &bad_members {
        let json = body(&format!(
            r#"{{"crew_id":"{CREW}","members":[{bad},{good}]}}"#
        ));
        let resp =
            HeartbeatResponse::from_json(json.as_bytes()).unwrap_or_else(|e| panic!("{bad}: {e}"));
        let presences = resp.presences();
        assert_eq!(presences.len(), 1, "{bad}");
        assert_eq!(presences[0].device, DeviceId(Uuid::from_u128(0xa)));
    }
    // Bad crew entries are skipped, the good crew survives.
    let good_crew = format!(r#"{{"crew_id":"{CREW}","members":[{good}]}}"#);
    for bad_crew in [
        format!(
            r#"{{"crew_id":"{}","members":[{good}]}}"#,
            CREW.to_uppercase()
        ),
        format!(r#"{{"crew_id":"{CREW}","members":{good}}}"#),
        format!(r#"{{"members":[{good}]}}"#),
        "42".to_owned(),
    ] {
        let resp =
            HeartbeatResponse::from_json(body(&format!("{bad_crew},{good_crew}")).as_bytes())
                .unwrap_or_else(|e| panic!("{bad_crew}: {e}"));
        assert_eq!(resp.crews.len(), 1, "{bad_crew}");
        assert_eq!(resp.presences().len(), 1, "{bad_crew}");
    }
}

/// Expected error of a malformed body.
type ErrorCheck = fn(&PresenceError) -> bool;

#[test]
fn a_malformed_top_level_body_is_an_error() {
    let cases: [(&str, ErrorCheck); 9] = [
        ("", |e| matches!(e, PresenceError::MalformedResponse(_))),
        ("[]", |e| matches!(e, PresenceError::MalformedResponse(_))),
        ("not json", |e| {
            matches!(e, PresenceError::MalformedResponse(_))
        }),
        (
            r#"{"ok":true,"seen_at_ms":1,"expires_at":2,"heartbeat_interval_ms":3,"crews":{}}"#,
            |e| matches!(e, PresenceError::MalformedResponse(_)),
        ),
        (
            r#"{"ok":true,"seen_at_ms":1,"expires_at":2,"heartbeat_interval_ms":3}"#,
            |e| matches!(e, PresenceError::MalformedResponse(_)),
        ),
        (
            r#"{"seen_at_ms":1,"expires_at":2,"heartbeat_interval_ms":3,"crews":[]}"#,
            |e| matches!(e, PresenceError::MalformedResponse(_)),
        ),
        (
            r#"{"ok":false,"seen_at_ms":1,"expires_at":2,"heartbeat_interval_ms":3,"crews":[]}"#,
            |e| *e == PresenceError::NotOk,
        ),
        (
            r#"{"ok":true,"seen_at_ms":9007199254740992,"expires_at":2,"heartbeat_interval_ms":3,"crews":[]}"#,
            |e| *e == PresenceError::UnsafeInteger("seen_at_ms"),
        ),
        (
            r#"{"ok":true,"seen_at_ms":1,"expires_at":2,"heartbeat_interval_ms":-3,"crews":[]}"#,
            |e| matches!(e, PresenceError::MalformedResponse(_)),
        ),
    ];
    for (json, expected) in cases {
        let err = HeartbeatResponse::from_json(json.as_bytes()).unwrap_err();
        assert!(expected(&err), "{json}: {err:?}");
    }
    // Deep nesting never overflows the stack: skipped inside an unknown field, refused (by the
    // JSON recursion limit) inside the crews array.
    let nested = format!("{}{}", "[".repeat(10_000), "]".repeat(10_000));
    let deep_unknown = format!(
        r#"{{"ok":true,"seen_at_ms":1,"expires_at":2,"heartbeat_interval_ms":3,"crews":[],"x":{nested}}}"#
    );
    assert!(HeartbeatResponse::from_json(deep_unknown.as_bytes()).is_ok());
    let deep_crew = body(&nested);
    assert!(HeartbeatResponse::from_json(deep_crew.as_bytes()).is_err());
}
