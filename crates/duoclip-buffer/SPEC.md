# duoclip-buffer — SPEC (replay ring buffer + "fixar e coletar" / pin-and-collect post-roll)

Holds the last N seconds of ENCODED packets (video H.264 + several audio tracks) in RAM, like OBS's replay
buffer. Implements the clip state machine that saves N s before AND M s after the hotkey (docs section 6,
read it all, including the edge-case table 6.6). Platform-independent, `#![forbid(unsafe_code)]`.

Key insight (docs 6.2): a plain time-capped ring cannot "wait M seconds then save", because the pre-roll would be
evicted meanwhile. At request time the collector must take shared references (`Arc`) to the buffered packets.
The ring then keeps evicting freely, while the memory stays alive through the collector's references.

## Units and conventions

- All times are `local_ns: i64` on the local monotonic clock (QPC on Windows). The UTC↔local mapping is injected
  through the `UtcToLocal` trait (the clock crate's `FrozenMapping` implements it in integration; this crate must
  NOT depend on duoclip-clock).
- Exactly one video track. Video packets carry `keyframe = true` on IDR frames (closed GOP, ~1 s, no B-frames, so
  pts == dts for video). Audio packets are always `keyframe = true`. Per track, packets arrive in non-decreasing
  dts order. Tracks may interleave arbitrarily, and audio may run ahead of or behind video by up to ~500 ms.

## API

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)] pub struct TrackId(pub u8);
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum TrackKind { Video, Audio }
#[derive(Clone, Debug)] pub struct TrackInfo { pub id: TrackId, pub kind: TrackKind, pub name: String } // e.g. "game", "discord", "mic"

#[derive(Clone, Debug)]
pub struct Packet { pub track: TrackId, pub pts_ns: i64, pub dts_ns: i64, pub duration_ns: i64, pub keyframe: bool, pub data: bytes::Bytes }
pub type SharedPacket = std::sync::Arc<Packet>;

pub struct RingConfig { pub max_duration_ns: i64 /* 60 s */, pub max_bytes: usize /* 600 MiB */, pub tracks: Vec<TrackInfo> }
pub struct RingBuffer { /* ... */ }
impl RingBuffer {
    pub fn new(cfg: RingConfig) -> Result<Self, BufferError>;     // exactly one video track
    pub fn push(&mut self, p: SharedPacket) -> Result<(), BufferError>; // unknown track / out-of-order per track => error, no panic
    pub fn oldest_pts(&self) -> Option<i64>;   // pts of the oldest retained video keyframe
    pub fn newest_pts(&self) -> Option<i64>;
    pub fn keyframe_at_or_before(&self, t: i64) -> Option<i64>;
    pub fn snapshot_from(&self, from_keyframe_pts: i64) -> Vec<SharedPacket>; // all tracks, video from that keyframe,
                                                                              // audio with pts >= that keyframe pts
    pub fn bytes(&self) -> usize;
    pub fn duration_ns(&self) -> i64;
}
```

**Eviction:** after each push, while `newest_video_pts - oldest_keyframe_pts > max_duration` OR `bytes > max_bytes`, evict
the oldest whole GOP: video packets from the oldest keyframe up to (not including) the next keyframe. Also evict audio
with `pts <` the new oldest keyframe pts. Never evict the GOP that is still being written, i.e. always keep the latest
keyframe and everything after it.

```rust
pub trait UtcToLocal { fn local_at(&self, utc_ns: i64) -> i64; }
impl<F: Fn(i64) -> i64> UtcToLocal for F { ... }

pub struct WindowRequest { pub hotkey_utc_ns: i64, pub pre_ns: i64, pub post_ns: i64,
                           pub margin_pre_ns: i64, pub margin_post_ns: i64, pub max_len_ns: i64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalWindow { pub start_local_ns: i64, pub end_local_ns: i64, pub hotkey_local_ns: i64 }
impl LocalWindow {
    /// start = local(hotkey - pre - margin_pre) - eps ; end = local(hotkey + post + margin_post) + eps
    /// (the length is capped at max_len_ns by moving END earlier). eps = sync uncertainty bound. Use checked arithmetic.
    pub fn compute(req: &WindowRequest, map: &dyn UtcToLocal, eps_ns: i64) -> Result<Self, BufferError>;
}

pub struct CollectConfig { pub finalize_timeout_ns: i64 /* 3 s */, pub max_len_ns: i64 /* 180 s */,
                           pub gap_threshold_ns: i64 /* 500 ms */ }

pub enum ClipState { Collecting, Done }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coverage { pub start_missing: bool, pub end_truncated: bool, pub truncated_by_source_end: bool,
                      pub actual_start_ns: i64, pub actual_end_ns: i64, pub gaps: Vec<(i64, i64)> }

pub struct Fragment { pub index: u32, pub start_pts_ns: i64, pub end_pts_ns: i64, pub packets: Vec<SharedPacket> }
pub struct FinishedClip { pub clip_id: uuid::Uuid, pub window: LocalWindow, pub coverage: Coverage,
                          pub packets: Vec<SharedPacket> /* sorted by dts, then track */, pub bytes: usize }

pub struct ClipCollector { /* ... */ }
impl ClipCollector {
    /// PIN: snapshot the ring from keyframe_at_or_before(start). If start < ring.oldest_pts(), use the oldest keyframe
    /// and set start_missing. If the window end already passed (late request), still pin, then finalize on the next tick.
    pub fn new(clip_id: uuid::Uuid, window: LocalWindow, ring: &RingBuffer, cfg: CollectConfig, now_local: i64) -> Self;
    /// COLLECT: append live packets with pts < end. A track "reaches the end" when it sees a packet with pts >= end
    /// (that packet is NOT included). Ignore packets older than the pinned start or already present (dedupe by (track, dts)).
    pub fn on_packet(&mut self, p: &SharedPacket);
    pub fn extend_end(&mut self, new_end_local: i64);   // only while collecting; capped at start + max_len
    pub fn source_ended(&mut self, now_local: i64);     // finalize now, truncated_by_source_end = true
    pub fn tick(&mut self, now_local: i64);             // finalize when ALL tracks reached the end, or now > end + timeout
    pub fn state(&self) -> ClipState;
    /// Fragments (one per GOP) ready for the local crash-safe bucket writer. A GOP fragment [k_i, k_{i+1}) is ready
    /// when the next video keyframe k_{i+1} has been seen AND every audio track has a packet with pts >= k_{i+1}
    /// (OBS mp4-mux rule). When finished, the last fragment is released too. Each packet is in exactly one fragment.
    /// Audio packets go to the fragment containing their pts. Indices are contiguous from 0.
    pub fn drain_ready_fragments(&mut self) -> Vec<Fragment>;
    pub fn take_finished(&mut self) -> Option<FinishedClip>;  // once Done
    pub fn held_bytes(&self) -> usize;
}
```
- **Coverage:** `actual_start` = first video keyframe pts included; `actual_end` = end pts of the last video packet
  included (pts + duration). `end_truncated` = finalized by timeout or source end before the end was reached.
  `gaps` = intervals where consecutive video packets are more than `gap_threshold` apart.

```rust
pub struct ManagerConfig { pub ring: RingConfig, pub collect: CollectConfig, pub max_active: usize /* 4 */,
                           pub pin_budget_bytes: usize /* 1 GiB */, pub merge_tolerance_ns: i64 /* 5 s */ }
pub struct ClipManager { /* ring + active collectors keyed by clip id */ }
impl ClipManager {
    pub fn new(cfg: ManagerConfig) -> Result<Self, BufferError>;
    pub fn push(&mut self, p: Packet, now_local: i64) -> Result<(), BufferError>; // to the ring + every collector
    /// Idempotent: an existing clip_id returns Ok(false) without creating a new one. Errors: TooManyActive, MemoryBudget.
    pub fn request(&mut self, clip_id: uuid::Uuid, window: LocalWindow, now_local: i64) -> Result<bool, BufferError>;
    pub fn extend(&mut self, clip_id: uuid::Uuid, new_end_local: i64) -> Result<(), BufferError>;
    pub fn source_ended(&mut self, now_local: i64);
    pub fn tick(&mut self, now_local: i64) -> Vec<FinishedClip>;
    pub fn drain_fragments(&mut self) -> Vec<(uuid::Uuid, Fragment)>;
    pub fn pinned_bytes(&self) -> usize;   // bytes held by collectors that the ring no longer holds (approximation OK; document it)
    pub fn ring(&self) -> &RingBuffer;
}
/// Docs 6.6: same requester, new window overlaps or is within tolerance of the active one => extend instead of a new clip.
pub fn should_extend(active: &LocalWindow, new: &LocalWindow, same_requester: bool, tolerance_ns: i64) -> bool;

#[derive(Debug, thiserror::Error)]
pub enum BufferError { UnknownTrack(u8), OutOfOrder { track: u8 }, NoVideoTrack, MultipleVideoTracks,
                       TooManyActive, MemoryBudget, UnknownClip, Overflow }
```

## Tests (required)

Write a synthetic stream generator: video at 60 fps with an IDR every 60 frames and realistic byte sizes (keyframes
larger); 2–3 audio tracks with 20 ms frames, optionally offset ±200 ms from the video.

- The ring keeps about `max_duration` (± one GOP), evicts whole GOPs, and respects `max_bytes`. Out-of-order or unknown-track
  packets give errors.
- **Post-roll:** pre = 30 s, post = 10 s, ring = 60 s. Request at T, keep pushing 15 s → the clip covers
  [keyframe ≤ T−32 s, T+12 s), all tracks are present, and the packets are sorted by dts. The ring had already evicted the oldest pinned
  packets, but the clip still has them (`Arc` keeps them alive).
- A late request where the start is already evicted gives `start_missing`, with `actual_start` = the oldest retained keyframe.
- Request after the end passed → finished on the next tick.
- `extend_end` lengthens the clip, capped at `max_len`.
- Timeout: an audio track stops → finalized at end + timeout with `end_truncated`.
- `source_ended` → `truncated_by_source_end`.
- Fragment rule: fragments are contiguous, every packet is in exactly one fragment, and no fragment is emitted before audio passes the next keyframe.
- Idempotent `request`. `TooManyActive` and `MemoryBudget` errors. Two overlapping clips share packet `Arc`s (`Arc::ptr_eq`).
- `should_extend` truth table. `LocalWindow::compute` with a non-trivial mapping (offset + rate), eps, and max_len capping.
- Property-style test: random interleavings/jitter never panic and the invariants hold.
- `cargo clippy -p duoclip-buffer --all-targets -- -D warnings` is clean and `cargo fmt` is applied. Tests run in < 10 s.

## Implementation notes (accepted deviations, Phase A review)

- Packets with an equal dts on one track are kept (dedupe is by (track, dts, content), up to 8 per dts). A keyframe repeating the previous keyframe's pts does not create
  an empty fragment.
- A window whose start is in the future re-anchors at the first keyframe at or after the start (`start_missing`). Far-future windows finalize at
  `created + max_len + timeout` and are `end_truncated`. Packets beyond `start + max_len` are not parked.
- `source_ended`: if every track already reached the end, the clip is complete (`truncated_by_source_end` stays false).
- Only keyframes included in the clip delimit fragments, so an extension never adds packets to an already-released fragment.
- `MemoryBudget` is checked only at request time (worst-case projection). Post-roll growth up to `max_len` is not projected.
- Configure **only active tracks**: a configured track that never produces packets makes clips wait for the timeout and holds back fragments.
- `FinishedClip` and drained fragments share `Arc`s, so RAM is freed only when both are dropped. Release-after-write is a Phase B task.
- Additive public API: the `synth` module (synthetic stream generator), `TrackInfo::{video,audio}`, `*Config::with_tracks`, `RingBuffer::gop_count`, etc.
