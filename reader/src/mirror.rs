//! MirrorScreen — the Rust port of mirror.koplugin's MirrorView on the
//! yui stack. Same protocol, same timings, same retry semantics as the
//! ad-hoc version. The Mac side (mac/server.py) is untouched.
//!
//! Structure: network work happens in on_enter / on_gesture / on_tick;
//! fetched PNG frames are decoded into a pixel cache that draw() blits,
//! so an overlay pop (frontlight) re-presents the frame without a
//! re-fetch.
//!
//! Two modes. Read (default): taps and E/W swipes turn pages with the
//! TURN_KEYS preset. Control (two-finger tap to toggle): taps click at
//! their mirrored coordinates, swipes scroll — the Kindle becomes a
//! touchpad for the window.

use std::time::{Duration, Instant};

use ybdev::config::{self, ServerConf};
use ybdev::input::{Gesture, SwipeDir};
use ybdev::log::{now_ms, plog};

use crate::protocol::{self, Conn, Resp};
use crate::wifi;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};

const CONF_PATH: &str = "/mnt/us/extensions/mirror/mirror.conf";
/// Keepalive cadence. Doubles as radio-heat: the MTK wifi power-save
/// dozes the radio between turns, and the first packets of each request
/// pay the wake-up — measured 2026-08-22, back-to-back requests cost
/// 25–85 ms on the leg while page turns spaced a second or two apart
/// paid 65–210 ms. A sub-3 s ping keeps the radio out of deep doze for
/// the price of a few tiny packets per interval (mirroring holds the
/// device awake anyway).
const PING_EVERY_MS: u128 = 2_500;

/// Read-mode page-turn keys, from mirror.conf TURN_KEYS=.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnPreset {
    /// ←/→ — readers whose own JS flips pages on arrows (the classic
    /// yb-mirror behavior, verified against books.example.com).
    Arrows,
    /// Space / Shift+Space — readers that page on scroll keys. Only
    /// works where the site binds Space itself: a pid-posted Space (or
    /// ↓) never triggers Safari's *native* space-scroll, which needs
    /// first-responder focus in the page.
    Space,
    /// PageDown / PageUp — native full-page keys, delivered even to a
    /// background Safari (verified), no site JS required.
    Pages,
}

impl TurnPreset {
    fn from_conf(v: &Option<String>) -> TurnPreset {
        match v.as_deref().map(str::trim) {
            Some("space") => TurnPreset::Space,
            Some("pages") => TurnPreset::Pages,
            _ => TurnPreset::Arrows,
        }
    }

    /// The query that turns one page forward/back. It carries its own
    /// params; act() appends wait/id/frame — same turn machinery for
    /// every action.
    fn next(&self) -> &'static str {
        match self {
            TurnPreset::Arrows => "/key?k=right",
            TurnPreset::Space => "/key?k=space",
            TurnPreset::Pages => "/key?k=pagedown",
        }
    }

    fn prev(&self) -> &'static str {
        match self {
            TurnPreset::Arrows => "/key?k=left",
            TurnPreset::Space => "/key?k=space&shift=1",
            TurnPreset::Pages => "/key?k=pageup",
        }
    }
}

pub struct MirrorScreen {
    w: u32,
    h: u32,
    conf: ServerConf,
    conn: Option<Conn>,
    server: Option<String>,
    host: Option<String>,
    port: u16,
    frame_count: u32,
    turn_seq: u32,
    busy: bool,
    ping_chain: bool,
    last_ping: u128,
    last_frame: Option<Vec<u8>>,
    /// Payload of the frame currently on glass — the speculative turn
    /// flow fetches a settled correction after showing the early frame;
    /// when it comes back byte-identical (the common case) the decode
    /// and the refresh are both skipped.
    last_shown: Option<Vec<u8>>,
    /// Decoded current frame (tight w*h grayscale).
    gray: Option<Vec<u8>>,
    /// Control mode: taps click, swipes scroll.
    control: bool,
    preset: TurnPreset,
    /// Settle correction in flight: the frame from the last action is on
    /// glass, but the server did not confirm it settled. on_tick fetches
    /// the settled frame until it does (or the deadline passes); taps
    /// stay live the whole time.
    settle_deadline: Option<Instant>,
}

impl MirrorScreen {
    pub fn new(w: u32, h: u32) -> MirrorScreen {
        let conf = config::read(CONF_PATH);
        let preset = TurnPreset::from_conf(&conf.turn_keys);
        MirrorScreen {
            w,
            h,
            conf,
            conn: None,
            server: None,
            host: None,
            port: protocol::DEFAULT_PORT,
            frame_count: 0,
            turn_seq: 0,
            busy: false,
            ping_chain: false,
            last_ping: 0,
            last_frame: None,
            last_shown: None,
            gray: None,
            control: false,
            preset,
            settle_deadline: None,
        }
    }

    fn frame_query(&self) -> String {
        format!("w={}&h={}&bpp=4", self.w, self.h)
    }

    /// Connect, using the remembered address if it still works, discovering
    /// the Mac otherwise.
    fn ensure_conn(&mut self) -> bool {
        if self.conn.is_some() {
            return true;
        }
        wifi::ensure_wifi();

        if let Some(s) = self.conf.server.clone() {
            let (host, port) = config::parse_server(&s);
            if let Some(host) = host {
                let mut conn = Conn::new(&host, port);
                if conn.open() {
                    self.conn = Some(conn);
                    self.host = Some(host);
                    self.port = port;
                    return true;
                }
                plog(&format!("conf server unreachable: {}", s));
            }
        }

        match protocol::discover(Duration::from_secs(1)) {
            Some((ip, port)) => {
                let mut conn = Conn::new(&ip, port);
                if conn.open() {
                    let server = format!("http://{}:{}", ip, port);
                    self.conn = Some(conn);
                    self.host = Some(ip);
                    self.port = port;
                    self.server = Some(server.clone());
                    config::write_server(CONF_PATH, &server);
                    plog(&format!("discovered Mac at {}", server));
                    return true;
                }
                plog(&format!("discovered {} but connect failed", ip));
            }
            None => {
                plog(&format!(
                    "discovery: no server answered (conf: {:?})",
                    self.conf.server
                ));
            }
        }
        false
    }

    /// Fresh-connection liveness probe after a failed turn.
    fn probe_alive(&mut self) -> bool {
        let Some(host) = self.host.clone() else {
            return false;
        };
        let mut conn = Conn::new(&host, self.port);
        if !conn.open() {
            return false;
        }
        let mut sink = |_: &[u8]| true;
        let ok = conn
            .request("GET", "/ping", &mut sink)
            .map(|r| r.status == 200)
            .unwrap_or(false);
        if ok {
            self.conn = Some(conn);
            return true;
        }
        false
    }

    /// Stream one request's body into a buffer. Returns headers, or None
    /// after one safe retry.
    fn fetch(&mut self, method: &str, path: &str) -> Option<Resp> {
        if !self.ensure_conn() {
            return None;
        }
        let t0 = now_ms();
        let mut buf: Vec<u8> = Vec::new();
        let mut sink = |chunk: &[u8]| {
            buf.extend_from_slice(chunk);
            true
        };
        let mut r = self.conn.as_mut().and_then(|c| {
            c.request(method, path, &mut sink)
                .map_err(|stage| stage)
                .ok()
        });
        let mut stage = r.as_ref().map(|_| "ok").unwrap_or("request1");
        if r.is_none() {
            // Retrying is safe for GETs, and for POSTs because page turns
            // carry an idempotency key.
            buf.clear();
            let mut sink = |chunk: &[u8]| {
                buf.extend_from_slice(chunk);
                true
            };
            r = self.conn.as_mut().and_then(|c| {
                c.request(method, path, &mut sink)
                    .map_err(|s| {
                        stage = match s {
                            protocol::Stage::Connect => "connect",
                            protocol::Stage::Send => "send",
                            protocol::Stage::Status => "status",
                            protocol::Stage::Headers => "headers",
                            protocol::Stage::Body => "body",
                        };
                        s
                    })
                    .ok()
            });
        }
        let ms = now_ms() - t0;
        let nbytes = buf.len();
        plog(&format!(
            "{} {} {}ms {}B status={:?} stage={}",
            method,
            path,
            ms,
            nbytes,
            r.as_ref().map(|x| x.status),
            stage
        ));
        match r {
            Some(resp) if resp.status == 200 => {
                self.last_frame = Some(buf);
                Some(resp)
            }
            _ => None,
        }
    }

    /// Decode the fetched frame into the pixel cache and say how it must
    /// be presented: a full flash on the first frame, every Nth frame,
    /// or when forced (screen-clean); partial otherwise. Mirrors the old
    /// show_frame() decision exactly.
    fn present_last(&mut self, force_full: bool) -> Action {
        let Some(frame) = self.last_frame.take() else {
            return Action::Keep;
        };
        // Byte-identical re-fetch: nothing to decode, nothing to draw.
        // Never skipped for force_full — screen-clean exists to flash,
        // and ghosting isn't in the pixels.
        if !force_full && self.last_shown.as_deref() == Some(frame.as_slice()) {
            plog("mirror: correction identical — skip");
            return Action::Keep;
        }
        let t0 = Instant::now();
        let decoded = ybdev::img::decode_png_gray(&frame, self.w, self.h);
        let decode_ms = t0.elapsed().as_millis();
        match decoded {
            Some(gray) => {
                // Visually-identical re-fetch: differs in bytes but by
                // less than a sliver (progress tick, cursor) — under 2k
                // of 2M px beyond 8 gray levels. Cache the truth, skip
                // the e-ink flash.
                let visually_same = !force_full
                    && match self.gray.as_deref() {
                        Some(prev) if prev.len() == gray.len() => prev
                            .iter()
                            .zip(gray.iter())
                            .filter(|&(a, b)| a.abs_diff(*b) > 8)
                            .count()
                            < 2000,
                        _ => false,
                    };
                if visually_same {
                    plog("mirror: correction visually identical — no flash");
                    self.gray = Some(gray);
                    self.last_shown = Some(frame);
                    return Action::Keep;
                }
                plog(&format!("mirror decode: {}ms ({}B)", decode_ms, frame.len()));
                self.last_shown = Some(frame);
                self.gray = Some(gray);
                self.frame_count += 1;
                let every = self.conf.refresh_every.unwrap_or(60);
                let full = force_full
                    || self.frame_count == 1
                    || (every != 0 && self.frame_count % every == 0);
                if full {
                    Action::RedrawFull
                } else {
                    Action::Redraw
                }
            }
            None => {
                plog("present_last: PNG decode failed");
                Action::Keep
            }
        }
    }

    /// First-frame fetch attempt: on success present, otherwise fall
    /// through to the in-screen "not found" state (draw() shows the
    /// retry hint when there is no frame).
    fn fetch_first_frame(&mut self) -> Action {
        let fp = format!("/frame.png?{}", self.frame_query());
        if self.fetch("GET", &fp).is_some() {
            self.present_last(true)
        } else {
            Action::RedrawFull
        }
    }

    /// One action on the mirrored window — key press, tap, scroll — with
    /// the shared turn semantics (idempotency, settle-polling, loss
    /// probe). `action` is the endpoint plus its own params, e.g.
    /// "/key?k=space&shift=1" or "/tap?x=100&y=200".
    fn act(&mut self, action: &str) -> Action {
        if self.busy {
            return Action::Keep;
        }
        self.busy = true;
        let a = self.act_inner(action);
        self.busy = false;
        a
    }

    fn act_inner(&mut self, action: &str) -> Action {
        // Idempotency key: one id per action, reused across retries.
        self.turn_seq += 1;
        let path = format!(
            "{}&wait=1&id={}&{}",
            action,
            self.turn_seq,
            self.frame_query()
        );
        match self.fetch("POST", &path) {
            Some(resp) => {
                // Not done yet: the site hadn't changed within the server's
                // wait window, or changed but hadn't settled. Poll with plain
                // GETs (never a re-POST — that would skip a page).
                let changed = resp
                    .headers
                    .get("x-changed")
                    .map(|v| v != "0")
                    .unwrap_or(true);
                let settled = resp
                    .headers
                    .get("x-settled")
                    .map(|v| v != "0")
                    .unwrap_or(true);
                if !settled {
                    // The reply is not final truth, but the frame it
                    // carries is usually the page already (a sparse
                    // chapter end never "settles": the server's loader
                    // heuristic misreads it). Show it NOW and let on_tick
                    // fetch the settled correction — the old inline poll
                    // blocked the gesture handler, freezing the previous
                    // page and dropping taps for its whole 8 s window.
                    self.settle_deadline =
                        Some(Instant::now() + Duration::from_secs(3));
                    if changed {
                        return self.present_last(false);
                    }
                    // No change observed: nothing new to show yet; the
                    // settle ticks deliver it when it lands.
                    return Action::Keep;
                }
                self.present_last(false)
            }
            None => {
                // Transient radio loss mid-connection: probe on a fresh
                // connection; if alive, quietly show the current frame.
                if self.probe_alive() {
                    plog("turn failed, Mac alive — transient, showing current frame");
                    let fp = format!("/frame.png?{}", self.frame_query());
                    if self.fetch("GET", &fp).is_some() {
                        return self.present_last(false);
                    }
                }
                self.conn = None; // force full reconnect next time
                // Mid-session loss: keep the last frame on screen; the
                // next tap retries through ensure_conn/discovery.
                Action::Keep
            }
        }
    }

    fn ping_tick(&mut self) {
        let now = now_ms();
        if !self.ping_chain {
            return;
        }
        if now - self.last_ping < PING_EVERY_MS {
            return;
        }
        self.last_ping = now;
        let mut sink = |_: &[u8]| true;
        let mut ok = false;
        if let Some(conn) = self.conn.as_mut() {
            ok = conn
                .request("GET", "/ping", &mut sink)
                .map(|r| r.status == 200)
                .unwrap_or(false);
        }
        if !ok {
            // One retry through a fresh connection (a dozing radio must not
            // tear the connection down while its reply is in flight).
            if let Some(host) = self.host.clone() {
                let mut conn = Conn::new(&host, self.port);
                if conn.open() {
                    ok = conn
                        .request("GET", "/ping", &mut sink)
                        .map(|r| r.status == 200)
                        .unwrap_or(false);
                    if ok {
                        self.conn = Some(conn);
                    }
                }
            }
        }
        if !ok {
            plog("keepalive: two pings failed — dropping connection");
            self.conn = None;
        }
    }
}

impl Screen for MirrorScreen {
    fn on_enter(&mut self) -> Action {
        plog("mirror start");
        crate::awake::screen_wants_awake(true);
        self.last_ping = now_ms();
        self.ping_chain = true;
        self.fetch_first_frame()
    }

    fn on_leave(&mut self) {
        self.ping_chain = false;
        self.conn = None;
        crate::awake::screen_wants_awake(false);
        plog("mirror exit");
    }

    fn holds_awake(&self) -> bool {
        true
    }

    fn draw(&mut self, p: &mut Painter) {
        p.clear(255);
        let (_, h) = p.size();
        if let Some(gray) = &self.gray {
            p.blit_gray(0, 0, self.w as i32, self.h as i32, gray, self.w as usize);
            if self.control {
                // The frame is WYSIWYG, so mark the one state that changes
                // what touches do. Top-left, opposite the read-mode
                // screen-clean corner.
                let bw = p.text_width(7.0, "CTRL") as i32 + pt(10.0);
                p.rect(Rect::new(0, 0, bw, pt(15.0)), 0);
                p.text(pt(5.0), pt(10.5), 7.0, 255, "CTRL");
            }
        } else {
            // No frame yet (server was down at entry): own the state
            // instead of pushing an overlay — an overlay's dismiss gesture
            // would land on a blank mirror and demand yet another swipe.
            p.text_center(h / 2 - pt(14.0), 10.0, 0, "Mirror: Mac not found");
            p.text_center(h / 2 + pt(6.0), 8.0, 120, "tap: retry · swipe: exit");
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        // Frameless state: every tap retries the fetch, any swipe leaves.
        if self.gray.is_none() {
            return match g {
                Gesture::Tap { .. } => {
                    plog("retry: tap with no frame");
                    let fp = format!("/frame.png?{}", self.frame_query());
                    if self.fetch("GET", &fp).is_some() {
                        self.present_last(true)
                    } else {
                        Action::Keep // hint is already on screen; stay quiet
                    }
                }
                _ => Action::Pop,
            };
        }
        // Control mode: the Kindle is a touchpad for the mirrored window.
        // Taps click at their mirrored coordinates, swipes scroll (raw
        // delta, natural-scroll sign: finger up moves the content up).
        // Exit stays with the app-level corner-back swipe.
        if self.control {
            return match g {
                Gesture::Tap { x, y } => self.act(&format!("/tap?x={}&y={}", x, y)),
                Gesture::Swipe { x, y, ex, ey, .. } => self.act(&format!(
                    "/scroll?dx={}&dy={}",
                    ex as i32 - x as i32,
                    ey as i32 - y as i32
                )),
                Gesture::TwoFingerTap => {
                    self.control = false;
                    plog("mirror: read mode");
                    Action::Redraw
                }
                _ => Action::Keep,
            };
        }
        let (w, h) = (self.w as i32, self.h as i32);
        match g {
            Gesture::TwoFingerTap => {
                self.control = true;
                plog("mirror: control mode");
                Action::Redraw
            }
            Gesture::Tap { x, y } => {
                let (x, y) = (x as i32, y as i32);
                // Screen-clean lives in the top-right corner.
                if x > w * 85 / 100 && y < h * 12 / 100 {
                    let fp = format!("/frame.png?{}", self.frame_query());
                    if self.fetch("GET", &fp).is_some() {
                        return self.present_last(true);
                    }
                    return Action::Keep;
                }
                if x < w / 3 {
                    self.act(self.preset.prev())
                } else {
                    self.act(self.preset.next())
                }
            }
            Gesture::Swipe { dir: SwipeDir::East, .. } => self.act(self.preset.prev()),
            Gesture::Swipe { dir: SwipeDir::West, .. } => self.act(self.preset.next()),
            // down/up/anything else: exit, even mid-sync
            Gesture::Swipe { .. } => Action::Pop,
            _ => Action::Keep,
        }
    }


    /// In Control mode every swipe belongs to the mirrored window — the
    /// app-level edges (top-edge brightness, corner-back) would eat page
    /// scrolls that start near an edge. Two-finger tap returns to Read
    /// mode, where the edges (and vertical-swipe exit) work again.
    fn default_edges(&self) -> bool {
        !self.control
    }

    fn on_tick(&mut self) -> Action {
        self.ping_tick();
        // Settle correction: one frame fetch per tick until the server
        // confirms settled or the deadline passes. Byte-identical frames
        // flash nothing (present_last skips them), so this is quiet on
        // final pages and self-corrects genuine loaders.
        let Some(deadline) = self.settle_deadline else {
            return Action::Keep;
        };
        if Instant::now() >= deadline {
            self.settle_deadline = None;
            return Action::Keep;
        }
        let fp = format!("/frame.png?{}", self.frame_query());
        if let Some(resp) = self.fetch("GET", &fp) {
            let settled = resp
                .headers
                .get("x-settled")
                .map(|v| v != "0")
                .unwrap_or(true);
            let a = self.present_last(false);
            if settled {
                self.settle_deadline = None;
            }
            return a;
        }
        Action::Keep
    }

    fn on_resume(&mut self) -> Action {
        // After an overlay closes: re-present the cached frame, or the
        // not-found hint when there is none — never a blank screen.
        Action::Redraw
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(500)
    }
}
