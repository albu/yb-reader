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
use crate::frontlight::FrontlightScreen;
use crate::orientation::Orientation;
use crate::painter::Painter;
use crate::screen::{Action, Screen};

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
}

/// A wall-clock gap this much larger than the poll budget means the SoC
/// was suspended under the loop: the poll deadline runs on the monotonic
/// clock, which stops in suspend-to-RAM, while wall time does not (the
/// same property SleepScreen's drain accounting relies on). The slack
/// absorbs scheduler jitter and small clock steps.
pub fn gap_is_suspend(expected: std::time::Duration, wall: std::time::Duration) -> bool {
    wall > expected + std::time::Duration::from_secs(5)
}

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
        })
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
        while !self.stack.is_empty() {
            let interval = self
                .stack
                .last()
                .map(|s| s.tick_interval())
                .unwrap_or_else(|| std::time::Duration::from_secs(1));
            let before = std::time::SystemTime::now();
            // Raw panel input becomes visual-space input here — the only
            // un-rotation touch ever sees; screens and the edge policy
            // below all speak visual coordinates.
            let (pw, ph) = (self.panel.width, self.panel.height);
            let orient = self.orientation;
            let gesture = self
                .input
                .next_gesture(interval)
                .map(|g| orient.gesture_to_visual(pw, ph, g));
            let wall = before.elapsed().unwrap_or_default();
            // powerd (or the sleep screen) suspended us under the loop.
            // Hand the app its resume hook and repaint, then still
            // dispatch whatever input woke us — with one exception: the
            // power press that woke the device must not immediately
            // re-sleep it (dispatch would push the sleep screen).
            let mut woke = false;
            if gap_is_suspend(interval, wall)
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


        let edges = self.stack.last().map(|s| s.default_edges()).unwrap_or(false);
        if edges {
            if g.top_edge_swipe() || matches!(g, Gesture::TwoFingerTap) {
                let overlay = self
                    .overlay
                    .as_ref()
                    .map(|make| make())
                    .unwrap_or_else(|| Box::new(FrontlightScreen::new()));
                return Action::Push(overlay);
            }
            if g.corner_back() {
                return Action::Pop;
            }
        }
        match self.stack.last_mut() {
            Some(s) => s.on_gesture(g),
            None => Action::Quit,
        }
    }






    fn apply(&mut self, a: Action) -> bool {
        let (cont, redraw_full) = transition(&mut self.stack, a);
        // Re-resolve orientation after the stack change; a flip forces the
        // refresh to full (rotating without the flash ghosts badly).
        let flipped = self.resolve_orientation();
        match redraw_full {
            Some(full) => self.draw_top(full || flipped),
            None if flipped => self.draw_top(true),
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

    fn draw_top(&mut self, full: bool) {
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
            let mut p = Painter::new(
                panel.buf_mut(),
                w,
                h,
                stride,
                *orientation,
                canvas,
                font,
            );
            if let Some(s) = stack.last_mut() {
                s.draw(&mut p);
            }
            p.flush();
        }
        if full {
            panel.refresh_full();
        } else {
            panel.refresh_partial(0, 0, w, h);
        }
    }
}

/// Pure stack machine behind Action — separately testable with fake
/// screens. Returns (continue?, redraw mode: Some(full?) means draw the
/// new top now).
fn transition(stack: &mut Vec<Box<dyn Screen>>, a: Action) -> (bool, Option<bool>) {
    match a {
        Action::Keep => (true, None),
        Action::Redraw => (true, Some(false)),
        Action::RedrawFull => (true, Some(true)),
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
        assert_eq!(redraw, Some(true));
        assert_eq!(*log.borrow(), vec!["a:enter"]);
    }

    #[test]
    fn popn_unwinds_multiple_overlays_and_resumes_the_base() {
        // A TOC picked on top of a scrubber must unwind both dialogs and
        // resume the reader beneath, not the scrubber in the middle.
        let log = Rc::new(RefCell::new(vec![]));
        let mut stack = vec![fake("reader", &log), fake("scrubber", &log), fake("toc", &log)];
        log.borrow_mut().clear();

        let (cont, redraw) = transition(&mut stack, Action::PopN(2));
        assert!(cont);
        assert_eq!(redraw, Some(true));
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
        assert_eq!(redraw, Some(true));
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
        assert_eq!(redraw, Some(false));
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
        assert_eq!(redraw, Some(true)); // msg's default on_enter
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
