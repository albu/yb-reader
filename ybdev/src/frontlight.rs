//! Kindle frontlight via /dev/frontlight ioctls (PW5 kernel header,
//! <linux/frontlight.h>, magic 'L').
//!
//! The PW5 has two independent LED strings (white + amber) exposed as
//! two backlight nodes. Driven raw, both knobs feel like "more light /
//! less light" — each string alone at 30% reads as off. The user-facing
//! model every control surface shares instead: `brightness` = total
//! light, `warmth` = how much of that light is amber (white and amber
//! crossfade, so hue moves at constant luminance). [`channels_for`] /
//! [`state_from`] are the only place the two models meet.

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicI32, Ordering};

use crate::mtk::Ioctl;

const FL_MAGIC: u8 = b'L';
const DEV: &str = "/dev/frontlight";

const fn fl_ioctl(nr: u32, write: bool) -> Ioctl {
    let dir = if write { 1u32 } else { 2u32 };
    let v = ((dir as u64) << 30)
        | ((4u64) << 16)
        | ((FL_MAGIC as u64) << 8)
        | (nr as u64);
    v as Ioctl
}

const FL_IOCTL_SET_INTENSITY: Ioctl = fl_ioctl(0x01, true);
const FL_IOCTL_GET_INTENSITY: Ioctl = fl_ioctl(0x02, false);
const FL_IOCTL_GET_RANGE_MAX: Ioctl = fl_ioctl(0x03, false);
const FL_IOCTL_SET_INTENSITY_AMBER_1: Ioctl = fl_ioctl(0x05, true);
const FL_IOCTL_GET_INTENSITY_AMBER_1: Ioctl = fl_ioctl(0x06, false);
const FL_IOCTL_GET_RANGE_MAX_AMBER_1: Ioctl = fl_ioctl(0x07, false);
const FL_IOCTL_SET_INTENSITY_AMBER_2: Ioctl = fl_ioctl(0x09, true);
const FL_IOCTL_GET_INTENSITY_AMBER_2: Ioctl = fl_ioctl(0x0a, false);
const FL_IOCTL_GET_RANGE_MAX_AMBER_2: Ioctl = fl_ioctl(0x0b, false);

pub struct Frontlight {
    f: File,
}

// Last levels WE set. powerd restores its own idea of the frontlight
// around suspend/wake; the resume hook reapplies these so our levels
// survive. -1 = never set in this process.
static LAST_BRIGHT: AtomicI32 = AtomicI32::new(-1);
static LAST_TONE: AtomicI32 = AtomicI32::new(-1);

pub fn last_bright() -> i32 {
    LAST_BRIGHT.load(Ordering::SeqCst)
}

pub fn last_tone() -> i32 {
    LAST_TONE.load(Ordering::SeqCst)
}

// The warm channel on the PW5 (FW 5.19): the /dev/frontlight amber ioctls
// report max 0, but the FP9966 driver exposes both LED channels as standard
// backlight devices. Per KOReader's device table the mapping is
const BL1_BRIGHTNESS: &str = "/sys/class/backlight/fp9966-bl1/brightness";
const BL1_MAX: &str = "/sys/class/backlight/fp9966-bl1/max_brightness";
const BL0_BRIGHTNESS: &str = "/sys/class/backlight/fp9966-bl0/brightness";
const BL0_MAX: &str = "/sys/class/backlight/fp9966-bl0/max_brightness";

fn read_i32(path: &str) -> Option<i32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

impl Frontlight {
    pub fn open() -> Result<Frontlight, String> {
        let f = File::options()
            .read(true)
            .write(true)
            .open(DEV)
            .map_err(|e| format!("open {}: {}", DEV, e))?;
        Ok(Frontlight { f })
    }

    fn ioctl_val(&self, req: Ioctl) -> std::io::Result<i32> {
        let mut v: i32 = 0;
        let rv = unsafe { libc::ioctl(self.f.as_raw_fd(), req, &mut v) };
        if rv < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(v)
        }
    }

    fn ioctl_set(&self, req: Ioctl, v: i32) -> std::io::Result<()> {
        let rv = unsafe { libc::ioctl(self.f.as_raw_fd(), req, &v) };
        if rv < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn max(&self) -> i32 {
        read_i32(BL1_MAX).unwrap_or_else(|| self.ioctl_val(FL_IOCTL_GET_RANGE_MAX).unwrap_or(24))
    }
    pub fn get(&self) -> i32 {
        read_i32(BL1_BRIGHTNESS).unwrap_or_else(|| self.ioctl_val(FL_IOCTL_GET_INTENSITY).unwrap_or(0))
    }
    pub fn set(&self, v: i32) {
        LAST_BRIGHT.store(v, Ordering::SeqCst);
        if std::path::Path::new(BL1_BRIGHTNESS).exists() {
            let _ = std::fs::write(BL1_BRIGHTNESS, format!("{}\n", v));
        } else {
            let _ = self.ioctl_set(FL_IOCTL_SET_INTENSITY, v);
        }
    }


    pub fn amber1_max(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_RANGE_MAX_AMBER_1).unwrap_or(0)
    }
    pub fn amber1_get(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_INTENSITY_AMBER_1).unwrap_or(0)
    }
    pub fn amber1_set(&self, v: i32) {
        let _ = self.ioctl_set(FL_IOCTL_SET_INTENSITY_AMBER_1, v);
    }

    pub fn amber2_max(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_RANGE_MAX_AMBER_2).unwrap_or(0)
    }
    pub fn amber2_get(&self) -> i32 {
        self.ioctl_val(FL_IOCTL_GET_INTENSITY_AMBER_2).unwrap_or(0)
    }
    pub fn amber2_set(&self, v: i32) {
        let _ = self.ioctl_set(FL_IOCTL_SET_INTENSITY_AMBER_2, v);
    }

    /// Warm/amber channel via sysfs (the ioctls don't serve it on FW 5.19).
    pub fn tone_max(&self) -> i32 {
        read_i32(BL0_MAX).unwrap_or(0)
    }
    pub fn tone_get(&self) -> i32 {
        read_i32(BL0_BRIGHTNESS).unwrap_or(0)
    }
    pub fn tone_set(&self, v: i32) {
        LAST_TONE.store(v, Ordering::SeqCst);
        let _ = std::fs::write(BL0_BRIGHTNESS, format!("{}\n", v));
    }

    /// (brightness, warmth) currently on the hardware — reads the glass,
    /// not our last write, so a powerd-stomped mixed state still reports
    /// sanely.
    pub fn levels(&self) -> (f32, f32) {
        state_from(self.get(), self.tone_get(), self.max(), self.tone_max())
    }

    /// Apply the crossfade model: brightness scales both strings, warmth
    /// swaps white for amber. Returns false (and writes nothing) when the
    /// hardware already holds these levels.
    pub fn apply_levels(&mut self, b: f32, w: f32) -> bool {
        let (white, amber) = channels_for(b, w, self.max(), self.tone_max());
        if white == self.get() && amber == self.tone_get() {
            return false;
        }
        self.set(white);
        self.tone_set(amber);
        true
    }
}

/// Register→light is steeply convex on the FP9966: measured on-device,
/// register 1024 (half) on either string draws ~1/16 the LED current of
/// 2047 — a power law with exponent ≈ 4 — while the two strings are
/// independent and additive (both-max draws the exact sum of the two
/// singles). The crossfade must therefore trade off in LIGHT units and
/// map back through the curve's inverse, or the mid-track collapses to
/// ~6% light (register-half^4) and reads as almost off.
const LIGHT_GAMMA: f32 = 4.0;

/// One-tap composite light points, (label, brightness, warmth) in slider
/// space — register-linear brightness, gamma-crossfaded warmth, the same
/// space every control speaks. Values stay above the ~30%-register
/// "feels off" floor.
pub const PRESETS: [(&str, f32, f32); 3] = [
    ("Day", 0.9, 0.0),
    ("Warm", 0.6, 0.8),
    ("Night", 0.35, 1.0),
];

/// The preset matching the given levels, if any (for highlighting the
/// tile the light is currently on).
pub fn nearest_preset(b: f32, w: f32) -> Option<usize> {
    PRESETS
        .iter()
        .position(|&(_, pb, pw)| (b - pb).abs() < 0.03 && (w - pw).abs() < 0.03)
}

/// (brightness, warmth) -> raw channel levels. `b` is register-linear
/// (what a slider over one string would show — the stock feel); the
/// warmth split happens in light units, so total light is constant
/// along the whole warmth track. Warmth is ignored when there is no
/// amber string (its slider never shows, but a stray value must not
/// dim the white channel).
pub fn channels_for(b: f32, warmth: f32, white_max: i32, amber_max: i32) -> (i32, i32) {
    let b = b.clamp(0.0, 1.0);
    let w = if amber_max <= 0 { 0.0 } else { warmth.clamp(0.0, 1.0) };
    let mix = |f: f32| f.powf(1.0 / LIGHT_GAMMA);
    let white = (b * mix(1.0 - w) * white_max.max(0) as f32).round() as i32;
    let amber = (b * mix(w) * amber_max.max(0) as f32).round() as i32;
    (white, amber)
}

/// Raw channel levels -> (brightness, warmth). Registers convert to
/// light through the gamma curve; brightness is the total light
/// expressed back in register-linear units, warmth is amber's share of
/// the light — the inverse of [`channels_for`] and a sane reading of
/// any mixed state the hardware happens to hold.
pub fn state_from(white: i32, amber: i32, white_max: i32, amber_max: i32) -> (f32, f32) {
    let light = |r: i32, m: i32| (r.max(0) as f32 / m.max(1) as f32).clamp(0.0, 1.0).powf(LIGHT_GAMMA);
    let lw = light(white, white_max);
    let la = light(amber, amber_max);
    let b = (lw + la).min(1.0).powf(1.0 / LIGHT_GAMMA);
    let w = if lw + la > f32::EPSILON {
        (la / (lw + la)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (b, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossfade_endpoints_and_midpoints() {
        // Full white / full amber at max brightness.
        assert_eq!(channels_for(1.0, 0.0, 100, 200), (100, 0));
        assert_eq!(channels_for(1.0, 1.0, 100, 200), (0, 200));
        // Midpoint: 0.5^(1/4) ≈ 0.841 of each string's register — NOT
        // register-half, which the gamma curve turns into ~6% light.
        assert_eq!(channels_for(1.0, 0.5, 100, 200), (84, 168));
        // Brightness scales both together (register-linear, stock feel).
        assert_eq!(channels_for(0.5, 0.5, 100, 200), (42, 84));
        assert_eq!(channels_for(0.25, 0.5, 100, 200), (21, 42));
        // Off is off.
        assert_eq!(channels_for(0.0, 0.7, 100, 200), (0, 0));
        // No amber string: warmth must not dim white.
        assert_eq!(channels_for(0.8, 0.9, 100, 0), (80, 0));
    }

    #[test]
    fn light_is_constant_along_warmth() {
        for k in 0..=20 {
            let w = k as f32 / 20.0;
            let (white, amber) = channels_for(1.0, w, 2047, 2047);
            // Convert registers back to light: the two strings' light
            // must sum to full drive (gamma-compensated invariance).
            let light = (white as f32 / 2047.0).powf(LIGHT_GAMMA)
                + (amber as f32 / 2047.0).powf(LIGHT_GAMMA);
            assert!((light - 1.0).abs() < 0.01, "w={w} light={light}");
        }
    }

    #[test]
    fn preset_tiles_are_composite_points_in_range() {
        // Each preset is one (brightness, warmth) pair, both axes in
        // range, brightness above the register floor where light "feels
        // off" on this panel.
        for (name, b, w) in PRESETS {
            assert!((0.0..=1.0).contains(&b) && (0.0..=1.0).contains(&w), "{name}");
            assert!(b >= 0.3, "{name} too dim for a tile");
        }
        assert_eq!(nearest_preset(0.9, 0.0), Some(0));
        assert_eq!(nearest_preset(0.62, 0.79), Some(1));
        assert_eq!(nearest_preset(0.33, 1.0), Some(2));
        assert_eq!(nearest_preset(0.5, 0.4), None);
    }

    #[test]
    fn state_from_inverts_and_reads_mixed_states() {
        for b in [0.0f32, 0.3, 0.7, 1.0] {
            for w in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                let (white, amber) = channels_for(b, w, 2047, 2047);
                let (rb, rw) = state_from(white, amber, 2047, 2047);
                // At b=0 warmth is unobservable — both strings are dark,
                // so the inverse can only report the origin. Everywhere
                // else the round trip must hold.
                let expected = if b == 0.0 { (0.0, 0.0) } else { (b, w) };
                assert!(
                    (rb - expected.0).abs() < 0.002 && (rw - expected.1).abs() < 0.002,
                    "b={b} w={w} white={white} amber={amber} -> ({rb}, {rw})"
                );
            }
        }
        // A raw both-max state (e.g. powerd's default) reads as full
        // brightness at neutral warmth instead of 200%.
        let (b, w) = state_from(2047, 2047, 2047, 2047);
        assert!((b - 1.0).abs() < 1e-6 && (w - 0.5).abs() < 1e-6);
        // Dead string reads as warm-neutral darkness, not NaN.
        assert_eq!(state_from(0, 0, 2047, 2047), (0.0, 0.0));
    }
}
