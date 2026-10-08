//! Rectangles, monitor rotation and the crop plan (desktop coordinates → duplicated texture).
//!
//! Desktop Duplication hands out the monitor image in the monitor's **native (unrotated)**
//! orientation, while window rectangles are in desktop (rotated) coordinates. [`plan_crop`] maps
//! the visible part of the game window into texture coordinates and says how to rotate it back
//! to upright pixels. The mapping is the inverse of the one in Microsoft's
//! `DXGIDesktopDuplication` sample (`DisplayManager::SetDirtyVert`), which converts texture
//! rectangles to desktop rectangles:
//!
//! | rotation | desktop rect from texture rect `t` (`W`/`H` = monitor width/height in desktop space) |
//! |---|---|
//! | 90  | `left = W - t.bottom, top = t.left, right = W - t.top, bottom = t.right` |
//! | 180 | `left = W - t.right, top = H - t.bottom, right = W - t.left, bottom = H - t.top` |
//! | 270 | `left = t.top, top = H - t.right, right = t.bottom, bottom = H - t.left` |

/// A rectangle with exclusive `right`/`bottom` edges (like Win32 `RECT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    /// Left edge (inclusive).
    pub left: i32,
    /// Top edge (inclusive).
    pub top: i32,
    /// Right edge (exclusive).
    pub right: i32,
    /// Bottom edge (exclusive).
    pub bottom: i32,
}

impl Rect {
    /// Builds a rectangle from its edges.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Width, 0 when `right <= left` (never overflows: computed in `i64`).
    pub fn width(&self) -> u32 {
        span(self.left, self.right)
    }

    /// Height, 0 when `bottom <= top`.
    pub fn height(&self) -> u32 {
        span(self.top, self.bottom)
    }

    /// `true` when the rectangle has no area.
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }

    /// Intersection of two rectangles; `Rect::default()` (empty) when they do not overlap.
    pub fn intersect(&self, other: &Rect) -> Rect {
        let r = Rect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        if r.is_empty() {
            Rect::default()
        } else {
            r
        }
    }

    /// `true` when the pixel `(x, y)` is inside.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

fn span(a: i32, b: i32) -> u32 {
    // b - a fits in i64; the result is in 0..=u32::MAX.
    (i64::from(b) - i64::from(a)).max(0) as u32
}

/// Rotation of a monitor as reported by `DXGI_OUTDUPL_DESC.Rotation` / `DXGI_OUTPUT_DESC.Rotation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    /// No rotation (`DXGI_MODE_ROTATION_IDENTITY`, also `UNSPECIFIED`).
    Identity,
    /// `DXGI_MODE_ROTATION_ROTATE90`: the native image must be turned 90° clockwise.
    Rotate90,
    /// `DXGI_MODE_ROTATION_ROTATE180`.
    Rotate180,
    /// `DXGI_MODE_ROTATION_ROTATE270`: the native image must be turned 270° clockwise.
    Rotate270,
}

impl Rotation {
    /// `DXGI_MODE_ROTATION` value → rotation: 0 (unspecified) and 1 → Identity, 2 → 90, 3 → 180,
    /// 4 → 270; anything else → Identity.
    pub fn from_dxgi(value: i32) -> Rotation {
        match value {
            2 => Rotation::Rotate90,
            3 => Rotation::Rotate180,
            4 => Rotation::Rotate270,
            _ => Rotation::Identity,
        }
    }

    /// `true` for 90° and 270° (width and height swap).
    pub fn swaps_axes(self) -> bool {
        matches!(self, Rotation::Rotate90 | Rotation::Rotate270)
    }

    /// Code used by the rotation shader (0 = identity, 1 = 90, 2 = 180, 3 = 270).
    pub fn shader_mode(self) -> u32 {
        match self {
            Rotation::Identity => 0,
            Rotation::Rotate90 => 1,
            Rotation::Rotate180 => 2,
            Rotation::Rotate270 => 3,
        }
    }
}

/// Where to copy from in the duplicated texture and how to make it upright.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropPlan {
    /// Source rectangle in texture coordinates (native orientation), even width and height.
    pub src: Rect,
    /// How to rotate `src` to get upright pixels.
    pub rotation: Rotation,
    /// Width of the upright crop (even).
    pub upright_width: u32,
    /// Height of the upright crop (even).
    pub upright_height: u32,
    /// The desktop rectangle the upright crop shows (window ∩ monitor, trimmed to even size).
    /// Upright pixel `(x, y)` is desktop pixel `(desktop.left + x, desktop.top + y)`.
    pub desktop: Rect,
}

impl CropPlan {
    /// Texture texel (absolute texture coordinates) shown at upright pixel `(x, y)`, or `None`
    /// outside the crop. The rotation shader implements exactly this mapping.
    pub fn source_texel(&self, x: u32, y: u32) -> Option<(u32, u32)> {
        if x >= self.upright_width || y >= self.upright_height {
            return None;
        }
        let (sw, sh) = (self.src.width(), self.src.height());
        let (sx, sy) = match self.rotation {
            Rotation::Identity => (x, y),
            Rotation::Rotate90 => (y, sh.checked_sub(1 + x)?),
            Rotation::Rotate180 => (sw.checked_sub(1 + x)?, sh.checked_sub(1 + y)?),
            Rotation::Rotate270 => (sw.checked_sub(1 + y)?, x),
        };
        // `src` lies inside the texture (non-negative coordinates).
        let tx = u32::try_from(self.src.left).ok()?.checked_add(sx)?;
        let ty = u32::try_from(self.src.top).ok()?.checked_add(sy)?;
        Some((tx, ty))
    }
}

/// Texture texel that shows the desktop pixel `(x, y)` of `monitor` (desktop coordinates, may be
/// negative), for a texture in the monitor's native orientation. `None` outside the monitor.
pub fn desktop_to_texture(x: i32, y: i32, monitor: Rect, rotation: Rotation) -> Option<(u32, u32)> {
    if !monitor.contains(x, y) {
        return None;
    }
    let (w, h) = (i64::from(monitor.width()), i64::from(monitor.height()));
    let dx = i64::from(x) - i64::from(monitor.left);
    let dy = i64::from(y) - i64::from(monitor.top);
    let (tx, ty) = match rotation {
        Rotation::Identity => (dx, dy),
        Rotation::Rotate90 => (dy, w - 1 - dx),
        Rotation::Rotate180 => (w - 1 - dx, h - 1 - dy),
        Rotation::Rotate270 => (h - 1 - dy, dx),
    };
    Some((u32::try_from(tx).ok()?, u32::try_from(ty).ok()?))
}

/// Rotation actually needed for a duplicated texture of `texture_w x texture_h`.
///
/// Defensive: Desktop Duplication documents (and Microsoft's sample relies on) a texture in the
/// native orientation, i.e. for 90°/270° the texture is `monitor_h x monitor_w`. If a driver
/// ever hands out a texture that already has the desktop orientation (`monitor_w x monitor_h`,
/// non-square), rotating it again would show the wrong pixels, so it is treated as Identity.
pub fn effective_rotation(
    rotation: Rotation,
    monitor_w: u32,
    monitor_h: u32,
    texture_w: u32,
    texture_h: u32,
) -> Rotation {
    if rotation.swaps_axes()
        && monitor_w != monitor_h
        && (texture_w, texture_h) == (monitor_w, monitor_h)
    {
        Rotation::Identity
    } else {
        rotation
    }
}

/// Plans the crop of `window` (desktop coordinates) on `monitor` (desktop coordinates) for a
/// duplicated texture of `texture_w x texture_h` (`DXGI_OUTDUPL_DESC.ModeDesc`, native size).
///
/// Returns `None` when the window does not intersect the monitor or the intersection is smaller
/// than 2x2. The visible part is trimmed to even width/height on its right/bottom edges (in
/// desktop space, so the upright crop keeps its top-left corner) and mapped into the texture;
/// the result is clamped to the texture (a texture that does not match the monitor size never
/// yields an out-of-bounds rectangle). See [`effective_rotation`] for an already-upright texture.
pub fn plan_crop(
    window: Rect,
    monitor: Rect,
    rotation: Rotation,
    texture_w: u32,
    texture_h: u32,
) -> Option<CropPlan> {
    let rotation = effective_rotation(
        rotation,
        monitor.width(),
        monitor.height(),
        texture_w,
        texture_h,
    );
    let vis = window.intersect(&monitor);
    let (vw, vh) = (vis.width() & !1, vis.height() & !1);
    if vw < 2 || vh < 2 {
        return None;
    }
    // Desktop rect relative to the monitor, in i64 (no overflow for any i32 input).
    let (mw, mh) = (i64::from(monitor.width()), i64::from(monitor.height()));
    let dl = i64::from(vis.left) - i64::from(monitor.left);
    let dt = i64::from(vis.top) - i64::from(monitor.top);
    let (dr, db) = (dl + i64::from(vw), dt + i64::from(vh));
    let (l, t, r, b) = match rotation {
        Rotation::Identity => (dl, dt, dr, db),
        Rotation::Rotate90 => (dt, mw - dr, db, mw - dl),
        Rotation::Rotate180 => (mw - dr, mh - db, mw - dl, mh - dt),
        Rotation::Rotate270 => (mh - db, dl, mh - dt, dr),
    };
    let (tw, th) = (i64::from(texture_w), i64::from(texture_h));
    let (l, r) = (l.clamp(0, tw), r.clamp(0, tw));
    let (t, b) = (t.clamp(0, th), b.clamp(0, th));
    // Clamping only bites on a texture that does not match the monitor; keep sizes even.
    let w = (r - l).max(0) & !1;
    let h = (b - t).max(0) & !1;
    if w < 2 || h < 2 {
        return None;
    }
    let src = Rect {
        left: i32::try_from(l).ok()?,
        top: i32::try_from(t).ok()?,
        right: i32::try_from(l + w).ok()?,
        bottom: i32::try_from(t + h).ok()?,
    };
    let (uw, uh) = if rotation.swaps_axes() {
        (h as u32, w as u32)
    } else {
        (w as u32, h as u32)
    };
    // The desktop rect shown, consistent with the (possibly clamped) upright size.
    let desktop = Rect {
        left: vis.left,
        top: vis.top,
        right: i32::try_from(i64::from(vis.left) + i64::from(uw)).ok()?,
        bottom: i32::try_from(i64::from(vis.top) + i64::from(uh)).ok()?,
    };
    Some(CropPlan {
        src,
        rotation,
        upright_width: uw,
        upright_height: uh,
        desktop,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Rotation; 4] = [
        Rotation::Identity,
        Rotation::Rotate90,
        Rotation::Rotate180,
        Rotation::Rotate270,
    ];

    /// The test machine's DISPLAY2: 1080x1920 portrait at (-1080, -317), native panel 1920x1080.
    const PORTRAIT: Rect = Rect::new(-1080, -317, 0, 1603);

    fn native(monitor: Rect, rot: Rotation) -> (u32, u32) {
        if rot.swaps_axes() {
            (monitor.height(), monitor.width())
        } else {
            (monitor.width(), monitor.height())
        }
    }

    #[test]
    fn rect_basics() {
        let r = Rect::new(-10, -20, 30, 40);
        assert_eq!((r.width(), r.height()), (40, 60));
        assert!(!r.is_empty());
        assert!(Rect::new(5, 5, 5, 10).is_empty());
        assert!(Rect::new(5, 5, 4, 10).is_empty());
        assert_eq!(Rect::new(5, 5, 4, 3).width(), 0);
        assert_eq!(
            Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX).width(),
            u32::MAX
        );
        assert_eq!(Rect::new(i32::MAX, 0, i32::MIN, 0).width(), 0);
        assert!(r.contains(-10, -20));
        assert!(!r.contains(30, 0));
    }

    #[test]
    fn rect_intersect_edge_cases() {
        let a = Rect::new(0, 0, 100, 100);
        assert_eq!(
            a.intersect(&Rect::new(50, 50, 150, 150)),
            Rect::new(50, 50, 100, 100)
        );
        assert_eq!(
            a.intersect(&Rect::new(10, 10, 20, 20)),
            Rect::new(10, 10, 20, 20)
        );
        // Touching edges: no overlap.
        assert_eq!(a.intersect(&Rect::new(100, 0, 200, 100)), Rect::default());
        assert_eq!(a.intersect(&Rect::new(0, 100, 100, 200)), Rect::default());
        // Disjoint.
        assert!(a.intersect(&Rect::new(-50, -50, -10, -10)).is_empty());
        // Negative coordinates.
        assert_eq!(
            Rect::new(-1080, -317, 0, 1603).intersect(&Rect::new(-500, -400, 200, 0)),
            Rect::new(-500, -317, 0, 0)
        );
        // Extremes never panic.
        let huge = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        assert_eq!(huge.intersect(&a), a);
        assert_eq!(a.intersect(&huge), a);
        let inverted = Rect::new(i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        assert_eq!(inverted.intersect(&huge), Rect::default());
        // Commutative.
        let b = Rect::new(-5, 20, 60, 300);
        assert_eq!(a.intersect(&b), b.intersect(&a));
    }

    #[test]
    fn rotation_from_dxgi() {
        assert_eq!(Rotation::from_dxgi(0), Rotation::Identity);
        assert_eq!(Rotation::from_dxgi(1), Rotation::Identity);
        assert_eq!(Rotation::from_dxgi(2), Rotation::Rotate90);
        assert_eq!(Rotation::from_dxgi(3), Rotation::Rotate180);
        assert_eq!(Rotation::from_dxgi(4), Rotation::Rotate270);
        for garbage in [-1, 5, 100, i32::MIN, i32::MAX] {
            assert_eq!(Rotation::from_dxgi(garbage), Rotation::Identity);
        }
    }

    #[test]
    fn identity_fully_inside() {
        let mon = Rect::new(0, 0, 2560, 1440);
        let p = plan_crop(
            Rect::new(100, 200, 740, 680),
            mon,
            Rotation::Identity,
            2560,
            1440,
        )
        .unwrap();
        assert_eq!(p.src, Rect::new(100, 200, 740, 680));
        assert_eq!((p.upright_width, p.upright_height), (640, 480));
        assert_eq!(p.rotation, Rotation::Identity);
        assert_eq!(p.desktop, Rect::new(100, 200, 740, 680));
    }

    #[test]
    fn identity_partly_outside_and_negative_monitor() {
        // Monitor above the primary (the test machine's DISPLAY3: 2560x1080 at (0,-1080)).
        let mon = Rect::new(0, -1080, 2560, 0);
        let win = Rect::new(-100, -300, 500, 200); // sticks out left and below
        let p = plan_crop(win, mon, Rotation::Identity, 2560, 1080).unwrap();
        assert_eq!(p.desktop, Rect::new(0, -300, 500, 0));
        assert_eq!(p.src, Rect::new(0, 780, 500, 1080));
        // Monitor left of the primary.
        let mon = Rect::new(-1920, 0, 0, 1080);
        let p = plan_crop(
            Rect::new(-1000, 100, -400, 500),
            mon,
            Rotation::Identity,
            1920,
            1080,
        )
        .unwrap();
        assert_eq!(p.src, Rect::new(920, 100, 1520, 500));
    }

    #[test]
    fn outside_or_tiny_is_none() {
        let mon = Rect::new(0, 0, 1920, 1080);
        assert!(plan_crop(
            Rect::new(2000, 0, 2500, 500),
            mon,
            Rotation::Identity,
            1920,
            1080
        )
        .is_none());
        // Minimized windows live at (-32000, -32000).
        assert!(plan_crop(
            Rect::new(-32000, -32000, -31840, -31972),
            mon,
            Rotation::Identity,
            1920,
            1080
        )
        .is_none());
        // 1 px wide / tall intersections.
        assert!(plan_crop(
            Rect::new(1919, 0, 2500, 500),
            mon,
            Rotation::Identity,
            1920,
            1080
        )
        .is_none());
        assert!(plan_crop(
            Rect::new(0, 1079, 50, 1200),
            mon,
            Rotation::Identity,
            1920,
            1080
        )
        .is_none());
        // 2x2 is accepted.
        let p = plan_crop(
            Rect::new(1918, 1078, 2500, 1500),
            mon,
            Rotation::Identity,
            1920,
            1080,
        )
        .unwrap();
        assert_eq!((p.upright_width, p.upright_height), (2, 2));
        // Empty / inverted window.
        assert!(plan_crop(
            Rect::new(10, 10, 10, 10),
            mon,
            Rotation::Identity,
            1920,
            1080
        )
        .is_none());
        assert!(plan_crop(Rect::new(10, 10, 0, 0), mon, Rotation::Identity, 1920, 1080).is_none());
        // Zero-size texture.
        assert!(plan_crop(Rect::new(0, 0, 100, 100), mon, Rotation::Identity, 0, 0).is_none());
    }

    #[test]
    fn odd_sizes_become_even() {
        for rot in ALL {
            // Landscape panel; portrait desktop when rotated by 90/270.
            let mon = if rot.swaps_axes() {
                Rect::new(0, 0, 1080, 1920)
            } else {
                Rect::new(0, 0, 1920, 1080)
            };
            let (tw, th) = native(mon, rot);
            assert_eq!((tw, th), (1920, 1080));
            let p = plan_crop(Rect::new(11, 13, 11 + 301, 13 + 199), mon, rot, tw, th).unwrap();
            assert_eq!(p.upright_width % 2, 0, "{rot:?}");
            assert_eq!(p.upright_height % 2, 0, "{rot:?}");
            assert_eq!((p.upright_width, p.upright_height), (300, 198), "{rot:?}");
            assert_eq!(p.src.width() % 2, 0);
            assert_eq!(p.src.height() % 2, 0);
            // Trimmed on the desktop right/bottom: the top-left corner is kept.
            assert_eq!(p.desktop, Rect::new(11, 13, 311, 211), "{rot:?}");
        }
    }

    #[test]
    fn portrait_known_points_each_rotation() {
        // Desktop corners of the portrait monitor → texel in the 1920x1080 native texture.
        let tl = (PORTRAIT.left, PORTRAIT.top); // (-1080, -317)
        let tr = (PORTRAIT.right - 1, PORTRAIT.top); // (-1, -317)
        let bl = (PORTRAIT.left, PORTRAIT.bottom - 1); // (-1080, 1602)
        let br = (PORTRAIT.right - 1, PORTRAIT.bottom - 1); // (-1, 1602)
        let at = |p: (i32, i32), r| desktop_to_texture(p.0, p.1, PORTRAIT, r).unwrap();
        // 90°: upright = native turned clockwise → desktop top-left is the texture bottom-left.
        assert_eq!(at(tl, Rotation::Rotate90), (0, 1079));
        assert_eq!(at(tr, Rotation::Rotate90), (0, 0));
        assert_eq!(at(bl, Rotation::Rotate90), (1919, 1079));
        assert_eq!(at(br, Rotation::Rotate90), (1919, 0));
        // 270°: desktop top-left is the texture top-right.
        assert_eq!(at(tl, Rotation::Rotate270), (1919, 0));
        assert_eq!(at(tr, Rotation::Rotate270), (1919, 1079));
        assert_eq!(at(bl, Rotation::Rotate270), (0, 0));
        assert_eq!(at(br, Rotation::Rotate270), (0, 1079));
        // 180° on a 1080x1920 portrait (native also 1080x1920).
        assert_eq!(at(tl, Rotation::Rotate180), (1079, 1919));
        assert_eq!(at(br, Rotation::Rotate180), (0, 0));
        assert_eq!(at(tl, Rotation::Identity), (0, 0));
        assert_eq!(at(br, Rotation::Identity), (1079, 1919));
        // A point in the middle: desktop (-580, 283) = monitor-relative (500, 600).
        assert_eq!(at((-580, 283), Rotation::Rotate90), (600, 1080 - 1 - 500));
        assert_eq!(at((-580, 283), Rotation::Rotate270), (1920 - 1 - 600, 500));
        assert_eq!(
            at((-580, 283), Rotation::Rotate180),
            (1079 - 500, 1919 - 600)
        );
        // Outside the monitor.
        assert!(desktop_to_texture(0, 0, PORTRAIT, Rotation::Rotate90).is_none());
        assert!(desktop_to_texture(-1081, 0, PORTRAIT, Rotation::Rotate90).is_none());
    }

    #[test]
    fn portrait_rect_mapping_matches_microsoft_sample() {
        // Window (monitor-relative) left=100 top=200 right=500 bottom=900 on the portrait.
        let win = Rect::new(-1080 + 100, -317 + 200, -1080 + 500, -317 + 900);
        let p = plan_crop(win, PORTRAIT, Rotation::Rotate90, 1920, 1080).unwrap();
        // 90: texture left = d.top, top = W - d.right, right = d.bottom, bottom = W - d.left.
        assert_eq!(p.src, Rect::new(200, 1080 - 500, 900, 1080 - 100));
        assert_eq!((p.upright_width, p.upright_height), (400, 700));
        // Feeding src back through the sample's texture→desktop formulas gives the window.
        let w = 1080;
        let back = Rect::new(w - p.src.bottom, p.src.left, w - p.src.top, p.src.right);
        assert_eq!(back, Rect::new(100, 200, 500, 900));

        let p = plan_crop(win, PORTRAIT, Rotation::Rotate270, 1920, 1080).unwrap();
        let h = 1920;
        assert_eq!(p.src, Rect::new(h - 900, 100, h - 200, 500));
        let back = Rect::new(p.src.top, h - p.src.right, p.src.bottom, h - p.src.left);
        assert_eq!(back, Rect::new(100, 200, 500, 900));

        let p = plan_crop(win, PORTRAIT, Rotation::Rotate180, 1080, 1920).unwrap();
        assert_eq!(
            p.src,
            Rect::new(1080 - 500, 1920 - 900, 1080 - 100, 1920 - 200)
        );
        assert_eq!((p.upright_width, p.upright_height), (400, 700));
    }

    /// Every upright pixel of the crop shows the desktop pixel it should, for every rotation,
    /// on landscape and portrait monitors at negative coordinates.
    #[test]
    fn upright_pixels_map_to_the_right_texels() {
        let monitors = [
            PORTRAIT,
            Rect::new(0, -1080, 2560, 0),
            Rect::new(-37, 11, 293, 211),
        ];
        let windows = [
            Rect::new(-900, -100, -300, 700),
            Rect::new(-1200, -500, 40, 2000),
            Rect::new(10, -1000, 333, -555),
            Rect::new(-37, 11, 293, 211),
            Rect::new(0, 0, 77, 51),
        ];
        for mon in monitors {
            for rot in ALL {
                let (tw, th) = native(mon, rot);
                for win in windows {
                    let Some(p) = plan_crop(win, mon, rot, tw, th) else {
                        continue;
                    };
                    assert!(p.src.right as u32 <= tw && p.src.bottom as u32 <= th);
                    assert!(p.src.left >= 0 && p.src.top >= 0);
                    let mut checked = 0;
                    for y in (0..p.upright_height)
                        .step_by(7)
                        .chain([p.upright_height - 1])
                    {
                        for x in (0..p.upright_width).step_by(5).chain([p.upright_width - 1]) {
                            let dx = p.desktop.left + x as i32;
                            let dy = p.desktop.top + y as i32;
                            let want = desktop_to_texture(dx, dy, mon, rot).unwrap();
                            let got = p.source_texel(x, y).unwrap();
                            assert_eq!(got, want, "{mon:?} {rot:?} {win:?} at ({x},{y})");
                            assert!(p.src.contains(got.0 as i32, got.1 as i32));
                            checked += 1;
                        }
                    }
                    assert!(checked > 0);
                    assert!(p.source_texel(p.upright_width, 0).is_none());
                    assert!(p.source_texel(0, p.upright_height).is_none());
                }
            }
        }
    }

    #[test]
    fn already_upright_texture_is_not_rotated_again() {
        // A driver handing out a 1080x1920 texture for the portrait monitor (desktop orientation).
        let win = Rect::new(-1000, -300, -600, 300);
        let p = plan_crop(win, PORTRAIT, Rotation::Rotate90, 1080, 1920).unwrap();
        assert_eq!(p.rotation, Rotation::Identity);
        assert_eq!(p.src, Rect::new(80, 17, 480, 617));
        // A square monitor cannot be told apart: the reported rotation is kept.
        assert_eq!(
            effective_rotation(Rotation::Rotate90, 1000, 1000, 1000, 1000),
            Rotation::Rotate90
        );
        assert_eq!(
            effective_rotation(Rotation::Rotate270, 1080, 1920, 1920, 1080),
            Rotation::Rotate270
        );
    }

    #[test]
    fn mismatched_texture_is_clamped() {
        let mon = Rect::new(0, 0, 1920, 1080);
        // Texture smaller than the monitor (should not happen): stays inside the texture.
        let p = plan_crop(
            Rect::new(1000, 500, 1900, 1000),
            mon,
            Rotation::Identity,
            1280,
            720,
        )
        .unwrap();
        assert_eq!(p.src, Rect::new(1000, 500, 1280, 720));
        assert_eq!((p.upright_width, p.upright_height), (280, 220));
        assert_eq!(p.desktop, Rect::new(1000, 500, 1280, 720));
        assert!(plan_crop(
            Rect::new(1500, 900, 1900, 1000),
            mon,
            Rotation::Identity,
            1280,
            720
        )
        .is_none());
    }

    #[test]
    fn huge_values_never_panic() {
        let extremes = [
            i32::MIN,
            i32::MIN + 1,
            -32000,
            -1,
            0,
            1,
            32000,
            i32::MAX - 1,
            i32::MAX,
        ];
        let sizes = [0u32, 1, 2, 1080, 1920, u32::MAX];
        for &a in &extremes {
            for &b in &extremes {
                let win = Rect::new(a, b, b, a);
                let win2 = Rect::new(a, a, b, b);
                let mon = Rect::new(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
                for rot in ALL {
                    for &s in &sizes {
                        for w in [win, win2] {
                            if let Some(p) = plan_crop(w, mon, rot, s, s) {
                                assert!(p.upright_width >= 2 && p.upright_height >= 2);
                                assert_eq!(p.upright_width % 2, 0);
                                let _ = p.source_texel(p.upright_width - 1, p.upright_height - 1);
                            }
                            let _ = plan_crop(w, Rect::new(b, a, a, b), rot, s, 7);
                            let _ = desktop_to_texture(a, b, w, rot);
                        }
                    }
                }
            }
        }
    }
}
