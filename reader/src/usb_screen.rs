//! The USB farewell screen: painted as the reader's last act when USB
//! power arrives, then the app exits (43) so the stock drive-mode dance
//! gets a free disk. Why exit at all: everything of ours lives on
//! /mnt/us — while the cable owns the disk the device is, by definition,
//! a card reader with a battery, so the reader gets out of the way
//! *gracefully* (positions flushed, frontlight restored) instead of
//! being evicted by the forced unmount minutes later (field-measured
//! 2026-08-25: with the reader holding the disk, volumd's unmount
//! handshake stalls and one 37 s plug left an empty LUN — no drive at
//! all). The paint stays on glass for the whole plug: e-ink holds it
//! for free, and boot.sh — parked at its unplug wait — has nothing to
//! draw and needs nothing.

use ybdev::input::Gesture;
use yui::painter::{pt, Painter};
use yui::screen::{Action, Screen};

pub struct UsbScreen;

impl UsbScreen {
    pub fn new() -> UsbScreen {
        UsbScreen
    }
}

impl Screen for UsbScreen {
    fn draw(&mut self, p: &mut Painter) {
        let (_, h) = p.size();
        p.clear(255);
        p.text_center(h / 2 - pt(30.0), 16.0, 0, "USB Drive Mode");
        p.text_center(
            h / 2 + pt(6.0),
            9.0,
            110,
            "The Kindle drive is available to this computer.",
        );
        p.text_center(
            h / 2 + pt(26.0),
            8.0,
            130,
            "Eject and unplug to continue reading.",
        );
    }

    fn on_gesture(&mut self, _g: Gesture) -> Action {
        // One paint, no interaction: the disk is leaving regardless.
        Action::Keep
    }

    fn default_edges(&self) -> bool {
        false
    }

    fn tick_interval(&self) -> std::time::Duration {
        // Long enough that the transition's paint lands on glass before
        // the quit tears the app down, short enough that the exit (and
        // the freed disk) follows the plug by less than a second.
        std::time::Duration::from_millis(400)
    }

    fn on_tick(&mut self) -> Action {
        Action::Quit
    }
}
