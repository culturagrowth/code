//! The friends flow against a LOCAL Worker (`wrangler dev` on 127.0.0.1:8787). It never talks
//! to a deployed Worker, Cloudflare or R2: the address is fixed to loopback.
//!
//! Setup (from `worker/README.md`, "Testar o fluxo de dois amigos"):
//! ```text
//! cd worker
//! npx wrangler d1 migrations apply duoclip --local
//! npm run dev
//! # then, in another terminal:
//! cargo test -p duoclip-net --test local_worker -- --ignored
//! ```

use duoclip_net::{Identity, NetError, WorkerClient};
use duoclip_presence::{HeartbeatFailure, HeartbeatRequest};
use duoclip_proto::{ClipId, CrewId};

const LOCAL_WORKER: &str = "http://127.0.0.1:8787";

fn new_client(name: &str) -> WorkerClient {
    let identity = Identity::generate(name).unwrap();
    WorkerClient::new(LOCAL_WORKER, identity).unwrap()
}

fn idle(seq: u64, online_since_ms: u64) -> HeartbeatRequest {
    HeartbeatRequest {
        game: None,
        active_crew: None,
        seated_since_ms: None,
        seq,
        online_since_ms,
    }
}

#[test]
#[ignore = "needs a local Worker (npm run dev)"]
fn two_friends_create_invite_join_and_see_each_other() {
    let mut ana = new_client("Ana");
    let mut bia = new_client("Bia");
    ana.health().unwrap();

    ana.register_device().unwrap();
    bia.register_device().unwrap();
    // Registering again with the same key is harmless.
    ana.register_device().unwrap();

    // A request signed by the right device id but the wrong key is refused.
    let mut impostor = WorkerClient::new(
        LOCAL_WORKER,
        Identity::from_secret_bytes(ana.identity().device_id(), "Ana", &[3u8; 32]).unwrap(),
    )
    .unwrap();
    let err = impostor.create_crew("Invasores").unwrap_err();
    assert_eq!(err.status(), Some(401), "{err:?}");

    let crew = ana.create_crew("Amigos de teste").unwrap();
    let invite = ana.create_invite(crew).unwrap();
    // Bia is not a member yet.
    assert_eq!(bia.members(crew).unwrap_err().code(), Some("not_a_member"));

    assert_eq!(bia.join_crew(&invite.code).unwrap(), crew);
    // Joining again with the same code is idempotent.
    assert_eq!(bia.join_crew(&invite.code).unwrap(), crew);
    let bad = bia.join_crew("AAAAAAAAAA").unwrap_err();
    assert_eq!(bad.code(), Some("invalid_invite"));

    for client in [&mut ana, &mut bia] {
        let members = client.members(crew).unwrap();
        let names: Vec<&str> = members.iter().map(|m| m.display_name.as_str()).collect();
        assert!(
            names.contains(&"Ana") && names.contains(&"Bia"),
            "{names:?}"
        );
    }

    // Presence: both announce, both see the crew snapshot with the other online.
    let run = 1_700_000_000_000u64;
    ana.heartbeat(&idle(1, run)).unwrap();
    let snapshot = bia.heartbeat(&idle(1, run)).unwrap();
    let mine = snapshot
        .crews
        .iter()
        .find(|c| c.crew_id == crew.to_string())
        .expect("the crew shows up in the snapshot");
    let ids: Vec<&str> = mine.members.iter().map(|m| m.device_id.as_str()).collect();
    assert!(ids.contains(&ana.identity().device_id().to_string().as_str()));

    // The same run/sequence again is stale, and maps onto the presence adapter's failure kind.
    let err = ana.heartbeat(&idle(1, run)).unwrap_err();
    assert_eq!(err.status(), Some(409), "{err:?}");
    assert_eq!(HeartbeatFailure::from(&err), HeartbeatFailure::Stale409);
    ana.heartbeat(&idle(2, run)).unwrap();

    // Clip registration works without R2; registering twice is idempotent.
    let clip = ClipId::new_random();
    ana.register_clip(clip, crew, 3600).unwrap();
    ana.register_clip(clip, crew, 3600).unwrap();
    // An outsider cannot register a clip in the crew.
    let mut outsider = new_client("Intruso");
    outsider.register_device().unwrap();
    let err: NetError = outsider
        .register_clip(ClipId::new_random(), crew, 60)
        .unwrap_err();
    assert_eq!(err.code(), Some("not_a_member"));

    // Going idle again so the test does not leave a lingering "online" announcement.
    ana.heartbeat(&idle(3, run)).unwrap();
    bia.heartbeat(&idle(2, run)).unwrap();
    let _: CrewId = crew;
}
