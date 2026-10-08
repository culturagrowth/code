//! Portable ownership bookkeeping of the encoder's input surfaces (B1-E1).
//!
//! The hardware H.264 MFT is asynchronous: it may keep an input sample (and therefore the
//! texture it wraps) after `ProcessInput` returns, and `METransformNeedInput` credits say
//! nothing about *which* sample was released. So a texture may only be written again once the
//! MFT released the sample that wraps it. [`SlotPool`] tracks that: each slot (one GPU texture
//! on Windows, see `input_pool.rs`) is either free or in flight; a slot becomes free only
//! through [`SlotPool::release`], which the Windows side calls from the `IMFTrackedSample`
//! release callback. The pool grows on demand up to a bound; when every slot is in flight the
//! producer has to wait (backpressure) instead of overwriting.

#![forbid(unsafe_code)]
// Only the Windows encoder uses it; the unit tests run everywhere.
#![cfg_attr(not(windows), allow(dead_code))]

/// What [`SlotPool::acquire`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Acquire {
    /// Slot `i` was free and is now in flight: its texture may be overwritten.
    Reuse(usize),
    /// No slot is free but the pool may grow: create the resource, then call
    /// [`SlotPool::push_in_flight`].
    Grow,
    /// Every slot is in flight and the bound is reached: wait for a release.
    Exhausted,
}

/// Free / in-flight state of up to `max` slots.
#[derive(Debug)]
pub(crate) struct SlotPool {
    in_flight: Vec<bool>,
    max: usize,
    /// Round-robin start of the free-slot search (spreads the reuse over the slots).
    next: usize,
}

impl SlotPool {
    /// Empty pool that may grow to `max` slots (`max` is raised to at least 1).
    pub(crate) fn new(max: usize) -> Self {
        Self {
            in_flight: Vec::new(),
            max: max.max(1),
            next: 0,
        }
    }

    /// Takes a free slot (marking it in flight), or says whether the pool may grow.
    pub(crate) fn acquire(&mut self) -> Acquire {
        let n = self.in_flight.len();
        for k in 0..n {
            let i = (self.next + k) % n;
            if !self.in_flight[i] {
                self.in_flight[i] = true;
                self.next = (i + 1) % n;
                return Acquire::Reuse(i);
            }
        }
        if n < self.max {
            Acquire::Grow
        } else {
            Acquire::Exhausted
        }
    }

    /// Adds a new slot, already in flight (after [`Acquire::Grow`]); returns its index.
    /// Ignored (returns `None`) when the bound is reached.
    pub(crate) fn push_in_flight(&mut self) -> Option<usize> {
        if self.in_flight.len() >= self.max {
            return None;
        }
        self.in_flight.push(true);
        Some(self.in_flight.len() - 1)
    }

    /// Marks slot `i` free. Returns `false` (and changes nothing) for an unknown or already
    /// free slot, so a duplicate notification can never free a slot that was re-acquired.
    pub(crate) fn release(&mut self, i: usize) -> bool {
        match self.in_flight.get_mut(i) {
            Some(busy @ true) => {
                *busy = false;
                true
            }
            _ => false,
        }
    }

    /// `true` when [`SlotPool::acquire`] would not return [`Acquire::Exhausted`].
    pub(crate) fn has_room(&self) -> bool {
        self.in_flight.len() < self.max || self.in_flight.iter().any(|b| !b)
    }

    /// Number of slots created so far.
    pub(crate) fn len(&self) -> usize {
        self.in_flight.len()
    }

    /// Number of slots in flight (tests).
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.in_flight.iter().filter(|b| **b).count()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Producer side of the model: acquires a slot (growing it when allowed) and "writes"
    /// `frame` into it. Returns `None` when exhausted.
    fn produce(pool: &mut SlotPool, pixels: &mut Vec<u64>, frame: u64) -> Option<usize> {
        let i = match pool.acquire() {
            Acquire::Reuse(i) => i,
            Acquire::Grow => {
                let i = pool.push_in_flight().expect("grow");
                pixels.push(0);
                i
            }
            Acquire::Exhausted => return None,
        };
        pixels[i] = frame;
        Some(i)
    }

    /// The scenario of B1-E1: a consumer (the async MFT) retains MORE than three inputs before
    /// releasing the first. No slot it still holds may be overwritten, and each frame it reads
    /// on release is the frame it was given.
    #[test]
    fn consumer_retaining_more_than_three_inputs_never_sees_overwritten_frames() {
        for retain in 0..=7usize {
            let mut pool = SlotPool::new(8);
            let mut pixels: Vec<u64> = Vec::new();
            let mut held: VecDeque<(usize, u64)> = VecDeque::new();
            for frame in 1..=200u64 {
                let slot = produce(&mut pool, &mut pixels, frame).expect("never exhausted");
                assert!(
                    held.iter().all(|(s, _)| *s != slot),
                    "retain {retain}: slot {slot} handed out while still held"
                );
                held.push_back((slot, frame));
                while held.len() > retain {
                    let (s, f) = held.pop_front().unwrap();
                    assert_eq!(pixels[s], f, "retain {retain}: frame {f} overwritten");
                    assert!(pool.release(s));
                }
            }
            // The pool grows to exactly what the consumer retains (+1 being written).
            assert_eq!(pool.len(), retain + 1, "retain {retain}");
            assert_eq!(pool.in_flight(), retain);
            for (s, f) in held {
                assert_eq!(pixels[s], f);
            }
        }
    }

    /// The model above catches the original bug: a fixed cyclic ring of three textures,
    /// overwritten regardless of ownership, corrupts a frame as soon as four are retained.
    #[test]
    fn naive_cyclic_ring_of_three_is_corrupted_by_a_four_frame_consumer() {
        let mut ring = [0u64; 3];
        let mut held: VecDeque<(usize, u64)> = VecDeque::new();
        let mut corrupted = false;
        for frame in 1..=20u64 {
            let slot = (frame as usize) % ring.len();
            ring[slot] = frame;
            held.push_back((slot, frame));
            while held.len() > 4 {
                let (s, f) = held.pop_front().unwrap();
                corrupted |= ring[s] != f;
            }
        }
        assert!(corrupted);
    }

    #[test]
    fn bound_reached_means_wait_then_the_released_slot_is_reused() {
        let mut pool = SlotPool::new(4);
        let mut pixels = Vec::new();
        let slots: Vec<usize> = (1..=4)
            .map(|f| produce(&mut pool, &mut pixels, f).unwrap())
            .collect();
        assert_eq!(slots, vec![0, 1, 2, 3]);
        assert!(!pool.has_room());
        assert_eq!(pool.acquire(), Acquire::Exhausted);
        assert_eq!(produce(&mut pool, &mut pixels, 5), None);
        // Releasing a slot other than the oldest: exactly that one comes back.
        assert!(pool.release(2));
        assert!(pool.has_room());
        assert_eq!(pool.acquire(), Acquire::Reuse(2));
        assert_eq!(pool.acquire(), Acquire::Exhausted);
        assert_eq!(pool.len(), 4);
        assert_eq!(pool.push_in_flight(), None, "never grows past the bound");
    }

    #[test]
    fn duplicate_or_unknown_releases_are_ignored() {
        let mut pool = SlotPool::new(2);
        assert_eq!(pool.acquire(), Acquire::Grow);
        assert_eq!(pool.push_in_flight(), Some(0));
        assert!(!pool.release(5));
        assert!(pool.release(0));
        assert!(!pool.release(0), "second release of the same slot");
        assert_eq!(pool.acquire(), Acquire::Reuse(0));
        assert_eq!(pool.in_flight(), 1);
        assert_eq!(SlotPool::new(0).max, 1);
    }

    #[test]
    fn reuse_is_round_robin_over_free_slots() {
        let mut pool = SlotPool::new(3);
        for _ in 0..3 {
            assert_eq!(pool.acquire(), Acquire::Grow);
            pool.push_in_flight();
        }
        for i in 0..3 {
            pool.release(i);
        }
        let order: Vec<Acquire> = (0..3).map(|_| pool.acquire()).collect();
        assert_eq!(
            order,
            vec![Acquire::Reuse(0), Acquire::Reuse(1), Acquire::Reuse(2)]
        );
    }
}
