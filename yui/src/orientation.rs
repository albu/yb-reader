//! Orientation: the single coordinate-space contract between the panel
//! (portrait-native, always 1236x1648 in panel space) and everything the
//! user sees and touches (visual space). Landscape is a device grip, not
//! a rendering mode — every screen authors and hit-tests in visual space,
//! and only two conversions ever happen: pixels out (Painter::flush) and
//! touch in (App's gesture un-rotation), both defined here.
//!
//! Anchors, pinned by the tests below: in `Cw` (device rotated clockwise,
//! USB bezel on the left) the panel's bottom-left corner IS the visual
//! top-left. Before this module existed, the swipe-direction tables in the
//! reader had both rotations' N/S rows transposed against their own point
//! transform — landscape page-turns ran backwards. Deriving the direction
//! remap from the same delta math as the point transform makes that class
//! of bug unrepresentable.

use ybdev::input::{Gesture, SwipeDir};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    /// Panel-native portrait.
    Portrait,
    /// Clockwise grip (split rotation 270; USB bezel on the left).
    Cw,
    /// Counter-clockwise grip (split rotation 90; USB bezel on the right).
    Ccw,
}

impl Orientation {
    /// From the persisted split-config degrees. Unknown values fall back
    /// to portrait (only 0/90/270 are ever stored).
    pub fn from_rotation(deg: u16) -> Orientation {
        match deg {
            90 => Orientation::Ccw,
            270 => Orientation::Cw,
            _ => Orientation::Portrait,
        }
    }

    pub fn to_rotation(self) -> u16 {
        match self {
            Orientation::Portrait => 0,
            Orientation::Cw => 270,
            Orientation::Ccw => 90,
        }
    }

    pub fn is_landscape(self) -> bool {
        !matches!(self, Orientation::Portrait)
    }

    /// The visual-space canvas dimensions for a panel of (w, h).
    pub fn visual_dims(self, panel_w: u32, panel_h: u32) -> (u32, u32) {
        match self {
            Orientation::Portrait => (panel_w, panel_h),
            _ => (panel_h, panel_w),
        }
    }

    /// Panel point → visual point.
    pub fn point_to_visual(self, panel_w: u32, panel_h: u32, x: i32, y: i32) -> (i32, i32) {
        match self {
            Orientation::Portrait => (x, y),
            // vx rides the long (h) axis, flipped; vy rides the short one.
            Orientation::Cw => ((panel_h as i32 - 1) - y, x),
            Orientation::Ccw => (y, (panel_w as i32 - 1) - x),
        }
    }

    /// Visual point → panel point (exactly inverts `point_to_visual`; also
    /// the write coordinate Painter::flush uses per canvas pixel).
    pub fn point_to_panel(self, panel_w: u32, panel_h: u32, vx: i32, vy: i32) -> (i32, i32) {
        match self {
            Orientation::Portrait => (vx, vy),
            Orientation::Cw => (vy, (panel_h as i32 - 1) - vx),
            Orientation::Ccw => ((panel_w as i32 - 1) - vy, vx),
        }
    }

    /// The visual-space direction of a panel-space delta. Derived from the
    /// same transform as the point mapping (never a hand-written table).
    fn delta_to_visual(self, dx: i32, dy: i32) -> (i32, i32) {
        match self {
            Orientation::Portrait => (dx, dy),
            Orientation::Cw => (-dy, dx),
            Orientation::Ccw => (dy, -dx),
        }
    }

    /// Swipe direction remap, classified with input.rs's own rule
    /// (dominant axis, positive = East/South) over the transformed delta.
    pub fn dir_to_visual(self, dir: SwipeDir) -> SwipeDir {
        let (dx, dy) = match dir {
            SwipeDir::East => (1, 0),
            SwipeDir::West => (-1, 0),
            SwipeDir::South => (0, 1),
            SwipeDir::North => (0, -1),
        };
        let (dvx, dvy) = self.delta_to_visual(dx, dy);
        if dvx.abs() >= dvy.abs() {
            if dvx > 0 {
                SwipeDir::East
            } else {
                SwipeDir::West
            }
        } else if dvy > 0 {
            SwipeDir::South
        } else {
            SwipeDir::North
        }
    }

    /// A whole gesture translated into visual space (positions and swipe
    /// direction). Button-style gestures pass through untouched.
    pub fn gesture_to_visual(self, panel_w: u32, panel_h: u32, g: Gesture) -> Gesture {
        let pt = |x: u32, y: u32| {
            let (vx, vy) = self.point_to_visual(panel_w, panel_h, x as i32, y as i32);
            (vx.max(0) as u32, vy.max(0) as u32)
        };
        match g {
            Gesture::Tap { x, y } => {
                let (x, y) = pt(x, y);
                Gesture::Tap { x, y }
            }
            Gesture::LongPress { x, y } => {
                let (x, y) = pt(x, y);
                Gesture::LongPress { x, y }
            }
            Gesture::Drag { x, y } => {
                let (x, y) = pt(x, y);
                Gesture::Drag { x, y }
            }
            Gesture::Swipe { dir, x, y, ex, ey } => {
                let (x, y) = pt(x, y);
                let (ex, ey) = pt(ex, ey);
                Gesture::Swipe {
                    dir: self.dir_to_visual(dir),
                    x,
                    y,
                    ex,
                    ey,
                }
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1236;
    const H: u32 = 1648;

    #[test]
    fn dims_swap_only_in_landscape() {
        assert_eq!(Orientation::Portrait.visual_dims(W, H), (W, H));
        assert_eq!(Orientation::Cw.visual_dims(W, H), (H, W));
        assert_eq!(Orientation::Ccw.visual_dims(W, H), (H, W));
    }

    #[test]
    fn point_maps_round_trip_both_grips() {
        for o in [Orientation::Cw, Orientation::Ccw] {
            for (x, y) in [(0, 0), (0, H - 1), (W - 1, 0), (W - 1, H - 1), (600, 900)] {
                let (vx, vy) = o.point_to_visual(W, H, x as i32, y as i32);
                let (px, py) = o.point_to_panel(W, H, vx, vy);
                assert_eq!((px, py), (x as i32, y as i32), "{o:?} ({x},{y})");
                // Every mapped point stays on the visual canvas.
                let (vw, vh) = o.visual_dims(W, H);
                assert!((0..vw as i32).contains(&vx) && (0..vh as i32).contains(&vy));
            }
        }
    }

    /// The physical anchors: in the Cw grip (USB left) the panel's
    /// bottom-left corner becomes the visual top-left; in Ccw (USB right)
    /// the portrait-left edge rotates down, so the panel's top-RIGHT corner
    /// does.
    #[test]
    fn corner_anchors_match_the_grip() {
        let (vx, vy) = Orientation::Cw.point_to_visual(W, H, 0, (H - 1) as i32);
        assert_eq!((vx, vy), (0, 0));
        let (vx, vy) = Orientation::Ccw.point_to_visual(W, H, (W - 1) as i32, 0);
        assert_eq!((vx, vy), (0, 0));
    }

    /// Pin the direction remap — the table the old reader code had
    /// transposed (its 270° rows were these 90° rows and vice versa).
    #[test]
    fn swipe_direction_remap() {
        use SwipeDir::*;
        assert_eq!(Orientation::Portrait.dir_to_visual(North), North);
        assert_eq!(Orientation::Cw.dir_to_visual(North), East);
        assert_eq!(Orientation::Cw.dir_to_visual(South), West);
        assert_eq!(Orientation::Cw.dir_to_visual(East), South);
        assert_eq!(Orientation::Cw.dir_to_visual(West), North);
        assert_eq!(Orientation::Ccw.dir_to_visual(North), West);
        assert_eq!(Orientation::Ccw.dir_to_visual(South), East);
        assert_eq!(Orientation::Ccw.dir_to_visual(East), North);
        assert_eq!(Orientation::Ccw.dir_to_visual(West), South);
    }

    #[test]
    fn gestures_translate_positions_and_direction() {
        let g = Gesture::Swipe {
            dir: SwipeDir::North,
            x: 0,
            y: 0,
            ex: 10,
            ey: 10,
        };
        let v = Orientation::Cw.gesture_to_visual(W, H, g);
        match v {
            Gesture::Swipe { dir, x, y, ex, ey } => {
                assert_eq!(dir, SwipeDir::East);
                assert_eq!((x, y), ((H - 1) as u32, 0));
                assert_eq!((ex, ey), ((H - 1) as u32 - 10, 10));
            }
            _ => panic!("swipe must stay a swipe"),
        }
        // Buttons are orientation-free.
        assert!(matches!(
            Orientation::Cw.gesture_to_visual(W, H, Gesture::PowerButton),
            Gesture::PowerButton
        ));
        let t = Orientation::Ccw.gesture_to_visual(W, H, Gesture::Tap { x: 5, y: 7 });
        match t {
            Gesture::Tap { x, y } => assert_eq!((x, y), (7, W - 6)),
            _ => panic!("tap must stay a tap"),
        }
    }

    #[test]
    fn rotation_round_trip() {
        for o in [Orientation::Portrait, Orientation::Cw, Orientation::Ccw] {
            assert_eq!(Orientation::from_rotation(o.to_rotation()), o);
        }
        // Garbage degrees degrade to portrait, not panic.
        assert_eq!(Orientation::from_rotation(180), Orientation::Portrait);
    }
}
