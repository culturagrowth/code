//! LocalWindow::compute and should_extend.

mod common;

use common::*;
use duoclip_buffer::{should_extend, BufferError, LocalWindow, UtcToLocal, WindowRequest};

/// local = 7 s + utc * (1 + 1/5000): an offset plus a 200 ppm rate difference.
struct Skewed;
impl UtcToLocal for Skewed {
    fn local_at(&self, utc_ns: i64) -> i64 {
        7 * S + utc_ns + utc_ns / 5_000
    }
}

fn req(h: i64) -> WindowRequest {
    WindowRequest {
        hotkey_utc_ns: h,
        pre_ns: 30 * S,
        post_ns: 10 * S,
        margin_pre_ns: 2 * S,
        margin_post_ns: 2 * S,
        max_len_ns: 180 * S,
    }
}

#[test]
fn compute_with_offset_rate_and_eps() {
    let h = 1_760_000_000 * S; // realistic Unix ns
    let eps = 5 * MS;
    let w = LocalWindow::compute(&req(h), &Skewed, eps).unwrap();
    let m = |u: i64| Skewed.local_at(u);
    assert_eq!(w.start_local_ns, m(h - 32 * S) - eps);
    assert_eq!(w.end_local_ns, m(h + 12 * S) + eps);
    assert_eq!(w.hotkey_local_ns, m(h));
    // 44 s of UTC is 44 s * 1.0002 locally, plus 2 eps.
    assert_eq!(w.len_ns(), 44 * S + 44 * S / 5_000 + 2 * eps);
    // A closure works as a mapping too.
    let f = |u: i64| u - 3 * S;
    let w2 = LocalWindow::compute(&req(h), &f, 0).unwrap();
    assert_eq!(w2.start_local_ns, h - 35 * S);
    assert_eq!(w2.end_local_ns, h + 9 * S);
}

#[test]
fn compute_caps_length_by_moving_the_end() {
    let h = 100 * S;
    let r = WindowRequest {
        max_len_ns: 20 * S,
        ..req(h)
    };
    let w = LocalWindow::compute(&r, &Skewed, MS).unwrap();
    assert_eq!(w.start_local_ns, Skewed.local_at(h - 32 * S) - MS);
    assert_eq!(w.end_local_ns, w.start_local_ns + 20 * S);
    // Negative inputs are treated as zero.
    let r = WindowRequest {
        pre_ns: -5,
        post_ns: -5,
        margin_pre_ns: -1,
        margin_post_ns: -1,
        max_len_ns: 180 * S,
        hotkey_utc_ns: h,
    };
    let w = LocalWindow::compute(&r, &|u: i64| u, -10).unwrap();
    assert_eq!((w.start_local_ns, w.end_local_ns), (h, h));
    let r = WindowRequest {
        max_len_ns: -1,
        ..req(h)
    };
    let w = LocalWindow::compute(&r, &|u: i64| u, 0).unwrap();
    assert_eq!(w.len_ns(), 0);
}

#[test]
fn compute_overflow_is_an_error() {
    let id = |u: i64| u;
    assert_eq!(
        LocalWindow::compute(&req(i64::MAX - S), &id, 0),
        Err(BufferError::Overflow)
    );
    assert_eq!(
        LocalWindow::compute(&req(i64::MIN + S), &id, 0),
        Err(BufferError::Overflow)
    );
    let huge = |_: i64| i64::MAX;
    assert_eq!(
        LocalWindow::compute(&req(0), &huge, 1),
        Err(BufferError::Overflow)
    );
    let tiny = |_: i64| i64::MIN;
    assert_eq!(
        LocalWindow::compute(&req(0), &tiny, 1),
        Err(BufferError::Overflow)
    );
    // An inverted (decreasing) mapping cannot produce a window.
    let inverted = |u: i64| -u;
    assert_eq!(
        LocalWindow::compute(&req(0), &inverted, 0),
        Err(BufferError::Overflow)
    );
    // Start near i64::MIN and end near i64::MAX: the length overflows.
    let spread = |u: i64| if u < 0 { i64::MIN + 1 } else { i64::MAX - 1 };
    assert_eq!(
        LocalWindow::compute(&req(0), &spread, 0),
        Err(BufferError::Overflow)
    );
}

fn w(start: i64, end: i64) -> LocalWindow {
    LocalWindow {
        start_local_ns: start,
        end_local_ns: end,
        hotkey_local_ns: start,
    }
}

#[test]
fn should_extend_truth_table() {
    let active = w(100, 200);
    let tol = 10;
    let cases = [
        // (new window, same requester, expected)
        (w(150, 250), true, true),   // overlaps the end
        (w(50, 150), true, true),    // overlaps the start
        (w(120, 180), true, true),   // inside
        (w(0, 300), true, true),     // contains
        (w(200, 300), true, true),   // touches
        (w(210, 300), true, true),   // hole == tolerance
        (w(211, 300), true, false),  // hole > tolerance
        (w(0, 90), true, true),      // before, hole == tolerance
        (w(0, 89), true, false),     // before, hole > tolerance
        (w(150, 250), false, false), // other requester never extends
        (w(200, 300), false, false),
        (w(500, 600), false, false),
    ];
    for (new, same, expected) in cases {
        assert_eq!(
            should_extend(&active, &new, same, tol),
            expected,
            "{new:?} same={same}"
        );
    }
    // Negative tolerance behaves as zero; extreme values do not overflow.
    assert!(should_extend(&active, &w(150, 250), true, -5));
    assert!(!should_extend(&active, &w(201, 250), true, -5));
    assert!(!should_extend(
        &w(i64::MIN, i64::MIN),
        &w(i64::MAX, i64::MAX),
        true,
        i64::MAX - 1
    ));
    assert!(should_extend(
        &w(i64::MIN, i64::MIN),
        &w(i64::MAX, i64::MAX),
        true,
        i64::MAX
    ));
}
