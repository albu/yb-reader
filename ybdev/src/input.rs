//! Touch input over evdev for MTK Kindles. The device is discovered
//! dynamically (KOReader's PW5 has no hardcoded path either): we parse
//! /proc/bus/input/devices and pick the device with ABS_MT_POSITION_X.
//! Gestures synthesized: Tap(x,y), Swipe(direction), TwoFingerTap.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwipeDir {
    East,
    West,
    North,
    South,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    Tap {
        x: u32,
        y: u32,
    },
    LongPress {
        x: u32,
        y: u32,
    },
    /// Continuous position after a fired long-press (finger stayed down
    /// and moved). Emitted in ~12px steps; screens snap to word/slider
    /// granularity and redraw only on index change, so the stream is
    /// naturally throttled. No tap/swipe is synthesized on release.
    Drag {
        x: u32,
        y: u32,
    },
    /// Direction plus the swipe's START and END points: handlers can bind
    /// edge gestures (top-edge swipe-down = brightness, bottom-right
    /// swipe-up = back) and turn bar-drags into value adjustments.
    Swipe {
        dir: SwipeDir,
        x: u32,
        y: u32,
        ex: u32,
        ey: u32,
    },
    TwoFingerTap,
    PowerButton,
}

/// Edge zones on the 1236x1648 panel: the top strip (where the stock
/// framework's status banner used to live) and the Boox-style back corner.
const TOP_EDGE_Y: u32 = 130;
const CORNER_X: u32 = 890;
const CORNER_Y: u32 = 1350;

impl Gesture {
    /// A downward swipe that starts in the top strip → brightness overlay
    /// (the stock panel's job, since the framework itself is frozen).
    pub fn top_edge_swipe(&self) -> bool {
        matches!(self, Gesture::Swipe { dir: SwipeDir::South, y, .. } if *y < TOP_EDGE_Y)
    }

    pub fn top_edge_swipe_in(&self, h: u32) -> bool {
        let max_y = (h * 8 / 100).max(TOP_EDGE_Y);
        matches!(self, Gesture::Swipe { dir: SwipeDir::South, y, .. } if *y < max_y)
    }

    /// An upward swipe that starts in the bottom-right corner → back.
    pub fn corner_back(&self) -> bool {
        matches!(
            self,
            Gesture::Swipe { dir: SwipeDir::North, x, y, .. }
                if *x > CORNER_X && *y > CORNER_Y
        )
    }

    pub fn corner_back_in(&self, w: u32, h: u32) -> bool {
        let min_x = w - (w * 28 / 100);
        let min_y = h - (h * 18 / 100);
        matches!(
            self,
            Gesture::Swipe { dir: SwipeDir::North, x, y, .. }
                if *x > min_x && *y > min_y
        )
    }

    /// An upward swipe that starts in the bottom-left corner → the
    /// mirror screen's quick settings (the left analog of
    /// corner_back_in's 28% × 18% corner).
    pub fn corner_settings_in(&self, w: u32, h: u32) -> bool {
        let max_x = w * 28 / 100;
        let min_y = h - (h * 18 / 100);
        matches!(
            self,
            Gesture::Swipe { dir: SwipeDir::North, x, y, .. }
                if *x < max_x && *y > min_y
        )
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InputEvent {
    time: libc::timeval,
    type_: u16,
    code: u16,
    value: i32,
}

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const KEY_POWER: u16 = 116;
const BTN_TOUCH: u16 = 0x14a;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

const SWIPE_MIN_DIST: i32 = 40;
const TWO_FINGER_MAX_DIST: i32 = 50;
/// Step between Drag emissions, in px — small enough to feel continuous
/// at word granularity, large enough to jitter-proof a steady hold.
const DRAG_STEP: i32 = 12;

const EV_ABS_BIT: u64 = 1 << 3;
const ABS_MT_POSITION_X_BIT: u64 = 1 << 53;
const ABS_MT_POSITION_Y_BIT: u64 = 1 << 54;

#[derive(Clone, Copy, Debug)]
struct Touch {
    x: i32,
    y: i32,
    down_x: i32,
    down_y: i32,
    x_set: bool,
    y_set: bool,
    _down: Instant,
    long_press_fired: bool,
    /// Anchor of the fired long-press / last Drag emission — the reference
    /// point for the next Drag step. None until the long-press fires.
    drag_from: Option<(i32, i32)>,
}

pub struct Input {
    f: File,
    pwr_f: Option<File>,
    slots: HashMap<i32, Touch>,
    released: Vec<Touch>,
    current_slot: i32,
    legacy_pos: Option<(i32, i32)>,
    legacy_down: bool,
    two_finger_seen: bool,
}

fn read_to_string_lossy(path: &str) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Find the power key event device (e.g. bd71828-pwrkey / event0).
pub fn discover_pwrkey() -> Option<String> {
    let proc = read_to_string_lossy("/proc/bus/input/devices");
    for block in proc.split("\n\n") {
        if block.to_ascii_lowercase().contains("pwrkey")
            || block.to_ascii_lowercase().contains("power")
        {
            for line in block.lines() {
                if let Some(rest) = line.trim().strip_prefix("H:") {
                    let ev_node = rest
                        .strip_prefix("Handlers=")
                        .unwrap_or(rest)
                        .split_whitespace()
                        .find(|w| w.starts_with("event"))
                        .map(|ev| format!("/dev/input/{}", ev));
                    if let Some(p) = ev_node {
                        if std::path::Path::new(&p).exists() {
                            return Some(p);
                        }
                    }
                }
            }
        }
    }
    if std::path::Path::new("/dev/input/event0").exists() {
        Some("/dev/input/event0".to_string())
    } else {
        None
    }
}

/// Find the touchscreen event device.
///
/// /proc/bus/input/devices prints capability masks as hex words, *high
/// word first* (word 0 = axes 32-63). On the PW5 the panel is `pt_mt`
/// (EV=f, ABS=e618000 0 => ABS_MT_SLOT/POSITION_X/POSITION_Y/TRACKING_ID/
/// PRESSURE), while event0 is the bd71828 power button — so "first event
/// device" is wrong. We score devices by multitouch axes, then ABS, then a
/// touch-ish name, requiring EV_ABS.
pub fn discover() -> Option<String> {
    if std::path::Path::new("/dev/input/touch").exists() {
        return Some("/dev/input/touch".to_string());
    }

    let proc = read_to_string_lossy("/proc/bus/input/devices");
    let mut best: Option<(u32, String)> = None;
    for block in proc.split("\n\n") {
        if let Some((score, path)) = score_block(block) {
            if std::path::Path::new(&path).exists()
                && best.as_ref().map(|(s, _)| score > *s).unwrap_or(true)
            {
                best = Some((score, path));
            }
        }
    }

    if let Some((_, path)) = best {
        return Some(path);
    }

    // Fallback: try each event device in order.
    for n in 1..8 {
        let p = format!("/dev/input/event{}", n);
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    None
}

/// Score one /proc/bus/input/devices block: returns (score, event path) if
/// it looks like a touchscreen, else None.
fn score_block(block: &str) -> Option<(u32, String)> {
    let mut handlers = String::new();
    let mut name = String::new();
    let mut ev_words: Vec<u64> = Vec::new();
    let mut abs_words: Vec<u64> = Vec::new();
    for line in block.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("N:") {
            name = rest.trim().to_string();
        } else if let Some(rest) = t.strip_prefix("H:") {
            // Raw line is "H: Handlers=event1 perfmgr": after the "H:"
            // prefix there's a leading space — trim it off.
            handlers = rest.trim().to_string();
        } else if let Some(rest) = t.strip_prefix("B: EV=") {
            ev_words = parse_hex_words(rest);
        } else if let Some(rest) = t.strip_prefix("B: ABS=") {
            abs_words = parse_hex_words(rest);
        }
    }
    let ev = mask_from_words(&ev_words);
    let abs = mask_from_words(&abs_words);
    if ev & EV_ABS_BIT == 0 || abs == 0 {
        return None;
    }
    let mt_x = abs & ABS_MT_POSITION_X_BIT != 0;
    let mt_y = abs & ABS_MT_POSITION_Y_BIT != 0;
    let name_lc = name.to_ascii_lowercase();
    let name_hint = name_lc.contains("touch")
        || name_lc.contains("pt_")
        || name_lc.contains("tp")
        || name_lc.contains("mtk");
    let score =
        (mt_x as u32) * 8 + (mt_y as u32) * 4 + (mt_x || mt_y) as u32 * 2 + name_hint as u32;
    let ev_node = handlers
        .strip_prefix("Handlers=")
        .unwrap_or(&handlers)
        .split_whitespace()
        .find(|w| w.starts_with("event"))
        .map(|ev| format!("/dev/input/{}", ev));
    ev_node.map(|path| (score, path))
}

/// Parse space-separated hex words ("e618000 0").
fn parse_hex_words(s: &str) -> Vec<u64> {
    s.split_whitespace()
        .filter_map(|w| u64::from_str_radix(w.trim_start_matches("0x"), 16).ok())
        .collect()
}

/// Reconstruct the full mask. /proc/bus/input/devices prints words from
/// the highest (axes 32-63) down; a single word is the low one.
fn mask_from_words(words: &[u64]) -> u64 {
    match words.len() {
        0 => 0,
        1 => words[0] & 0xffff_ffff,
        _ => (words[0] << 32) | (words[1] & 0xffff_ffff),
    }
}

#[cfg(test)]
mod tests {
    use super::{mask_from_words, parse_hex_words, score_block, Gesture, SwipeDir};

    fn north(x: u32, y: u32) -> Gesture {
        Gesture::Swipe {
            dir: SwipeDir::North,
            x,
            y,
            ex: x,
            ey: y.saturating_sub(80),
        }
    }

    #[test]
    fn corner_settings_region_is_the_bottom_left_analog_of_back() {
        // 1236×1648 panel: back owns x > 890-ish, settings x < 345-ish,
        // both only in the bottom 18%.
        let (w, h) = (1236u32, 1648u32);
        assert!(north(30, 1600).corner_settings_in(w, h));
        assert!(north(340, 1550).corner_settings_in(w, h));
        // Too high, or the bottom-right corner which is corner_back's.
        assert!(!north(30, 1000).corner_settings_in(w, h));
        assert!(!north(1000, 1600).corner_settings_in(w, h));
        assert!(north(1000, 1600).corner_back_in(w, h));
        // The two corners must not overlap in the middle.
        for x in [400u32, 600, 830] {
            assert!(!north(x, 1600).corner_settings_in(w, h));
            assert!(!north(x, 1600).corner_back_in(w, h));
        }
        // Only upward swipes open it.
        assert!(!Gesture::Swipe {
            dir: SwipeDir::East,
            x: 30,
            y: 1600,
            ex: 400,
            ey: 1600
        }
        .corner_settings_in(w, h));
        assert!(!Gesture::Tap { x: 30, y: 1600 }.corner_settings_in(w, h));
    }

    #[test]
    fn decodes_pt_mt_abs_mask_high_word_first() {
        // The PW5 panel: "B: ABS=e618000 0" — word 0 covers axes 32-63.
        let words = parse_hex_words("e618000 0");
        assert_eq!(words, vec![0x0e618000, 0]);
        let mask = mask_from_words(&words);
        // ABS_MT_SLOT(47), TOUCH_MAJOR(48), POSITION_X(53), POSITION_Y(54),
        // TRACKING_ID(57), PRESSURE(58), DISTANCE(59).
        assert_ne!(mask & (1 << 53), 0, "ABS_MT_POSITION_X must be set");
        assert_ne!(mask & (1 << 54), 0, "ABS_MT_POSITION_Y must be set");
        assert_ne!(mask & (1 << 47), 0, "ABS_MT_SLOT must be set");
        assert_eq!(mask & (1 << 0), 0, "ABS_X must not be set");
    }

    #[test]
    fn single_word_is_low_mask() {
        // "B: EV=f" prints as one low word.
        let mask = mask_from_words(&parse_hex_words("f"));
        assert_eq!(mask, 0xf);
        let ev_abs = (mask >> 3) & 1;
        assert_eq!(ev_abs, 1);
    }

    #[test]
    fn scores_the_real_pw5_input_devices() {
        // Copied verbatim from the on-device /proc/bus/input/devices.
        let pwrkey = "\
I: Bus=0019 Vendor=0001 Product=0001 Version=0100
N: Name=\"bd71828-pwrkey\"
P: Phys=gpio-keys/input0
H: Handlers=event0 perfmgr
B: PROP=0
B: EV=3
B: KEY=100000 0 0 0
";
        let panel = "\
I: Bus=0000 Vendor=0000 Product=0000 Version=0000
N: Name=\"pt_mt\"
P: Phys=2-0024/input0
H: Handlers=event1 perfmgr
B: PROP=2
B: EV=f
B: KEY=0
B: REL=0
B: ABS=e618000 0
";
        assert!(score_block(pwrkey).is_none(), "pwrkey must be rejected");
        let (score, _path) = score_block(panel).expect("pt_mt must be selected");
        assert!(score >= 8, "MT axes must dominate the score");
    }
}

impl Input {
    pub fn open(path: &str) -> Result<Input, String> {
        let f = OpenOptions::new()
            .read(true)
            .write(false)
            .open(path)
            .map_err(|e| format!("open {}: {}", path, e))?;

        // Exclusive grab. The framework's Xorg also holds this device open
        // and keeps reading touches even while awesome is SIGSTOPed — it
        // queues the whole session as X events, and when the framework
        // resumes on our exit, awesome REPLAYS every touch onto the
        // homescreen. With the grab, the kernel routes events only to
        // this fd; it is released automatically when the fd closes (even
        // on crash). KOReader grabs the same way.
        const EVIOCGRAB: libc::c_int = 0x40044590;
        // `as _`: the request type is c_int on 32-bit targets and c_ulong
        // on the 64-bit host — let inference pick per target.
        let rv = unsafe { libc::ioctl(f.as_raw_fd(), EVIOCGRAB as _, &1i32) };
        crate::log::plog(&format!(
            "touch grab on {}: {}",
            path,
            if rv == 0 { "ok" } else { "FAILED" }
        ));

        let pwr_f = discover_pwrkey().and_then(|p| {
            let file = OpenOptions::new().read(true).write(false).open(&p).ok()?;
            let _ = unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB as _, &1i32) };
            crate::log::plog(&format!("pwrkey opened on {}", p));
            Some(file)
        });

        Ok(Input {
            f,
            pwr_f,
            slots: HashMap::new(),
            released: Vec::new(),
            current_slot: 0,
            legacy_pos: None,
            legacy_down: false,
            two_finger_seen: false,
        })
    }

    /// Block up to `timeout` for the next gesture. Returns None on timeout
    /// so callers can run periodic work (keep-alive pings).
    pub fn next_gesture(&mut self, timeout: Duration) -> Option<Gesture> {
        let fd = self.f.as_raw_fd();
        let pwr_fd = self.pwr_f.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);
        let deadline = Instant::now() + timeout;

        loop {
            // Check active hold for immediate long-press trigger while finger is held down!
            // Only allow single-touch holds (suppressed during multi-touch gestures).
            if !self.two_finger_seen && self.slots.len() == 1 {
                for t in self.slots.values_mut() {
                    if !t.long_press_fired && t.x_set && t.y_set {
                        let dx = (t.x - t.down_x).abs();
                        let dy = (t.y - t.down_y).abs();
                        if dx <= 25 && dy <= 25 && t._down.elapsed() >= Duration::from_millis(360) {
                            t.long_press_fired = true;
                            t.drag_from = Some((t.x, t.y));
                            let g = Gesture::LongPress {
                                x: t.x.max(0) as u32,
                                y: t.y.max(0) as u32,
                            };
                            crate::log::plog(&format!("input: {:?}", g));
                            return Some(g);
                        }
                    }
                    // The finger stayed down after the long-press and moved:
                    // stream its position as Drag steps. (No plog — a word-
                    // snapped drag emits dozens per selection.)
                    if t.long_press_fired && t.x_set && t.y_set {
                        if let Some((lx, ly)) = t.drag_from {
                            if (t.x - lx).abs() >= DRAG_STEP || (t.y - ly).abs() >= DRAG_STEP {
                                t.drag_from = Some((t.x, t.y));
                                return Some(Gesture::Drag {
                                    x: t.x.max(0) as u32,
                                    y: t.y.max(0) as u32,
                                });
                            }
                        }
                    }
                }
            }

            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                return None;
            }
            let mut pfds = [
                libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pwr_fd,
                    events: if pwr_fd >= 0 { libc::POLLIN } else { 0 },
                    revents: 0,
                },
            ];
            let n_fds = if pwr_fd >= 0 { 2 } else { 1 };
            let ms = remain.as_millis().min(40) as libc::c_int;
            let rv = unsafe { libc::poll(pfds.as_mut_ptr(), n_fds, ms) };
            if rv < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return None;
            }
            if rv == 0 {
                continue;
            }

            // Check power key events first
            if pwr_fd >= 0 && (pfds[1].revents & libc::POLLIN) != 0 {
                if let Some(g) = self.drain_pwr_events() {
                    crate::log::plog(&format!("input: {:?}", g));
                    return Some(g);
                }
            }

            if (pfds[0].revents & libc::POLLIN) != 0 {
                if let Some(g) = self.drain_events() {
                    crate::log::plog(&format!("input: {:?}", g));
                    return Some(g);
                }
            }
        }
    }

    /// Read the complete input events out of a byte buffer. evdev delivers
    /// whole events, but `[u8; N]` is only byte-aligned, so casting to
    /// `*const InputEvent` (align_of 4 on armv7) is UB; copying each event
    /// with `read_unaligned` keeps the count exact and the alignment sound.
    fn events_from(buf: &[u8]) -> impl Iterator<Item = InputEvent> + '_ {
        let sz = std::mem::size_of::<InputEvent>();
        let count = buf.len() / sz;
        (0..count).map(move |i| unsafe {
            buf.as_ptr().add(i * sz).cast::<InputEvent>().read_unaligned()
        })
    }

    fn drain_pwr_events(&mut self) -> Option<Gesture> {
        let Some(pwr) = &mut self.pwr_f else {
            return None;
        };
        let mut buf = [0u8; 256];
        let mut pwr_seen = false;
        let n = pwr.read(&mut buf).unwrap_or(0);
        for ev in Self::events_from(&buf[..n]) {
            if ev.type_ == EV_KEY && ev.code == KEY_POWER && ev.value == 1 {
                pwr_seen = true;
            }
        }
        if pwr_seen {
            Some(Gesture::PowerButton)
        } else {
            None
        }
    }

    fn drain_events(&mut self) -> Option<Gesture> {
        let mut buf = [0u8; 512];
        let mut result = None;
        loop {
            let n = match self.f.read(&mut buf) {
                Ok(0) => return result,
                Ok(n) => n,
                Err(_) => return result,
            };
            for ev in Self::events_from(&buf[..n]) {
                if let Some(g) = self.handle_event(ev) {
                    result = Some(g);
                }
            }
            if result.is_some() {
                return result;
            }
            if n < buf.len() {
                return result;
            }
        }
    }

    fn handle_event(&mut self, ev: InputEvent) -> Option<Gesture> {
        match ev.type_ {
            EV_SYN if ev.code == SYN_REPORT => self.finalize_frame(),
            EV_ABS => self.handle_abs(ev.code, ev.value),
            EV_KEY if ev.code == BTN_TOUCH => {
                self.legacy_down = ev.value != 0;
                None
            }
            _ => None,
        }
    }

    fn handle_abs(&mut self, code: u16, value: i32) -> Option<Gesture> {
        match code {
            ABS_MT_SLOT => {
                self.current_slot = value;
                None
            }
            ABS_MT_TRACKING_ID => {
                if value < 0 {
                    if let Some(t) = self.slots.remove(&self.current_slot) {
                        self.released.push(t);
                    }
                } else {
                    let now = Instant::now();
                    if !self.slots.is_empty() {
                        self.two_finger_seen = true;
                    }
                    self.slots.insert(
                        self.current_slot,
                        Touch {
                            x: 0,
                            y: 0,
                            down_x: 0,
                            down_y: 0,
                            x_set: false,
                            y_set: false,
                            _down: now,
                            long_press_fired: false,
                            drag_from: None,
                        },
                    );
                }
                None
            }
            ABS_MT_POSITION_X => {
                if let Some(t) = self.slots.get_mut(&self.current_slot) {
                    if !t.x_set {
                        t.down_x = value;
                        t.x_set = true;
                    }
                    t.x = value;
                }
                None
            }
            ABS_MT_POSITION_Y => {
                if let Some(t) = self.slots.get_mut(&self.current_slot) {
                    if !t.y_set {
                        t.down_y = value;
                        t.y_set = true;
                    }
                    t.y = value;
                }
                None
            }
            ABS_X => {
                let p = self.legacy_pos.get_or_insert((0, 0));
                p.0 = value;
                None
            }
            ABS_Y => {
                let p = self.legacy_pos.get_or_insert((0, 0));
                p.1 = value;
                None
            }
            _ => None,
        }
    }

    fn finalize_frame(&mut self) -> Option<Gesture> {
        if self.two_finger_seen {
            if self.slots.is_empty() {
                self.two_finger_seen = false;
                let small = self.released.iter().all(|t| {
                    (t.x - t.down_x).abs() <= TWO_FINGER_MAX_DIST
                        && (t.y - t.down_y).abs() <= TWO_FINGER_MAX_DIST
                });
                self.released.clear();
                if small {
                    return Some(Gesture::TwoFingerTap);
                }
            }
            return None;
        }

        if let Some(t) = self.released.pop() {
            self.released.clear();
            if t.long_press_fired {
                return None; // Finger lifted after long-press already triggered while holding!
            }
            // A contact that never reported both axes is not a gesture:
            // emitting its default 0s once landed a mid-page tap in the
            // top-bar bookmark zone (2026-08-22, Tap{x:1078,y:0} with the
            // finger at y≈1100).
            if !t.x_set || !t.y_set {
                return None;
            }
            let dx = t.x - t.down_x;
            let dy = t.y - t.down_y;
            if dx.abs() > SWIPE_MIN_DIST || dy.abs() > SWIPE_MIN_DIST {
                let dir = if dx.abs() > dy.abs() {
                    if dx > 0 {
                        SwipeDir::East
                    } else {
                        SwipeDir::West
                    }
                } else if dy > 0 {
                    SwipeDir::South
                } else {
                    SwipeDir::North
                };
                return Some(Gesture::Swipe {
                    dir,
                    x: t.down_x.max(0) as u32,
                    y: t.down_y.max(0) as u32,
                    ex: t.x.max(0) as u32,
                    ey: t.y.max(0) as u32,
                });
            }
            // Down position, not last: the lift-off frame can carry a
            // junk coordinate (the same y→0 glitch), and for hit
            // targets where the finger LANDED is the truth anyway.
            return Some(Gesture::Tap {
                x: t.down_x.max(0) as u32,
                y: t.down_y.max(0) as u32,
            });
        }

        // Legacy single-touch protocol (BTN_TOUCH + ABS_X/Y): on release.
        if !self.legacy_down {
            if let Some((x, y)) = self.legacy_pos.take() {
                return Some(Gesture::Tap {
                    x: x.max(0) as u32,
                    y: y.max(0) as u32,
                });
            }
        }
        None
    }
}
