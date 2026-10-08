//! Owns the ring buffer and the active clip collectors.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use uuid::Uuid;

use crate::collector::{ClipCollector, ClipState, CollectConfig, FinishedClip, Fragment};
use crate::ring::{RingBuffer, RingConfig};
use crate::types::{Packet, SharedPacket, TrackInfo};
use crate::window::LocalWindow;
use crate::{BufferError, NS_PER_SEC};

/// How many finished clip ids are remembered so that a re-sent request for an already finished
/// clip (docs 6.6, reconnect) is still recognised as a duplicate.
pub const FINISHED_ID_MEMORY: usize = 1024;

/// Configuration of a [`ClipManager`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerConfig {
    /// Ring buffer configuration.
    pub ring: RingConfig,
    /// Per-clip collector configuration.
    pub collect: CollectConfig,
    /// Maximum clips collecting at the same time (default 4).
    pub max_active: usize,
    /// Memory budget for pinned packets (default 1 GiB); see [`ClipManager::request`].
    pub pin_budget_bytes: usize,
    /// Tolerance used by the app with [`should_extend`](crate::should_extend) (default 5 s).
    pub merge_tolerance_ns: i64,
}

impl ManagerConfig {
    /// Default maximum active clips.
    pub const DEFAULT_MAX_ACTIVE: usize = 4;
    /// Default pin budget: 1 GiB.
    pub const DEFAULT_PIN_BUDGET_BYTES: usize = 1024 * 1024 * 1024;
    /// Default merge tolerance: 5 s.
    pub const DEFAULT_MERGE_TOLERANCE_NS: i64 = 5 * NS_PER_SEC;

    /// A configuration with all defaults (docs 6.4) for the given tracks.
    pub fn with_tracks(tracks: Vec<TrackInfo>) -> Self {
        Self {
            ring: RingConfig::with_tracks(tracks),
            collect: CollectConfig::default(),
            max_active: Self::DEFAULT_MAX_ACTIVE,
            pin_budget_bytes: Self::DEFAULT_PIN_BUDGET_BYTES,
            merge_tolerance_ns: Self::DEFAULT_MERGE_TOLERANCE_NS,
        }
    }
}

/// The ring buffer plus the active clip collectors, keyed by clip id.
///
/// Typical loop: [`push`](Self::push) every encoded packet, call [`tick`](Self::tick)
/// periodically (e.g. every 100 ms) to collect finished clips, and
/// [`drain_fragments`](Self::drain_fragments) to feed the bucket writer.
#[derive(Debug)]
pub struct ClipManager {
    ring: RingBuffer,
    collect: CollectConfig,
    max_active: usize,
    pin_budget_bytes: usize,
    merge_tolerance_ns: i64,
    active: Vec<(Uuid, ClipCollector)>,
    queued: Vec<(Uuid, Fragment)>,
    finished_ids: HashSet<Uuid>,
    finished_order: VecDeque<Uuid>,
}

impl ClipManager {
    /// Creates the manager and its ring (errors as [`RingBuffer::new`]).
    pub fn new(cfg: ManagerConfig) -> Result<Self, BufferError> {
        Ok(Self {
            ring: RingBuffer::new(cfg.ring)?,
            collect: cfg.collect,
            max_active: cfg.max_active,
            pin_budget_bytes: cfg.pin_budget_bytes,
            merge_tolerance_ns: cfg.merge_tolerance_ns,
            active: Vec::new(),
            queued: Vec::new(),
            finished_ids: HashSet::new(),
            finished_order: VecDeque::new(),
        })
    }

    /// Pushes a packet to the ring and to every collector. A packet the ring rejects (unknown
    /// track, out of order) is returned as an error and not given to the collectors.
    ///
    /// `now_local` is accepted for symmetry with the other calls; finalization happens in
    /// [`tick`](Self::tick).
    pub fn push(&mut self, p: Packet, now_local: i64) -> Result<(), BufferError> {
        let _ = now_local;
        let p: SharedPacket = Arc::new(p);
        self.ring.push(Arc::clone(&p))?;
        for (_, c) in &mut self.active {
            c.on_packet(&p);
        }
        Ok(())
    }

    /// Idempotent: an existing clip_id returns Ok(false) without creating a new one. Errors: TooManyActive, MemoryBudget.
    ///
    /// "Existing" covers the active clips and the last [`FINISHED_ID_MEMORY`] finished ones.
    /// `TooManyActive` when `max_active` clips are already collecting. `MemoryBudget` when the
    /// projected pinned memory would exceed `pin_budget_bytes`: the projection is the payload
    /// of every packet referenced by any clip (including the new clip's pre-roll and
    /// undrained fragments), counted once per packet even when clips share it, as if the
    /// ring had already evicted all of it (worst case). Packets briefly parked beyond a
    /// window's end (see [`ClipCollector`]) are left out: they live only until finalization.
    /// Fragments of finished clips count until [`drain_fragments`](Self::drain_fragments)
    /// takes them, so a caller that never drains eventually gets `MemoryBudget` instead of
    /// growing without bound.
    pub fn request(
        &mut self,
        clip_id: Uuid,
        window: LocalWindow,
        now_local: i64,
    ) -> Result<bool, BufferError> {
        if self.active.iter().any(|(id, _)| *id == clip_id) || self.finished_ids.contains(&clip_id)
        {
            return Ok(false);
        }
        let collecting = self
            .active
            .iter()
            .filter(|(_, c)| c.state() == ClipState::Collecting)
            .count();
        if collecting >= self.max_active {
            return Err(BufferError::TooManyActive);
        }
        let collector = ClipCollector::new(clip_id, window, &self.ring, self.collect, now_local);
        let projected = self.union_bytes(Some(&collector), false, |_| true);
        if projected > self.pin_budget_bytes {
            return Err(BufferError::MemoryBudget);
        }
        self.active.push((clip_id, collector));
        Ok(true)
    }

    /// Extends an active (collecting) clip's end, capped at `start + max_len`.
    /// [`BufferError::UnknownClip`] when the clip is unknown or already finalized.
    pub fn extend(&mut self, clip_id: Uuid, new_end_local: i64) -> Result<(), BufferError> {
        let c = self
            .active
            .iter_mut()
            .find(|(id, c)| *id == clip_id && c.state() == ClipState::Collecting)
            .map(|(_, c)| c)
            .ok_or(BufferError::UnknownClip)?;
        c.extend_end(new_end_local);
        Ok(())
    }

    /// The capture source ended: every collecting clip is finalized now (returned by the next
    /// [`tick`](Self::tick)).
    pub fn source_ended(&mut self, now_local: i64) {
        for (_, c) in &mut self.active {
            c.source_ended(now_local);
        }
    }

    /// Advances every collector and returns the clips finalized since the last call (in
    /// request order). Their remaining fragments move to the queue of
    /// [`drain_fragments`](Self::drain_fragments).
    pub fn tick(&mut self, now_local: i64) -> Vec<FinishedClip> {
        let mut finished = Vec::new();
        let mut i = 0;
        while i < self.active.len() {
            self.active[i].1.tick(now_local);
            if self.active[i].1.state() != ClipState::Done {
                i += 1;
                continue;
            }
            let (id, mut c) = self.active.remove(i);
            if let Some(f) = c.take_finished() {
                finished.push(f);
            }
            self.queued
                .extend(c.drain_ready_fragments().into_iter().map(|f| (id, f)));
            self.remember_finished(id);
        }
        finished
    }

    fn remember_finished(&mut self, id: Uuid) {
        if self.finished_ids.insert(id) {
            self.finished_order.push_back(id);
        }
        while self.finished_order.len() > FINISHED_ID_MEMORY {
            if let Some(old) = self.finished_order.pop_front() {
                self.finished_ids.remove(&old);
            }
        }
    }

    /// Ready fragments of every clip, tagged with the clip id, in order per clip.
    pub fn drain_fragments(&mut self) -> Vec<(Uuid, Fragment)> {
        let mut out = std::mem::take(&mut self.queued);
        for (id, c) in &mut self.active {
            let id = *id;
            out.extend(c.drain_ready_fragments().into_iter().map(|f| (id, f)));
        }
        out
    }

    /// Bytes held by collectors (and undrained fragments) that the ring no longer holds.
    ///
    /// Approximation: a held packet counts as evicted from the ring when its pts is older than
    /// the ring's oldest retained keyframe (all of them when the ring has no video). Packets
    /// shared by several clips are counted once. Cost is O(held packets), so call it at
    /// request time or at a low rate (e.g. for a UI), not per packet.
    pub fn pinned_bytes(&self) -> usize {
        match self.ring.oldest_pts() {
            Some(oldest) => self.union_bytes(None, true, |p| p.pts_ns < oldest),
            None => self.union_bytes(None, true, |_| true),
        }
    }

    /// Payload bytes of the union (by `Arc` identity) of every packet referenced by the active
    /// collectors, the queued fragments and `extra`, restricted to `filter`.
    fn union_bytes(
        &self,
        extra: Option<&ClipCollector>,
        include_parked: bool,
        filter: impl Fn(&SharedPacket) -> bool,
    ) -> usize {
        let mut seen: HashSet<*const Packet> = HashSet::new();
        let mut total = 0usize;
        let mut add = |p: &SharedPacket| {
            if filter(p) && seen.insert(Arc::as_ptr(p)) {
                total = total.saturating_add(p.size());
            }
        };
        for (_, c) in &self.active {
            c.for_each_held(include_parked, &mut add);
        }
        for (_, f) in &self.queued {
            f.packets.iter().for_each(&mut add);
        }
        if let Some(c) = extra {
            c.for_each_held(include_parked, &mut add);
        }
        total
    }

    /// The ring buffer.
    pub fn ring(&self) -> &RingBuffer {
        &self.ring
    }

    /// Ids and current windows of the clips still collecting (for [`should_extend`](crate::should_extend)).
    pub fn active_windows(&self) -> Vec<(Uuid, LocalWindow)> {
        self.active
            .iter()
            .filter(|(_, c)| c.state() == ClipState::Collecting)
            .map(|(id, c)| (*id, c.window()))
            .collect()
    }

    /// Whether a clip is currently collecting.
    pub fn is_active(&self, clip_id: Uuid) -> bool {
        self.active
            .iter()
            .any(|(id, c)| *id == clip_id && c.state() == ClipState::Collecting)
    }

    /// The configured merge tolerance (for [`should_extend`](crate::should_extend)).
    pub fn merge_tolerance_ns(&self) -> i64 {
        self.merge_tolerance_ns
    }

    /// Bytes referenced by the active collectors (shared packets counted once, ring overlap
    /// included).
    pub fn held_bytes(&self) -> usize {
        self.union_bytes(None, true, |_| true)
    }
}
