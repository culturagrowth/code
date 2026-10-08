//! Integration tests of the public API of duoclip-proto.

use duoclip_proto::*;
use uuid::Uuid;

const SEC: i64 = 1_000_000_000;
/// A realistic hotkey instant (2023-11-14).
const T0: i64 = 1_700_000_000 * SEC;

const KEY_B64_ZEROS: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn dev(n: u128) -> DeviceId {
    DeviceId(Uuid::from_u128(n))
}
fn clip(n: u128) -> ClipId {
    ClipId(Uuid::from_u128(n))
}
fn crew(n: u128) -> CrewId {
    CrewId(Uuid::from_u128(n))
}
fn iv(from: i64, to: i64) -> Interval {
    Interval::new(from, to)
}

fn request() -> ClipRequest {
    ClipRequest::with_defaults(clip(1), dev(2), T0, 5_000_000)
}

fn sha() -> String {
    "ab".repeat(32)
}

fn chunk() -> ChunkRef {
    ChunkRef {
        clip_id: clip(1),
        pov: dev(2),
        quality: Quality::Proxy,
        index: 3,
        is_last: false,
        object_key: object_key(crew(9), clip(1), dev(2), Quality::Proxy, 3),
        range: iv(T0, T0 + 4 * SEC),
        size_bytes: 6 * 1024 * 1024,
        sha256_hex: sha(),
    }
}

fn ack(stage: AckStage) -> ClipAck {
    ClipAck {
        clip_id: clip(1),
        from: dev(3),
        stage,
        coverage: vec![iv(T0 - 30 * SEC, T0 + 12 * SEC)],
        expected_bytes: Some(16_000_000),
        proxy_available: true,
        truncated_by_source_end: false,
    }
}

/// One valid instance of every message variant (plus the ack stages / reject reasons).
fn all_messages() -> Vec<Message> {
    vec![
        Message::Hello {
            device_id: dev(1),
            crew_id: crew(2),
            app_version: "0.1.0-beta+7".into(),
            protocol_version: PROTOCOL_VERSION,
        },
        Message::TimePing {
            seq: u64::MAX,
            t1_local_ns: 123_456_789,
        },
        Message::TimePong {
            seq: 7,
            t1_local_ns: -5,
            t2_local_ns: 10,
            t3_local_ns: 10,
        },
        Message::ClipRequest(request()),
        Message::ClipAck(ack(AckStage::Pinned)),
        Message::ClipAck(ack(AckStage::Ready)),
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::NotInSession,
        })),
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::NoSource,
        })),
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::MemoryBudget,
        })),
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::TooManyActiveClips,
        })),
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::InvalidRequest,
        })),
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::Other("disk on fire \u{1F525}".into()),
        })),
        Message::ClipAck(ClipAck {
            coverage: vec![],
            expected_bytes: None,
            truncated_by_source_end: true,
            ..ack(AckStage::Pinned)
        }),
        Message::ClipExtend {
            clip_id: clip(1),
            requester: dev(2),
            seq: 4,
            new_hotkey_utc_ns: T0 + 3 * SEC,
        },
        Message::RangeRequest {
            clip_id: clip(1),
            pov: dev(3),
            quality: Quality::Full,
            range: iv(T0, T0 + 10 * SEC),
        },
        Message::ChunkAvailable(chunk()),
        Message::ChunkAvailable(ChunkRef {
            is_last: true,
            quality: Quality::Full,
            ..chunk()
        }),
        Message::ClipKey {
            clip_id: clip(1),
            key_b64: KEY_B64_ZEROS.into(),
        },
        Message::ClipDelete { clip_id: clip(1) },
        Message::Bye,
    ]
}

fn assert_invalid(msg: &Message) {
    assert!(
        matches!(msg.validate(), Err(ProtoError::Invalid(_))),
        "expected Invalid for {msg:?}, got {:?}",
        msg.validate()
    );
    assert!(
        matches!(encode(msg), Err(ProtoError::Invalid(_))),
        "encode must reject {msg:?}"
    );
}

fn assert_valid(msg: &Message) {
    msg.validate()
        .unwrap_or_else(|e| panic!("expected valid {msg:?}: {e}"));
    let bytes = encode(msg).expect("encode");
    assert_eq!(&decode(&bytes).expect("decode"), msg);
}

/// Encodes `msg`, applies `edit` to the JSON, and returns the new bytes.
fn tweak(msg: &Message, edit: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let mut v: serde_json::Value = serde_json::from_slice(&encode(msg).unwrap()).unwrap();
    edit(&mut v);
    serde_json::to_vec(&v).unwrap()
}

// ---------------------------------------------------------------------------------------
// Round trips and wire format
// ---------------------------------------------------------------------------------------

#[test]
fn every_variant_round_trips() {
    for msg in all_messages() {
        let bytes = encode(&msg).unwrap_or_else(|e| panic!("encode {msg:?}: {e}"));
        assert!(bytes.len() <= MAX_MESSAGE_BYTES);
        let back = decode(&bytes).unwrap_or_else(|e| panic!("decode {msg:?}: {e}"));
        assert_eq!(back, msg);
        // And the re-encoding is byte-identical (deterministic encoding).
        assert_eq!(encode(&back).unwrap(), bytes);
    }
}

#[test]
fn every_message_type_is_covered_by_the_round_trip_list() {
    let mut names: Vec<&str> = all_messages().iter().map(Message::type_name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names,
        [
            "bye",
            "chunk_available",
            "clip_ack",
            "clip_delete",
            "clip_extend",
            "clip_key",
            "clip_request",
            "hello",
            "range_request",
            "time_ping",
            "time_pong"
        ]
    );
}

#[test]
fn wire_format_is_stable() {
    let bye = encode(&Message::Bye).unwrap();
    assert_eq!(bye, br#"{"v":1,"msg":{"type":"bye"}}"#);

    let del = encode(&Message::ClipDelete { clip_id: clip(1) }).unwrap();
    assert_eq!(
        String::from_utf8(del).unwrap(),
        r#"{"v":1,"msg":{"type":"clip_delete","clip_id":"00000000-0000-0000-0000-000000000001"}}"#
    );

    let ping = encode(&Message::TimePing {
        seq: 3,
        t1_local_ns: 42,
    })
    .unwrap();
    assert_eq!(
        String::from_utf8(ping).unwrap(),
        r#"{"v":1,"msg":{"type":"time_ping","seq":3,"t1_local_ns":42}}"#
    );

    let nack = encode(&Message::ClipAck(ClipAck {
        coverage: vec![],
        expected_bytes: None,
        proxy_available: false,
        truncated_by_source_end: false,
        ..ack(AckStage::Rejected {
            reason: RejectReason::NoSource,
        })
    }))
    .unwrap();
    assert_eq!(
        String::from_utf8(nack).unwrap(),
        concat!(
            r#"{"v":1,"msg":{"type":"clip_ack","clip_id":"00000000-0000-0000-0000-000000000001","#,
            r#""from":"00000000-0000-0000-0000-000000000003","#,
            r#""stage":{"rejected":{"reason":"no_source"}},"coverage":[],"#,
            r#""expected_bytes":null,"proxy_available":false,"truncated_by_source_end":false}}"#
        )
    );

    let range = encode(&Message::RangeRequest {
        clip_id: clip(1),
        pov: dev(2),
        quality: Quality::Full,
        range: iv(10, 20),
    })
    .unwrap();
    assert_eq!(
        String::from_utf8(range).unwrap(),
        concat!(
            r#"{"v":1,"msg":{"type":"range_request","clip_id":"00000000-0000-0000-0000-000000000001","#,
            r#""pov":"00000000-0000-0000-0000-000000000002","quality":"full","#,
            r#""range":{"from_utc_ns":10,"to_utc_ns":20}}}"#
        )
    );
}

#[test]
fn clip_request_decodes_from_hand_written_json() {
    let json = r#"{"v":1,"msg":{"type":"clip_request",
        "clip_id":"00000000-0000-0000-0000-000000000001",
        "requester":"00000000-0000-0000-0000-000000000002",
        "seq":0,"hotkey_utc_ns":1700000000000000000,
        "pre_ms":30000,"post_ms":10000,"margin_pre_ms":2000,"margin_post_ms":2000,
        "max_len_ms":180000,"sync_uncertainty_ns":5000000,"requester_no_source":false}}"#;
    let msg = decode(json.as_bytes()).unwrap();
    assert_eq!(msg, Message::ClipRequest(request()));
}

#[test]
fn ids_accept_only_canonical_lowercase_hyphenated_uuid_text() {
    let delete = |id: &str| {
        format!(r#"{{"v":1,"msg":{{"type":"clip_delete","clip_id":"{id}"}}}}"#).into_bytes()
    };
    let canonical = "abcdef01-0000-0000-0000-000000000001";
    match decode(&delete(canonical)).unwrap() {
        Message::ClipDelete { clip_id } => assert_eq!(clip_id.to_string(), canonical),
        other => panic!("unexpected {other:?}"),
    }
    for bad in [
        // The `uuid` crate would accept all of these spellings; the protocol must not, so that
        // one identifier has exactly one wire form.
        "ABCDEF01-0000-0000-0000-000000000001",
        "abcdef01-0000-0000-0000-00000000000A",
        "abcdef01000000000000000000000001",
        "{abcdef01-0000-0000-0000-000000000001}",
        "urn:uuid:abcdef01-0000-0000-0000-000000000001",
        " abcdef01-0000-0000-0000-000000000001",
        "abcdef01-0000-0000-0000-000000000001 ",
        "abcdef01-0000-0000-0000-000000000001\\n",
        "abcdef01-0000-0000-0000-00000000000",
        "abcdef01-0000-0000-0000-0000000000011",
        "abcdef010-000-0000-0000-000000000001",
        "abcdef01_0000_0000_0000_000000000001",
        "abcdef01-0000-0000-0000-00000000000g",
        "abcdef01-0000-0000-0000-00000000000\\u00e9",
        "",
    ] {
        assert!(
            matches!(decode(&delete(bad)), Err(ProtoError::Json(_))),
            "must reject {bad:?}"
        );
    }
    // Same rule on every id-typed field and on the bare id types.
    let hello = r#"{"v":1,"msg":{"type":"hello","device_id":"00000000-0000-0000-0000-00000000000A","crew_id":"00000000-0000-0000-0000-000000000001","app_version":"1","protocol_version":1}}"#;
    assert!(matches!(decode(hello.as_bytes()), Err(ProtoError::Json(_))));
    for text in [
        r#""00000000-0000-0000-0000-00000000000A""#,
        r#""{00000000-0000-0000-0000-00000000000a}""#,
        "12345",
        "null",
        "[]",
    ] {
        assert!(serde_json::from_str::<ClipId>(text).is_err(), "{text}");
        assert!(serde_json::from_str::<DeviceId>(text).is_err(), "{text}");
        assert!(serde_json::from_str::<CrewId>(text).is_err(), "{text}");
    }
    let id: CrewId = serde_json::from_str(r#""00000000-0000-0000-0000-0000000000ab""#).unwrap();
    assert_eq!(id, crew(0xab));
    // The id round-trips through its own serializer.
    assert_eq!(
        serde_json::from_str::<CrewId>(&serde_json::to_string(&id).unwrap()).unwrap(),
        id
    );
}

#[test]
fn id_helpers_work() {
    let a = ClipId::new_random();
    let b = ClipId::new_random();
    assert_ne!(a, b);
    assert_eq!(a.0.get_version_num(), 4);
    assert_eq!(
        DeviceId::from(Uuid::from_u128(255)).to_string(),
        "00000000-0000-0000-0000-0000000000ff"
    );
    assert_eq!(Quality::Proxy.as_str(), "proxy");
    assert_eq!(Quality::Full.to_string(), "full");
}

// ---------------------------------------------------------------------------------------
// Interval
// ---------------------------------------------------------------------------------------

#[test]
fn interval_rules() {
    assert!(iv(1, 2).validate().is_ok());
    assert!(iv(T0, T0 + 600 * SEC).validate().is_ok(), "exactly 10 min");
    assert!(
        iv(T0, T0 + 600 * SEC + 1).validate().is_err(),
        "10 min + 1ns"
    );
    assert!(iv(5, 5).validate().is_err(), "empty");
    assert!(iv(6, 5).validate().is_err(), "reversed");
    assert!(iv(0, 5).validate().is_err(), "from == 0");
    assert!(iv(-5, 5).validate().is_err(), "from < 0");
    assert!(iv(5, 0).validate().is_err(), "to == 0");
    assert!(
        iv(i64::MIN, i64::MAX).validate().is_err(),
        "extremes must not overflow"
    );
    assert!(iv(1, i64::MAX).validate().is_err());
}

#[test]
fn interval_helpers() {
    let a = iv(10, 20);
    assert_eq!(a.len_ns(), 10);
    assert_eq!(iv(i64::MIN, i64::MAX).len_ns(), i64::MAX, "saturates");
    assert!(a.contains(10));
    assert!(a.contains(19));
    assert!(!a.contains(20), "half-open");
    assert!(!a.contains(9));
    assert_eq!(a.intersection(&iv(15, 30)), Some(iv(15, 20)));
    assert_eq!(
        a.intersection(&iv(20, 30)),
        None,
        "touching is not overlapping"
    );
    assert_eq!(a.intersection(&iv(0, 5)), None);
}

#[test]
fn invalid_interval_in_range_request_is_rejected() {
    let good = Message::RangeRequest {
        clip_id: clip(1),
        pov: dev(2),
        quality: Quality::Proxy,
        range: iv(100, 200),
    };
    assert_valid(&good);
    for bad in [iv(200, 100), iv(0, 100), iv(100, 100), iv(1, 601 * SEC)] {
        assert_invalid(&Message::RangeRequest {
            clip_id: clip(1),
            pov: dev(2),
            quality: Quality::Proxy,
            range: bad,
        });
    }
}

// ---------------------------------------------------------------------------------------
// ClipRequest
// ---------------------------------------------------------------------------------------

#[test]
fn defaults_match_the_spec() {
    let r = request();
    assert_eq!(
        (
            r.pre_ms,
            r.post_ms,
            r.margin_pre_ms,
            r.margin_post_ms,
            r.max_len_ms
        ),
        (30_000, 10_000, 2_000, 2_000, 180_000)
    );
    assert!(!r.requester_no_source);
    assert!(r.validate().is_ok());
}

#[test]
fn window_is_hotkey_minus_pre_plus_post_with_margins() {
    let r = request();
    let w = r.window_utc();
    assert_eq!(w.from_utc_ns, T0 - 32 * SEC);
    assert_eq!(w.to_utc_ns, T0 + 12 * SEC);
    assert_eq!(r.checked_window_utc(), Some(w));
    assert!(w.validate().is_ok());

    let r = ClipRequest {
        pre_ms: 1,
        post_ms: 2,
        margin_pre_ms: 3,
        margin_post_ms: 4,
        hotkey_utc_ns: 1_000_000_000,
        ..request()
    };
    assert_eq!(
        r.window_utc(),
        iv(1_000_000_000 - 4_000_000, 1_000_000_000 + 6_000_000)
    );
}

#[test]
fn window_never_panics_on_extreme_inputs() {
    let r = ClipRequest {
        hotkey_utc_ns: i64::MAX,
        ..request()
    };
    assert_eq!(r.window_utc().to_utc_ns, i64::MAX, "saturates");
    assert_eq!(r.checked_window_utc(), None);

    let r = ClipRequest {
        hotkey_utc_ns: i64::MIN,
        ..request()
    };
    assert_eq!(r.window_utc().from_utc_ns, i64::MIN);
    assert_eq!(r.checked_window_utc(), None);

    let r = ClipRequest {
        pre_ms: u32::MAX,
        post_ms: u32::MAX,
        margin_pre_ms: u32::MAX,
        margin_post_ms: u32::MAX,
        max_len_ms: u32::MAX,
        ..request()
    };
    let _ = r.window_utc();
    assert!(r.validate().is_err());
}

#[test]
fn clip_request_limits_accept_the_maximum() {
    let r = ClipRequest {
        pre_ms: 120_000,
        post_ms: 30_000,
        margin_pre_ms: 10_000,
        margin_post_ms: 10_000,
        max_len_ms: 180_000,
        ..request()
    };
    // 120+30+10+10 = 170 s <= 180 s
    assert_valid(&Message::ClipRequest(r));

    let exact = ClipRequest {
        pre_ms: 100_000,
        post_ms: 30_000,
        margin_pre_ms: 10_000,
        margin_post_ms: 10_000,
        max_len_ms: 150_000,
        ..request()
    };
    assert_valid(&Message::ClipRequest(exact));
}

#[test]
fn clip_request_rejects_each_bad_field() {
    let cases: Vec<(&str, ClipRequest)> = vec![
        (
            "pre_ms",
            ClipRequest {
                pre_ms: 120_001,
                ..request()
            },
        ),
        (
            "post_ms",
            ClipRequest {
                post_ms: 30_001,
                ..request()
            },
        ),
        (
            "margin_pre_ms",
            ClipRequest {
                margin_pre_ms: 10_001,
                ..request()
            },
        ),
        (
            "margin_post_ms",
            ClipRequest {
                margin_post_ms: 10_001,
                ..request()
            },
        ),
        (
            "max_len_ms",
            ClipRequest {
                max_len_ms: 180_001,
                ..request()
            },
        ),
        (
            "sum > max_len",
            ClipRequest {
                max_len_ms: 43_999,
                ..request()
            },
        ),
        (
            "sum overflow of u32",
            ClipRequest {
                pre_ms: u32::MAX,
                post_ms: u32::MAX,
                ..request()
            },
        ),
        (
            "negative uncertainty",
            ClipRequest {
                sync_uncertainty_ns: -1,
                ..request()
            },
        ),
        (
            "hotkey zero",
            ClipRequest {
                hotkey_utc_ns: 0,
                ..request()
            },
        ),
        (
            "hotkey negative",
            ClipRequest {
                hotkey_utc_ns: -T0,
                ..request()
            },
        ),
        (
            "window overflow (end)",
            ClipRequest {
                hotkey_utc_ns: i64::MAX,
                ..request()
            },
        ),
        (
            "window overflow (end, by one ms)",
            ClipRequest {
                hotkey_utc_ns: i64::MAX - 11_999_999,
                ..request()
            },
        ),
        (
            "empty window",
            ClipRequest {
                pre_ms: 0,
                post_ms: 0,
                margin_pre_ms: 0,
                margin_post_ms: 0,
                ..request()
            },
        ),
    ];
    for (name, bad) in cases {
        assert!(
            matches!(bad.validate(), Err(ProtoError::Invalid(_))),
            "{name}: validate should fail"
        );
        assert_invalid(&Message::ClipRequest(bad));
    }
    // One ms inside the overflow edge is fine.
    let edge = ClipRequest {
        hotkey_utc_ns: i64::MAX - 12 * SEC,
        ..request()
    };
    assert!(edge.validate().is_ok());
}

#[test]
fn sum_check_is_exact() {
    // pre+post+margins = 44_000 ms with defaults.
    let at = ClipRequest {
        max_len_ms: 44_000,
        ..request()
    };
    assert!(at.validate().is_ok());
    let below = ClipRequest {
        max_len_ms: 43_999,
        ..request()
    };
    assert!(below.validate().is_err());
}

#[test]
fn decode_validates_clip_request() {
    let bytes = tweak(&Message::ClipRequest(request()), |v| {
        v["msg"]["pre_ms"] = 999_999.into();
    });
    assert!(matches!(decode(&bytes), Err(ProtoError::Invalid(_))));
}

// ---------------------------------------------------------------------------------------
// ClipExtend / TimePong / Hello
// ---------------------------------------------------------------------------------------

#[test]
fn clip_extend_rule() {
    let mk = |t| Message::ClipExtend {
        clip_id: clip(1),
        requester: dev(2),
        seq: 1,
        new_hotkey_utc_ns: t,
    };
    assert_valid(&mk(1));
    assert_invalid(&mk(0));
    assert_invalid(&mk(-1));
    assert_invalid(&mk(i64::MIN));
}

#[test]
fn time_pong_rule() {
    let mk = |t2, t3| Message::TimePong {
        seq: 1,
        t1_local_ns: 0,
        t2_local_ns: t2,
        t3_local_ns: t3,
    };
    assert_valid(&mk(10, 10));
    assert_valid(&mk(10, 11));
    assert_valid(&mk(-10, -5));
    assert_invalid(&mk(11, 10));
    assert_invalid(&mk(0, -1));
    assert_invalid(&mk(i64::MAX, i64::MIN));
}

#[test]
fn hello_app_version_rules() {
    let mk = |v: &str| Message::Hello {
        device_id: dev(1),
        crew_id: crew(1),
        app_version: v.into(),
        protocol_version: 1,
    };
    for ok in ["0", "1.2.3", "1.2.3-rc.1+build5", "A", &"x".repeat(32)] {
        assert_valid(&mk(ok));
    }
    for bad in [
        "",
        &"x".repeat(33),
        "1.2 3",
        "v1_0",
        "1.0\n",
        "ver\u{e3}o",
        "1.0/2",
        "1.0\"",
        "\u{ff11}",
    ] {
        assert_invalid(&mk(bad));
    }
}

// ---------------------------------------------------------------------------------------
// ClipAck
// ---------------------------------------------------------------------------------------

#[test]
fn ack_coverage_rules() {
    let many = |n: usize| -> Vec<Interval> {
        (0..n as i64)
            .map(|i| iv(T0 + i * 2 * SEC, T0 + i * 2 * SEC + SEC))
            .collect()
    };
    assert_valid(&Message::ClipAck(ClipAck {
        coverage: many(64),
        ..ack(AckStage::Ready)
    }));
    assert_invalid(&Message::ClipAck(ClipAck {
        coverage: many(65),
        ..ack(AckStage::Ready)
    }));
    assert_invalid(&Message::ClipAck(ClipAck {
        coverage: vec![iv(T0, T0)],
        ..ack(AckStage::Ready)
    }));
    assert_invalid(&Message::ClipAck(ClipAck {
        coverage: vec![iv(T0, T0 + SEC), iv(0, 10)],
        ..ack(AckStage::Pinned)
    }));
    assert_invalid(&Message::ClipAck(ClipAck {
        coverage: vec![iv(T0, T0 + 601 * SEC)],
        ..ack(AckStage::Pinned)
    }));
}

#[test]
fn ack_other_reason_length_rule() {
    let other = |s: String| {
        Message::ClipAck(ack(AckStage::Rejected {
            reason: RejectReason::Other(s),
        }))
    };
    assert_valid(&other(String::new()));
    assert_valid(&other("a".repeat(200)));
    assert_invalid(&other("a".repeat(201)));
    // Characters, not bytes: 200 multi-byte characters are fine, 201 are not.
    assert_valid(&other("\u{e9}".repeat(200)));
    assert_invalid(&other("\u{e9}".repeat(201)));
}

// ---------------------------------------------------------------------------------------
// ChunkRef and object keys
// ---------------------------------------------------------------------------------------

fn with_key(k: &str) -> Message {
    Message::ChunkAvailable(ChunkRef {
        object_key: k.into(),
        ..chunk()
    })
}

#[test]
fn chunk_ref_object_key_rules() {
    for ok in [
        "clips/a/b.bin",
        "clips/x-y_z.0/1",
        "clips/",
        &object_key(crew(1), clip(2), dev(3), Quality::Full, 123),
        &format!("clips/{}", "a".repeat(512 - 6)),
    ] {
        assert_valid(&with_key(ok));
    }
    assert_eq!(format!("clips/{}", "a".repeat(512 - 6)).len(), 512);

    let long = format!("clips/{}", "a".repeat(512 - 5));
    assert_eq!(long.len(), 513);
    for bad in [
        "",
        "clip/a.bin",
        "Clips/a.bin",
        "/clips/a.bin",
        "other/a.bin",
        "clips/a/../b.bin",
        "clips/..",
        "clips/a..b",
        "clips//a.bin",
        "clips/a//b",
        "clips/a b.bin",
        "clips/a\\b.bin",
        "clips/a?x=1",
        "clips/a%2Fb",
        "clips/a:b",
        "clips/\u{e9}.bin",
        "clips/a\n.bin",
        "clips/a\0.bin",
        long.as_str(),
    ] {
        assert_invalid(&with_key(bad));
    }
}

#[test]
fn chunk_ref_hash_rules() {
    let mk = |h: &str| {
        Message::ChunkAvailable(ChunkRef {
            sha256_hex: h.into(),
            ..chunk()
        })
    };
    assert_valid(&mk(&"0123456789abcdef".repeat(4)));
    for bad in [
        String::new(),
        "ab".repeat(31),
        "ab".repeat(33),
        "AB".repeat(32),
        format!("{}G", "a".repeat(63)),
        format!("{}\u{e9}", "a".repeat(62)),
        format!(" {}", "a".repeat(63)),
        "0x".to_owned() + &"a".repeat(62),
    ] {
        assert_invalid(&mk(&bad));
    }
}

#[test]
fn chunk_ref_size_and_range_rules() {
    let mk = |size| {
        Message::ChunkAvailable(ChunkRef {
            size_bytes: size,
            ..chunk()
        })
    };
    assert_valid(&mk(0));
    assert_valid(&mk(64 * 1024 * 1024));
    assert_invalid(&mk(64 * 1024 * 1024 + 1));
    assert_invalid(&mk(u64::MAX));

    for bad in [iv(5, 5), iv(9, 3), iv(0, 9), iv(1, 700 * SEC)] {
        assert_invalid(&Message::ChunkAvailable(ChunkRef {
            range: bad,
            ..chunk()
        }));
    }
}

#[test]
fn object_keys_have_a_stable_format() {
    let (c, k, p) = (crew(0xA), clip(0xB), dev(0xC));
    assert_eq!(
        object_key(c, k, p, Quality::Proxy, 0),
        "clips/00000000-0000-0000-0000-00000000000a/00000000-0000-0000-0000-00000000000b/\
         00000000-0000-0000-0000-00000000000c/proxy/000000.bin"
    );
    assert_eq!(
        object_key(c, k, p, Quality::Full, 42),
        "clips/00000000-0000-0000-0000-00000000000a/00000000-0000-0000-0000-00000000000b/\
         00000000-0000-0000-0000-00000000000c/full/000042.bin"
    );
    assert_eq!(
        object_key(c, k, p, Quality::Full, 1_234_567),
        "clips/00000000-0000-0000-0000-00000000000a/00000000-0000-0000-0000-00000000000b/\
         00000000-0000-0000-0000-00000000000c/full/1234567.bin"
    );
    assert_eq!(
        manifest_key(c, k, p, Quality::Proxy),
        "clips/00000000-0000-0000-0000-00000000000a/00000000-0000-0000-0000-00000000000b/\
         00000000-0000-0000-0000-00000000000c/proxy/manifest.bin"
    );
    assert_eq!(
        clip_prefix(c, k),
        "clips/00000000-0000-0000-0000-00000000000a/00000000-0000-0000-0000-00000000000b/"
    );
}

#[test]
fn generated_keys_are_valid_and_share_the_clip_prefix() {
    let (c, k, p) = (crew(u128::MAX), clip(u128::MAX), dev(u128::MAX));
    let prefix = clip_prefix(c, k);
    for q in [Quality::Proxy, Quality::Full] {
        for idx in [0, 1, 999_999, 1_000_000, u32::MAX] {
            let key = object_key(c, k, p, q, idx);
            assert!(validate_object_key(&key).is_ok(), "{key}");
            assert!(key.starts_with(&prefix));
            assert!(key.len() <= MAX_OBJECT_KEY_BYTES);
        }
        let m = manifest_key(c, k, p, q);
        assert!(validate_object_key(&m).is_ok());
        assert!(m.starts_with(&prefix));
    }
    assert!(validate_object_key(&prefix).is_ok());
}

// ---------------------------------------------------------------------------------------
// ClipKey
// ---------------------------------------------------------------------------------------

#[test]
fn clip_key_rules() {
    let mk = |s: &str| Message::ClipKey {
        clip_id: clip(1),
        key_b64: s.into(),
    };
    assert_valid(&mk(KEY_B64_ZEROS));
    // Alphabet edge cases: every byte 0xFF ("/") and every byte pattern that yields "+".
    assert_valid(&mk(&format!("{}8=", "/".repeat(42))));
    assert_valid(&mk(&format!("{}8=", "+".repeat(42))));

    for bad in [
        "".to_owned(),
        "not base64!".to_owned(),
        // 31 bytes
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        // 33 bytes
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        // 64 bytes
        "A".repeat(86) + "==",
        // URL-safe alphabet is not accepted
        "-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_-_8=".to_owned(),
        // 32 bytes but unpadded
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        // non-canonical trailing bits
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=".to_owned(),
        // whitespace
        " AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned(),
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n".to_owned(),
    ] {
        assert_invalid(&mk(&bad));
    }
}

#[test]
fn debug_output_never_contains_the_clip_key() {
    let secret = "q83vEjRWeJqrzu3M0pYm8zDd7yH0yX5Y1pSx2cJ0aVQ=";
    let msg = Message::ClipKey {
        clip_id: clip(1),
        key_b64: secret.into(),
    };
    assert_valid(&msg);
    let dbg = format!("{msg:?}");
    assert!(!dbg.contains(secret));
    assert!(!dbg.contains("q83v"));
    assert!(dbg.contains("redacted"));
    assert!(!format!("{:#?}", Envelope { v: 1, msg }).contains(secret));
}

#[test]
fn debug_of_other_messages_is_informative() {
    for msg in all_messages() {
        let dbg = format!("{msg:?}");
        assert!(!dbg.is_empty());
    }
    assert_eq!(format!("{:?}", Message::Bye), "Bye");
}

// ---------------------------------------------------------------------------------------
// Hostile input
// ---------------------------------------------------------------------------------------

#[test]
fn oversized_input_is_rejected_before_parsing() {
    let valid = encode(&Message::Bye).unwrap();
    // Exactly at the limit (valid JSON padded with whitespace) is accepted.
    let mut at_limit = valid.clone();
    at_limit.resize(MAX_MESSAGE_BYTES, b' ');
    assert_eq!(decode(&at_limit).unwrap(), Message::Bye);

    // One byte over is TooLarge, even though it is still valid JSON.
    let mut over = valid;
    over.resize(MAX_MESSAGE_BYTES + 1, b' ');
    assert_eq!(
        decode(&over),
        Err(ProtoError::TooLarge(MAX_MESSAGE_BYTES + 1))
    );

    // Garbage that is also too large reports the size first.
    let junk = vec![0xFFu8; MAX_MESSAGE_BYTES * 2];
    assert_eq!(
        decode(&junk),
        Err(ProtoError::TooLarge(MAX_MESSAGE_BYTES * 2))
    );
}

#[test]
fn wrong_version_is_rejected() {
    for v in [0u16, 2, 3, 255, u16::MAX] {
        let bytes = tweak(&Message::Bye, |j| j["v"] = v.into());
        assert_eq!(decode(&bytes), Err(ProtoError::UnsupportedVersion(v)));
    }
    // A future version may carry unknown message types and fields: still reported as a
    // version problem rather than as malformed JSON.
    let future = br#"{"v":2,"msg":{"type":"hologram","x":1},"extra":true}"#;
    assert_eq!(decode(future), Err(ProtoError::UnsupportedVersion(2)));
}

#[test]
fn bad_version_field_shapes_are_json_errors() {
    for bad in [
        &br#"{"msg":{"type":"bye"}}"#[..],
        br#"{"v":"1","msg":{"type":"bye"}}"#,
        br#"{"v":-1,"msg":{"type":"bye"}}"#,
        br#"{"v":65536,"msg":{"type":"bye"}}"#,
        br#"{"v":1.5,"msg":{"type":"bye"}}"#,
        br#"{"v":null,"msg":{"type":"bye"}}"#,
    ] {
        assert!(
            matches!(decode(bad), Err(ProtoError::Json(_))),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn malformed_envelopes_are_json_errors() {
    for bad in [
        &br#"{"v":1}"#[..],
        br#"{"v":1,"msg":null}"#,
        br#"{"v":1,"msg":"bye"}"#,
        br#"{"v":1,"msg":{}}"#,
        br#"{"v":1,"msg":{"type":"nope"}}"#,
        br#"{"v":1,"msg":{"type":"BYE"}}"#,
        br#"{"v":1,"msg":{"type":7}}"#,
        br#"{"v":1,"msg":{"type":null}}"#,
        br#"{"v":1,"msg":{"type":"bye"},"extra":1}"#,
        br#"{"v":1,"msg":{"type":"clip_delete","clip_id":"00000000-0000-0000-0000-000000000001","extra":1}}"#,
        br#"{"v":1,"msg":{"type":"time_ping","seq":1,"t1_local_ns":1,"extra":1}}"#,
        br#"{"v":1,"v":1,"msg":{"type":"bye"}}"#,
        br#"{"v":1,"msg":{"type":"bye"}} trailing"#,
        br#"{"v":1,"msg":{"type":"bye"}}{"v":1,"msg":{"type":"bye"}}"#,
        br#"[1,2,3]"#,
        br#""bye""#,
        b"42",
        b"null",
        b"",
        b"   ",
        b"{",
        br#"{"v":1,"msg":{"type":"bye""#,
        b"\xEF\xBB\xBF{\"v\":1,\"msg\":{\"type\":\"bye\"}}",
    ] {
        assert!(
            matches!(decode(bad), Err(ProtoError::Json(_))),
            "should be a Json error: {}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn wrong_field_types_and_missing_fields_are_rejected() {
    let base = Message::ClipRequest(request());
    type Edit = Box<dyn Fn(&mut serde_json::Value)>;
    let edits: Vec<Edit> = vec![
        Box::new(|v| v["msg"]["seq"] = (-1).into()),
        Box::new(|v| v["msg"]["seq"] = 4_294_967_296u64.into()),
        Box::new(|v| v["msg"]["pre_ms"] = "30000".into()),
        Box::new(|v| v["msg"]["pre_ms"] = 1.5.into()),
        Box::new(|v| v["msg"]["hotkey_utc_ns"] = serde_json::Value::Null),
        Box::new(|v| v["msg"]["hotkey_utc_ns"] = u64::MAX.into()),
        Box::new(|v| v["msg"]["requester_no_source"] = 1.into()),
        Box::new(|v| v["msg"]["clip_id"] = "not-a-uuid".into()),
        Box::new(|v| v["msg"]["clip_id"] = 5.into()),
        Box::new(|v| {
            v["msg"].as_object_mut().unwrap().remove("post_ms");
        }),
        Box::new(|v| v["msg"]["surprise"] = true.into()),
    ];
    for (i, edit) in edits.iter().enumerate() {
        let bytes = tweak(&base, edit);
        assert!(
            matches!(decode(&bytes), Err(ProtoError::Json(_))),
            "edit #{i} should fail: {}",
            String::from_utf8_lossy(&bytes)
        );
    }

    let ack_msg = Message::ClipAck(ack(AckStage::Ready));
    for edit in [
        (|v: &mut serde_json::Value| v["msg"]["stage"] = "exploded".into()) as fn(&mut _),
        |v| v["msg"]["stage"] = serde_json::json!({"rejected": {"reason": "bored"}}),
        |v| v["msg"]["stage"] = serde_json::json!({"rejected": {}}),
        |v| v["msg"]["coverage"] = serde_json::json!([{"from_utc_ns": 1}]),
        |v| v["msg"]["coverage"] = serde_json::json!([{"from_utc_ns": 1, "to_utc_ns": 2, "x": 3}]),
        |v| v["msg"]["expected_bytes"] = (-1).into(),
    ] {
        let bytes = tweak(&ack_msg, edit);
        assert!(
            matches!(decode(&bytes), Err(ProtoError::Json(_))),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
    }

    let quality_msg = Message::RangeRequest {
        clip_id: clip(1),
        pov: dev(2),
        quality: Quality::Full,
        range: iv(1, 2),
    };
    let bytes = tweak(&quality_msg, |v| v["msg"]["quality"] = "ultra".into());
    assert!(matches!(decode(&bytes), Err(ProtoError::Json(_))));
    let bytes = tweak(&quality_msg, |v| v["msg"]["quality"] = "Full".into());
    assert!(matches!(decode(&bytes), Err(ProtoError::Json(_))));
}

#[test]
fn pathological_json_does_not_panic_or_overflow_the_stack() {
    let depth = 30_000;
    let nested_array = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
    assert!(decode(nested_array.as_bytes()).is_err());

    let nested_in_field = format!(
        r#"{{"v":1,"msg":{{"type":"bye"}},"x":{}{}}}"#,
        "[".repeat(depth),
        "]".repeat(depth)
    );
    assert!(decode(nested_in_field.as_bytes()).is_err());

    let nested_in_msg = format!(
        r#"{{"v":1,"msg":{{"type":"clip_delete","clip_id":{}{}}}}}"#,
        "{\"a\":".repeat(10_000),
        "}".repeat(10_000)
    );
    assert!(decode(nested_in_msg.as_bytes()).is_err());

    let huge_number = format!(
        r#"{{"v":1,"msg":{{"type":"time_ping","seq":{},"t1_local_ns":0}}}}"#,
        "9".repeat(5_000)
    );
    assert!(decode(huge_number.as_bytes()).is_err());

    let long_unknown_type = format!(r#"{{"v":1,"msg":{{"type":"{}"}}}}"#, "z".repeat(60_000));
    match decode(long_unknown_type.as_bytes()) {
        Err(ProtoError::Json(text)) => {
            assert!(text.chars().count() <= 210, "error text must be truncated")
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn garbage_bytes_never_panic() {
    // Deterministic xorshift so the test is reproducible.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for round in 0..2_000 {
        let len = (next() % 300) as usize;
        let junk: Vec<u8> = (0..len).map(|_| (next() & 0xFF) as u8).collect();
        assert!(decode(&junk).is_err(), "round {round}");
    }
}

#[test]
fn mutating_valid_messages_never_panics() {
    let mut state = 0xDEAD_BEEF_CAFE_F00Du64;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for msg in all_messages() {
        let bytes = encode(&msg).unwrap();
        // Every truncation.
        for cut in 0..bytes.len() {
            let _ = decode(&bytes[..cut]);
        }
        // Single random byte replacements; whatever decodes must also re-encode.
        for _ in 0..300 {
            let mut copy = bytes.clone();
            let at = (next() as usize) % copy.len();
            copy[at] = (next() & 0xFF) as u8;
            if let Ok(decoded) = decode(&copy) {
                assert!(encode(&decoded).is_ok());
            }
        }
    }
}

#[test]
fn encode_decode_agree_on_validity() {
    // Everything decode accepts must satisfy validate; everything encode accepts must decode.
    for msg in all_messages() {
        assert!(msg.validate().is_ok());
        let env = Envelope {
            v: PROTOCOL_VERSION,
            msg: msg.clone(),
        };
        let via_envelope = serde_json::to_vec(&env).unwrap();
        assert_eq!(via_envelope, encode(&msg).unwrap());
    }
}

#[test]
fn error_messages_are_human_readable() {
    assert_eq!(
        ProtoError::TooLarge(70_000).to_string(),
        "message too large: 70000 bytes (max 65536)"
    );
    assert_eq!(
        ProtoError::UnsupportedVersion(9).to_string(),
        "unsupported protocol version 9"
    );
    assert!(ProtoError::Invalid("x".into()).to_string().contains('x'));
    assert!(ProtoError::Json("y".into()).to_string().contains('y'));
}

// ---------------------------------------------------------------------------------------
// Adversarial review regressions
// ---------------------------------------------------------------------------------------

#[test]
fn clip_extend_rejects_a_hotkey_whose_window_would_overflow() {
    let mk = |t| Message::ClipExtend {
        clip_id: clip(1),
        requester: dev(2),
        seq: 1,
        new_hotkey_utc_ns: t,
    };
    // Worst-case reach past the press: max post + max margin.
    let reach = (i64::from(MAX_POST_MS) + i64::from(MAX_MARGIN_MS)) * 1_000_000;
    assert_valid(&mk(i64::MAX - reach));
    assert_invalid(&mk(i64::MAX - reach + 1));
    assert_invalid(&mk(i64::MAX));
    // And the same through the wire.
    let json = format!(
        r#"{{"v":1,"msg":{{"type":"clip_extend","clip_id":"{}","requester":"{}","seq":1,"new_hotkey_utc_ns":{}}}}}"#,
        clip(1),
        dev(2),
        i64::MAX
    );
    assert!(matches!(
        decode(json.as_bytes()),
        Err(ProtoError::Invalid(_))
    ));
}

#[test]
fn bye_with_extra_fields_is_rejected_on_decode() {
    assert_eq!(
        decode(br#"{"v":1,"msg":{"type":"bye"}}"#).unwrap(),
        Message::Bye
    );
    for bad in [
        &br#"{"v":1,"msg":{"type":"bye","x":1}}"#[..],
        br#"{"v":1,"msg":{"type":"bye","payload":{"a":[1,2,3]}}}"#,
        br#"{"v":1,"msg":{"type":"bye","clip_id":"00000000-0000-0000-0000-000000000001"}}"#,
        br#"{"v":1,"extra":true,"msg":{"type":"bye"}}"#,
    ] {
        assert!(decode(bad).is_err(), "{}", String::from_utf8_lossy(bad));
    }
    // What the encoder produces still decodes.
    assert_eq!(
        decode(&encode(&Message::Bye).unwrap()).unwrap(),
        Message::Bye
    );
}

#[test]
fn ack_stage_and_reject_reason_reject_unknown_fields_and_shapes() {
    let ack = |stage: &str| {
        format!(
            r#"{{"v":1,"msg":{{"type":"clip_ack","clip_id":"{}","from":"{}","stage":{stage},"coverage":[],"expected_bytes":null,"proxy_available":false,"truncated_by_source_end":false}}}}"#,
            clip(1),
            dev(2)
        )
        .into_bytes()
    };
    assert!(decode(&ack(r#""pinned""#)).is_ok());
    assert!(decode(&ack(r#"{"rejected":{"reason":"no_source"}}"#)).is_ok());
    assert!(decode(&ack(r#"{"rejected":{"reason":{"other":"x"}}}"#)).is_ok());
    for bad in [
        r#"{"rejected":{"reason":"no_source","extra":1}}"#,
        r#"{"rejected":{}}"#,
        r#"{"rejected":"no_source"}"#,
        r#"{"rejected":{"reason":"Other"}}"#,
        r#"{"rejected":{"reason":"nope"}}"#,
        r#"{"rejected":{"reason":{"other":5}}}"#,
        r#"{"rejected":{"reason":{"other":"x","extra":1}}}"#,
        r#""Pinned""#,
        r#"{"pinned":{"x":1}}"#,
        r#"{"ready":null,"pinned":null}"#,
        "null",
    ] {
        assert!(decode(&ack(bad)).is_err(), "{bad}");
    }
    // An Other(..) reason of 201 characters is rejected, 200 accepted, counted in characters.
    let other = |n: usize| {
        Message::ClipAck(ack_with(AckStage::Rejected {
            reason: RejectReason::Other("\u{e9}".repeat(n)),
        }))
    };
    assert_valid(&other(200));
    assert_invalid(&other(201));
}

fn ack_with(stage: AckStage) -> ClipAck {
    ack(stage)
}

#[test]
fn chunk_ref_verify_object_key_ties_the_key_to_the_announced_chunk() {
    let good = chunk();
    assert!(good.validate().is_ok());
    assert!(good.verify_object_key(crew(9)).is_ok());

    // Wrong crew: a peer pointing at another crew's objects.
    assert!(good.verify_object_key(crew(10)).is_err());

    let key_of = |c, cl, p, q, i| object_key(c, cl, p, q, i);
    let others: [(&str, String); 7] = [
        (
            "other clip",
            key_of(crew(9), clip(77), dev(2), Quality::Proxy, 3),
        ),
        (
            "other pov",
            key_of(crew(9), clip(1), dev(77), Quality::Proxy, 3),
        ),
        (
            "other quality",
            key_of(crew(9), clip(1), dev(2), Quality::Full, 3),
        ),
        (
            "other index",
            key_of(crew(9), clip(1), dev(2), Quality::Proxy, 4),
        ),
        (
            "manifest object",
            manifest_key(crew(9), clip(1), dev(2), Quality::Proxy),
        ),
        (
            "trailing junk",
            format!(
                "{}x",
                object_key(crew(9), clip(1), dev(2), Quality::Proxy, 3)
            ),
        ),
        (
            "uppercased",
            object_key(crew(9), clip(1), dev(2), Quality::Proxy, 3).to_uppercase(),
        ),
    ];
    for (what, key) in others {
        let forged = ChunkRef {
            object_key: key,
            ..chunk()
        };
        assert!(
            forged.verify_object_key(crew(9)).is_err(),
            "{what} must not verify"
        );
    }
    // Fields that change while the key stays put are caught too.
    for forged in [
        ChunkRef {
            clip_id: clip(2),
            ..chunk()
        },
        ChunkRef {
            pov: dev(3),
            ..chunk()
        },
        ChunkRef {
            quality: Quality::Full,
            ..chunk()
        },
        ChunkRef {
            index: 4,
            ..chunk()
        },
    ] {
        assert!(forged.verify_object_key(crew(9)).is_err());
    }
}

#[test]
fn duplicate_keys_never_slip_through() {
    for bad in [
        r#"{"v":1,"v":1,"msg":{"type":"bye"}}"#,
        r#"{"v":1,"msg":{"type":"bye"},"msg":{"type":"bye"}}"#,
        r#"{"v":1,"msg":{"type":"bye","type":"bye"}}"#,
        r#"{"v":1,"msg":{"type":"time_ping","seq":1,"seq":2,"t1_local_ns":0}}"#,
        // Two different versions: the probe and the full parse must agree.
        r#"{"v":2,"v":1,"msg":{"type":"bye"}}"#,
    ] {
        assert!(decode(bad.as_bytes()).is_err(), "{bad}");
    }
}
