//! Mapping a global (UTC) clip request onto a window of the local monotonic clock.

use serde::{Deserialize, Serialize};

use crate::BufferError;

/// Maps a global UTC timestamp to the local monotonic clock.
///
/// In the app this is the clock crate's mapping frozen at request time (docs 8.5). This crate
/// deliberately does not depend on the clock crate; any `Fn(i64) -> i64` works too.
pub trait UtcToLocal {
    /// Local monotonic ns corresponding to `utc_ns`.
    fn local_at(&self, utc_ns: i64) -> i64;
}

impl<F: Fn(i64) -> i64> UtcToLocal for F {
    fn local_at(&self, utc_ns: i64) -> i64 {
        self(utc_ns)
    }
}

/// A clip request expressed on the global clock (docs 6.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowRequest {
    /// Hotkey press, global UTC ns.
    pub hotkey_utc_ns: i64,
    /// Seconds before the press, in ns (default 30 s).
    pub pre_ns: i64,
    /// Seconds after the press, in ns (default 10 s).
    pub post_ns: i64,
    /// Hidden margin before (default 2 s).
    pub margin_pre_ns: i64,
    /// Hidden margin after (default 2 s).
    pub margin_post_ns: i64,
    /// Maximum window length (default 180 s).
    pub max_len_ns: i64,
}

/// A clip window on the local monotonic clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalWindow {
    /// Inclusive start.
    pub start_local_ns: i64,
    /// Exclusive end.
    pub end_local_ns: i64,
    /// The hotkey press mapped to the local clock.
    pub hotkey_local_ns: i64,
}

impl LocalWindow {
    /// `start = local(hotkey - pre - margin_pre) - eps`, `end = local(hotkey + post + margin_post) + eps`
    /// (the length is capped at `max_len_ns` by moving END earlier). `eps` = sync uncertainty bound.
    ///
    /// Negative `pre`, `post`, margins, `eps` and `max_len` are treated as zero. Returns
    /// [`BufferError::Overflow`] when any step overflows `i64` or the mapping produces an
    /// inverted window (`end < start`).
    pub fn compute(
        req: &WindowRequest,
        map: &dyn UtcToLocal,
        eps_ns: i64,
    ) -> Result<Self, BufferError> {
        let pre = req.pre_ns.max(0);
        let post = req.post_ns.max(0);
        let margin_pre = req.margin_pre_ns.max(0);
        let margin_post = req.margin_post_ns.max(0);
        let eps = eps_ns.max(0);
        let max_len = req.max_len_ns.max(0);
        let start_utc = req
            .hotkey_utc_ns
            .checked_sub(pre)
            .and_then(|v| v.checked_sub(margin_pre))
            .ok_or(BufferError::Overflow)?;
        let end_utc = req
            .hotkey_utc_ns
            .checked_add(post)
            .and_then(|v| v.checked_add(margin_post))
            .ok_or(BufferError::Overflow)?;
        let start = map
            .local_at(start_utc)
            .checked_sub(eps)
            .ok_or(BufferError::Overflow)?;
        let end = map
            .local_at(end_utc)
            .checked_add(eps)
            .ok_or(BufferError::Overflow)?;
        let hotkey = map.local_at(req.hotkey_utc_ns);
        if end < start {
            return Err(BufferError::Overflow);
        }
        let len = end.checked_sub(start).ok_or(BufferError::Overflow)?;
        let end = if len > max_len {
            start.checked_add(max_len).ok_or(BufferError::Overflow)?
        } else {
            end
        };
        Ok(Self {
            start_local_ns: start,
            end_local_ns: end,
            hotkey_local_ns: hotkey,
        })
    }

    /// `end - start`, saturating, never negative.
    pub fn len_ns(&self) -> i64 {
        self.end_local_ns.saturating_sub(self.start_local_ns).max(0)
    }
}

/// Docs 6.6: same requester, new window overlaps or is within tolerance of the active one => extend instead of a new clip.
///
/// The windows are compared as intervals: their distance is 0 when they overlap or touch, else
/// the size of the hole between them (in either order). A negative tolerance counts as 0, so
/// overlapping windows of the same requester always extend.
pub fn should_extend(
    active: &LocalWindow,
    new: &LocalWindow,
    same_requester: bool,
    tolerance_ns: i64,
) -> bool {
    if !same_requester {
        return false;
    }
    let after = new.start_local_ns.saturating_sub(active.end_local_ns);
    let before = active.start_local_ns.saturating_sub(new.end_local_ns);
    let distance = after.max(before).max(0);
    distance <= tolerance_ns.max(0)
}
