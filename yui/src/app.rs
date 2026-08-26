//! App: owns the panel, the input and the screen stack. Runs the event
//! loop, routes the edge gestures, applies screen actions, and enforces
//! the e-ink refresh discipline (an update only ever follows a `Redraw*`
//! action or a stack transition — never a bare tick).
//!
//! App is also one of the two orientation boundaries: raw panel input is
//! un-rotated into visual space here before any screen or edge policy
//! sees it (Painter::flush is the other direction, pixels out).

use ybdev::input::{Gesture, Input};
use ybdev::panel::Panel;

use crate::font::Font;
use crate::orientation::Orientation;
use crate::painter::Painter;
use crate::screen::{Action, RefreshMode, Screen};

pub struct App {
    panel: Panel,
    input: Input,
    font: Font,
    stack: Vec<Box<dyn Screen>>,
    /// Reused visual-space scratch canvas handed to every Painter — one
    /// allocation for the process, both orientations fit (same area).
    canvas: Vec<u8>,
    /// The orientation currently on glass; re-resolved from the stack on
    /// every transition (first Some() from the top wins).
    orientation: Orientation,
    /// Custom screen for the top-edge/two-finger overlay (the default is
    /// yui's FrontlightScreen; apps with a richer control center — clock,
    /// battery, wifi — install their own factory here).
    overlay: Option<Box<dyn Fn() -> Box<dyn Screen>>>,
    /// Called with the suspend duration when the loop detects a
    /// resume-from-suspend (wall-clock gap over the poll budget).
    resume: Option<Box<dyn Fn(std::time::Duration)>>,
    /// Polled every loop iteration: returning true winds the app down.
    /// Lets the app observe async-signal-safe flags (a TERM/INT handoff)
    /// from normal context instead of inside a signal handler.
    quit_check: Option<Box<dyn Fn() -> bool>>,
    /// Called every loop iteration: proof the loop itself is alive.
    /// A device watchdog (outside this process) reads the touch it makes
    /// to tell a hung UI from a suspended one.
    heartbeat: Option<Box<dyn Fn()>>,
    /// Called when the top of the stack enters/leaves the sleep screen.
    /// Lets the app persist "suspending" state so a boot-time audit can
    /// tell a battery death in sleep from an awake hang.
    sleep_state: Option<Box<dyn Fn(bool)>>,
    /// Polled every loop iteration: when true, the factory's screen is
    /// pushed once (it paints a farewell frame and quits on its next
    /// tick) and the app winds down. The device-owned use: USB power
    /// arriving means the stock drive-mode dance wants the disk — the
    /// app bows out gracefully instead of being evicted by the forced
    /// unmount.
    usb_check: Option<Box<dyn Fn() -> bool>>,
    usb_screen: Option<Box<dyn Fn() -> Box<dyn Screen>>>,
    usb_shown: bool,
}

/// A wall-clock gap this much larger than the poll budget means the SoC
/// was suspended under the loop: the poll deadline runs on the monotonic
/// clock, which stops in suspend-to-RAM, while wall time does not (the
/// same property SleepScreen's drain accounting relies on). The slack
/// absorbs scheduler jitter and small clock steps.
pub fn gap_is_suspend(expected: std::time::Duration, wall: std::time::Duration) -> bool {
    wall > expected + std::time::Duration::from_secs(5)
}

/// Pure edge, host-testable: the sleep_state hook fires only on
/// transitions — `Some(true)` entering the sleep screen, `Some(false)`
/// leaving it, `None` when nothing changed.
pub fn sleep_edge(prev: bool, cur: bool) -> Option<bool> {
    (prev != cur).then_some(cur)
}

/// No input for this long → the sleep screen (whose tick suspends).
/// Stock-t1 semantics, enforced by the app rather than powerd: powerd's
/// t1 path depends on the framework yb-reader freezes in takeover, and
/// a wedged wlan stack resets its timer forever via wmt t1TimerReset
/// spam (measured 2026-08-21: 27 min awake untouched at 121mA, 3%
/// battery). Screens with a live reason override via `holds_awake`;
/// USB power is checked at the deadline.
const IDLE_SUSPEND: std::time::Duration = std::time::Duration::from_secs(600);

impl App {
    /// Loads the shared font; panel and input are handed over for good.
    pub fn new(panel: Panel, input: Input) -> Result<App, String> {
        let font = Font::load()?;
        let canvas = vec![0u8; (panel.width * panel.height) as usize];
        Ok(App {
            panel,
            input,
            font,
            stack: Vec::new(),
            canvas,
            orientation: Orientation::Portrait,
            overlay: None,
            resume: None,
            quit_check: None,
            heartbeat: None,
            sleep_state: None,
            usb_check: None,
            usb_screen: None,
            usb_shown: false,
        })
    }

    /// Is the sleep screen currently on top of the stack?
    fn is_sleep_top(&self) -> bool {
        self.stack.last().is_some_and(|s| s.is_sleep())
    }

    /// Replace the edge-gesture overlay with a custom screen factory.
    pub fn with_edge_overlay(mut self, make: Box<dyn Fn() -> Box<dyn Screen>>) -> App {
        self.overlay = Some(make);
        self
    }

    /// Install the resume-from-suspend hook (gap duration handed over).
    pub fn with_resume(mut self, f: Box<dyn Fn(std::time::Duration)>) -> App {
        self.resume = Some(f);
        self
    }

    /// Polled every loop iteration; returning true winds the app down.
    pub fn with_quit_check(mut self, f: Box<dyn Fn() -> bool>) -> App {
        self.quit_check = Some(f);
        self
    }

    /// Called every loop iteration: the app's liveness proof. Whatever
    /// it touches (a tmpfs mtime) is what an external watchdog reads.
    pub fn with_heartbeat(mut self, f: Box<dyn Fn()>) -> App {
        self.heartbeat = Some(f);
        self
    }

    /// Called with `true` when the sleep screen becomes the top of the
    /// stack and `false` when it leaves. Edges only — a sleeping device
    /// must not pay a call per 300 ms tick.
    pub fn with_sleep_state(mut self, f: Box<dyn Fn(bool)>) -> App {
        self.sleep_state = Some(f);
        self
    }

    /// Graceful bow-out: `check` is polled every iteration; the first
    /// true pushes `make`'s screen once — paint a farewell frame, quit
    /// on a short tick — and the loop winds down through normal exit
    /// paths (never mid-paint).
    pub fn with_usb_exit(
        mut self,
        make: Box<dyn Fn() -> Box<dyn Screen>>,
        check: Box<dyn Fn() -> bool>,
    ) -> App {
        self.usb_screen = Some(make);
        self.usb_check = Some(check);
        self
    }

    pub fn dims(&self) -> (u32, u32) {
        (self.panel.width, self.panel.height)
    }

    /// Run until the stack empties or a screen quits. Ends with one final
    /// full refresh (the launcher redraws over whatever we left).
    pub fn run(&mut self, root: Box<dyn Screen>) {
        self.stack.clear();
        if !self.apply(Action::Push(root)) {
            return;
        }
        // Input-idle wall clock — wall, not monotonic: powerd can
        // suspend under the poll and monotonic stops with it, freezing
        // the idle measurement across exactly the sleeps this timer
        // exists to enforce.
        let mut last_input = std::time::SystemTime::now();
        // Sleep-state edge tracking for the sleep_state hook. Starts
        // false: the root screen is never the sleep screen.
        let mut asleep = false;
        while !self.stack.is_empty() {
            // A TERM/INT flag (async-signal-safe store in a signal
            // handler) winds the loop down here, so the app's restore
            // runs from normal context — never inside a signal frame.
            if self.quit_check.as_ref().is_some_and(|f| f()) {
                break;
            }
            // Liveness proof for the external watchdog — same spot as
            // the quit poll, so a loop that can check TERM can touch.
            if let Some(f) = &self.heartbeat {
                f();
            }
            // USB bow-out: the first true pushes the farewell screen
            // (the transition paints it at once); its short tick then
            // winds the loop down through the ordinary Quit path — the
            // exit never lands mid-paint or mid-gesture.
            if !self.usb_shown && self.usb_check.as_ref().is_some_and(|f| f()) {
                self.usb_shown = true;
                if let Some(make) = &self.usb_screen {
                    if !self.apply(Action::Push(make())) {
                        break;
                    }
                }
            }
            let interval = self
                .stack
                .last()
                .map(|s| s.tick_interval())
                .unwrap_or_else(|| std::time::Duration::from_secs(1));
            // Cap the poll wait to 1s so loop checks (USB plug, watchdog heartbeat,
            // quit flag) evaluate promptly even if the screen requested a long tick (e.g. 20s).
            let poll_timeout = interval.min(std::time::Duration::from_secs(1));
            let before = std::time::SystemTime::now();
            // Raw panel input becomes visual-space input here — the only
            // un-rotation touch ever sees; screens and the edge policy
            // below all speak visual coordinates.
            let (pw, ph) = (self.panel.width, self.panel.height);
            let orient = self.orientation;
            let gesture = self
                .input
                .next_gesture(poll_timeout)
                .map(|g| orient.gesture_to_visual(pw, ph, g));
            let wall = before.elapsed().unwrap_or_default();
            // Any gesture is user activity — including the power press
            // that wakes us (updating here, before the swallow below,
            // is what keeps a long sleep from instantly re-sleeping).
            if gesture.is_some() {
                last_input = std::time::SystemTime::now();
            }
            // powerd (or the sleep screen) suspended us under the loop.
            // Hand the app its resume hook and repaint, then still
            // dispatch whatever input woke us — with one exception: the
            // power press that woke the device must not immediately
            // re-sleep it (dispatch would push the sleep screen).
            let mut woke = false;
            if gap_is_suspend(poll_timeout, wall)
                && !self.stack.last().map(|s| s.is_sleep()).unwrap_or(false)
            {
                woke = true;
                if let Some(f) = &self.resume {
                    f(wall);
                }
                if !self.apply(Action::RedrawFull) {
                    break;
                }
            }
            let action = match gesture {
                Some(g) if woke && matches!(g, Gesture::PowerButton) => Action::Keep,
                Some(g) => self.dispatch(g),
                None => self
                    .stack
                    .last_mut()
                    .map(|s| s.on_tick())
                    .unwrap_or(Action::Quit),
            };
            if !self.apply(action) {
                break;
            }
            // Idle sleep: untouched past the deadline, no live session
            // on top, no USB power → the sleep screen, exactly as if
            // the power button had been pressed. Its 300ms tick is
            // what carries us into suspend; the wake path restores.
            // Wi-Fi up counts as a reason to stay awake (reachable ⇒
            // awake; the curtain's Wi-Fi toggle is the opt-out).
            if last_input.elapsed().unwrap_or_default() >= IDLE_SUSPEND
                && !self
                    .stack
                    .last()
                    .map(|s| s.is_sleep() || s.holds_awake())
                    .unwrap_or(false)
                && !ybdev::sysinfo::vbus()
                && !ybdev::sysinfo::wifi_up()
                && !self.apply(Action::Push(Box::new(crate::widgets::SleepScreen::new()))) {
                    break;
                }
            // Sleep-state edges fire here, at iteration end, so both the
            // power-button dispatch and the idle-timer push above are
            // seen. Mid-iteration breaks (apply → false) leave the
            // marker alone; the post-loop reset below covers the exit.
            if let Some(edge) = sleep_edge(asleep, self.is_sleep_top()) {
                asleep = edge;
                if let Some(f) = &self.sleep_state {
                    f(edge);
                }
            }
        }
        // The app is on its way out; by definition it no longer sleeps.
        // Without this, an exit from inside the sleep screen (TERM while
        // suspended) would leave a stale "sleeping" marker that a later
        // boot audit could misread.
        if let Some(f) = &self.sleep_state {
            f(false);
        }
        self.panel.refresh_full();
    }

    /// Edge-gesture policy first (for screens that accept it), then the
    /// screen's own handler. Taps never match the edge patterns, so tap
    /// zones keep their precedence for free.
    fn dispatch(&mut self, g: Gesture) -> Action {
        if matches!(g, Gesture::PowerButton) {
            let is_sleep = self.stack.last().map(|s| s.is_sleep()).unwrap_or(false);
            if is_sleep {
                return Action::Pop;
            } else {
                return Action::Push(Box::new(crate::widgets::SleepScreen::new()));
            }
        }

        let edges = self
            .stack
            .last()
            .map(|s| s.default_edges())
            .unwrap_or(false);
        if edges {
            let (vw, vh) = self
                .orientation
                .visual_dims(self.panel.width, self.panel.height);
            if g.top_edge_swipe() || g.top_edge_swipe_in(vh) {
                // No default overlay: the app registers its control
                // center (with_edge_overlay) or edge gestures aren't
                // special here.
                if let Some(make) = self.overlay.as_ref() {
                    return Action::Push(make());
                }
            } else if (g.corner_back() || g.corner_back_in(vw, vh))
                && self.stack.len() > 1
            {
                // Back only goes somewhere. Corner-back on the ROOT
                // screen is a Pop of the root — which quits the app,
                // and in takeover that is exit-to-stock with no
                // confirm: a stray bottom-corner swipe did exactly
                // that five seconds after boot (field log 2026-08-23:
                // Swipe North from y=1647, graceful exit 42 in the
                // same second, device "hung" while the framework
                // churned). On the root the gesture now falls through
                // to the screen's own handler.
                return Action::Pop;
            }
        }
        match self.stack.last_mut() {
            Some(s) => s.on_gesture(g),
            None => Action::Quit,
        }
    }

    fn apply(&mut self, a: Action) -> bool {
        let (cont, refresh) = transition(&mut self.stack, a);
        // Re-resolve orientation after the stack change; a flip forces the
        // refresh to full (rotating without the flash ghosts badly).
        let flipped = self.resolve_orientation();
        match refresh {
            Some(mode) => self.draw_top(mode, flipped),
            None if flipped => self.draw_top(RefreshMode::Full, false),
            _ => {}
        }
        cont
    }

    /// First screen from the top with an orientation opinion wins; no
    /// opinion anywhere keeps the current one (overlays inherit).
    fn resolve_orientation(&mut self) -> bool {
        match self.stack.iter().rev().find_map(|s| s.orientation()) {
            Some(o) if o != self.orientation => {
                self.orientation = o;
                true
            }
            _ => false,
        }
    }

    fn draw_top(&mut self, mode: RefreshMode, flipped: bool) {
        let App {
            panel,
            font,
            stack,
            canvas,
            orientation,
            ..
        } = self;
        let w = panel.width;
        let h = panel.height;
        let stride = panel.stride as usize;
        {
            let mut p = Painter::new(panel.buf_mut(), w, h, stride, *orientation, canvas, font);
            if let Some(s) = stack.last_mut() {
                s.draw(&mut p);
            }
            p.flush();
        }
        if flipped {
            panel.refresh_full();
        } else {
            match mode {
                RefreshMode::Fast => panel.refresh_fast(0, 0, w, h),
                RefreshMode::Partial => panel.refresh_partial(0, 0, w, h),
                RefreshMode::Full => panel.refresh_full(),
            }
        }
    }
}

/// Pure stack machine behind Action — separately testable with fake
/// screens. Returns (continue?, refresh mode: Some(..) means draw the
/// new top now, with that panel refresh).
fn transition(stack: &mut Vec<Box<dyn Screen>>, a: Action) -> (bool, Option<RefreshMode>) {
    match a {
        Action::Keep => (true, None),
        Action::Redraw => (true, Some(RefreshMode::Partial)),
        Action::RedrawFast => (true, Some(RefreshMode::Fast)),
        Action::RedrawFull => (true, Some(RefreshMode::Full)),
        Action::Push(s) => {
            stack.push(s);
            let a = stack.last_mut().unwrap().on_enter();
            transition(stack, a)
        }
        Action::Pop => {
            if stack.len() <= 1 {
                // Popping the root quits — but still release its resources.
                if let Some(mut popped) = stack.pop() {
                    popped.on_leave();
                }
                return (false, None);
            }
            if let Some(mut popped) = stack.pop() {
                popped.on_leave();
            }
            let a = stack.last_mut().unwrap().on_resume();
            transition(stack, a)
        }
        Action::PopN(n) => {
            // Unwind n overlays down to the screen that must handle the
            // result. Clamped to the overlays: never quits by popping the
            // root (an explicit Pop from the root screen means that).
            let n = n.min(stack.len().saturating_sub(1));
            for _ in 0..n {
                if let Some(mut popped) = stack.pop() {
                    popped.on_leave();
                }
            }
            let a = stack.last_mut().unwrap().on_resume();
            transition(stack, a)
        }
        Action::Quit => (false, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use crate::painter::Rect;

    #[test]
    fn gap_detection_boundaries() {
        let e = Duration::from_secs(20);
        // Normal poll: no.
        assert!(!gap_is_suspend(e, Duration::from_secs(2)));
        // Exactly at the slack boundary: no (strictly greater only).
        assert!(!gap_is_suspend(e, e + Duration::from_secs(5)));
        // Just past it: yes.
        assert!(gap_is_suspend(e, e + Duration::from_secs(6)));
        // A suspend-scale gap: yes.
        assert!(gap_is_suspend(e, Duration::from_secs(900)));
    }

    #[test]
    fn sleep_edges_fire_only_on_transitions() {
        assert_eq!(sleep_edge(false, true), Some(true)); // entering sleep
        assert_eq!(sleep_edge(true, false), Some(false)); // waking
        assert_eq!(sleep_edge(false, false), None); // awake stays quiet
        assert_eq!(sleep_edge(true, true), None); // asleep stays quiet
    }

    /// Screen that records its lifecycle calls into a shared log and never
    /// draws (there is no buffer here — draw is exercised by the Painter
    /// tests and on-device).
    struct Fake {
        name: &'static str,
        log: Rc<RefCell<Vec<String>>>,
        resume: Action,
    }

    impl Screen for Fake {
        fn draw(&mut self, _p: &mut Painter) {}
        fn on_enter(&mut self) -> Action {
            self.log.borrow_mut().push(format!("{}:enter", self.name));
            Action::RedrawFull
        }
        fn on_leave(&mut self) {
            self.log.borrow_mut().push(format!("{}:leave", self.name));
        }
        fn on_resume(&mut self) -> Action {
            self.log.borrow_mut().push(format!("{}:resume", self.name));
            std::mem::replace(&mut self.resume, Action::Keep)
        }
    }

    fn fake(name: &'static str, log: &Rc<RefCell<Vec<String>>>) -> Box<dyn Screen> {
        Box::new(Fake {
            name,
            log: log.clone(),
            resume: Action::RedrawFull,
        })
    }

    #[test]
    fn push_runs_on_enter_and_requests_full_draw() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![];
        let (cont, redraw) = transition(&mut stack, Action::Push(fake("a", &log)));
        assert!(cont);
        assert_eq!(redraw, Some(RefreshMode::Full));
        assert_eq!(*log.borrow(), vec!["a:enter"]);
    }

    #[test]
    fn redraw_fast_requests_a2_refresh() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![fake("a", &log)];
        let (cont, redraw) = transition(&mut stack, Action::RedrawFast);
        assert!(cont);
        assert_eq!(redraw, Some(RefreshMode::Fast));
        assert!(log.borrow().is_empty()); // no lifecycle calls — pure repaint
    }

    #[test]
    fn popn_unwinds_multiple_overlays_and_resumes_the_base() {
        // A TOC picked on top of a scrubber must unwind both dialogs and
        // resume the reader beneath, not the scrubber in the middle.
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![
            fake("reader", &log),
            fake("scrubber", &log),
            fake("toc", &log),
        ];
        log.borrow_mut().clear();

        let (cont, redraw) = transition(&mut stack, Action::PopN(2));
        assert!(cont);
        assert_eq!(redraw, Some(RefreshMode::Full));
        assert_eq!(
            *log.borrow(),
            vec!["toc:leave", "scrubber:leave", "reader:resume"]
        );
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn popn_never_pops_the_root() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![fake("base", &log), fake("over", &log)];
        log.borrow_mut().clear();

        let (cont, _) = transition(&mut stack, Action::PopN(9));
        assert!(cont);
        assert_eq!(stack.len(), 1);
        assert_eq!(*log.borrow(), vec!["over:leave", "base:resume"]);
    }

    #[test]
    fn pop_runs_leave_then_resume_and_applies_resume_action() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![fake("base", &log), fake("over", &log)];
        log.borrow_mut().clear();

        let (cont, redraw) = transition(&mut stack, Action::Pop);
        assert!(cont);
        // base's default-impl resume: RedrawFull.
        assert_eq!(redraw, Some(RefreshMode::Full));
        assert_eq!(*log.borrow(), vec!["over:leave", "base:resume"]);
        assert_eq!(stack.len(), 1);
    }

    #[test]
    fn resume_redraw_partial_is_honored() {
        let log = Rc::new(RefCell::new(vec![]));
        // A screen holding a pixel cache re-presents it with Redraw:
        let base: Box<dyn Screen> = Box::new(Fake {
            name: "base",
            log: log.clone(),
            resume: Action::Redraw,
        });
        let mut stack: Vec<Box<dyn Screen>> = vec![base, fake("over", &log)];
        let (_, redraw) = transition(&mut stack, Action::Pop);
        assert_eq!(redraw, Some(RefreshMode::Partial));
    }

    #[test]
    fn keep_never_draws() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![fake("a", &log)];
        let (cont, redraw) = transition(&mut stack, Action::Keep);
        assert!(cont);
        assert_eq!(redraw, None);
        assert!(log.borrow().is_empty());
    }

    #[test]
    fn popping_the_root_quits() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![fake("root", &log)];
        let (cont, _) = transition(&mut stack, Action::Pop);
        assert!(!cont);
        // Root leave still fires (release resources on exit).
        assert_eq!(*log.borrow(), vec!["root:leave"]);
    }

    #[test]
    fn push_enter_can_push_an_overlay_itself() {
        // A screen whose setup fails pushes a message on entry.
        struct Failing {
            log: Rc<RefCell<Vec<String>>>,
        }
        impl Screen for Failing {
            fn draw(&mut self, _p: &mut Painter) {}
            fn on_enter(&mut self) -> Action {
                self.log.borrow_mut().push("fail:enter".into());
                Action::Push(Box::new(Fake {
                    name: "msg",
                    log: self.log.clone(),
                    resume: Action::RedrawFull,
                }))
            }
        }
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack: Vec<Box<dyn Screen>> = vec![];
        let (cont, redraw) = transition(
            &mut stack,
            Action::Push(Box::new(Failing { log: log.clone() })),
        );
        assert!(cont);
        assert_eq!(redraw, Some(RefreshMode::Full)); // msg's default on_enter
        assert_eq!(*log.borrow(), vec!["fail:enter", "msg:enter"]);
        assert_eq!(stack.len(), 2);
    }

    #[test]
    fn rect_only_used_for_compile_guard() {
        // Rect is exercised here so the import stays honest if tests change.
        let r = Rect::new(0, 0, 1, 1);
        assert!(r.contains(0, 0));
    }
}
