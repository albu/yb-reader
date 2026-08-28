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
//! TURN_KEYS preset. Control: taps click at their mirrored coordinates,
//! swipes scroll — the Kindle becomes a touchpad for the window. The
//! quick-settings sheet (bottom-left swipe-up) switches modes and picks
//! the preset; two-finger tap is the app-wide screen-clean (full flash
//! of the current frame), same as the reader.

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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

    /// The mirror.conf TURN_KEYS= token (from_conf's inverse).
    fn as_conf(&self) -> &'static str {
        match self {
            TurnPreset::Arrows => "arrows",
            TurnPreset::Space => "space",
            TurnPreset::Pages => "pages",
        }
    }

    /// Sheet radio label — left to right in PRESETS order.
    fn label(&self) -> &'static str {
        match self {
            TurnPreset::Arrows => "Arrows",
            TurnPreset::Space => "Space",
            TurnPreset::Pages => "Pages",
        }
    }
}

/// Sheet display order, left to right.
const PRESETS: [TurnPreset; 3] = [TurnPreset::Arrows, TurnPreset::Space, TurnPreset::Pages];

/// Quick-settings sheet metrics in points — same idioms as the reader's
/// quick_settings sheet (grab pill, 8.5pt row labels, pt(22) buttons).
const SHEET_H_PT: f32 = 104.0;
const SHEET_PAD_PT: f32 = 16.0;
const SHEET_ROW_H_PT: f32 = 25.0;
const SHEET_BTN_H_PT: f32 = 22.0;

/// What a tap on the open sheet means. Pure data so the hit mapping is
/// testable without a Painter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SheetHit {
    Dismiss,
    ToggleControl,
    Preset(TurnPreset),
}

/// Sheet geometry shared by draw and hit-testing, panel pixels.
fn sheet_rows(w: i32, h: i32) -> (i32, i32, i32, Rect) {
    let sheet_y = h - pt(SHEET_H_PT);
    let row1_y = sheet_y + pt(12.0);
    let row2_y = row1_y + pt(SHEET_ROW_H_PT) + pt(4.0);
    let btn_w = pt(120.0);
    let control_btn = Rect::new(
        w - pt(SHEET_PAD_PT) - btn_w,
        row1_y,
        btn_w,
        pt(SHEET_BTN_H_PT),
    );
    (sheet_y, row1_y, row2_y, control_btn)
}

/// The three preset radio buttons on row 2, left to right.
fn preset_buttons(w: i32, row2_y: i32) -> [(TurnPreset, Rect); 3] {
    let btn_w = pt(64.0);
    let gap = pt(4.0);
    let mut out = [(TurnPreset::Arrows, Rect::new(0, 0, 0, 0)); 3];
    for i in 0..3 {
        let x = w - pt(SHEET_PAD_PT) - (3 - i as i32) * (btn_w + gap);
        out[i] = (PRESETS[i], Rect::new(x, row2_y, btn_w, pt(SHEET_BTN_H_PT)));
    }
    out
}

pub struct MirrorScreen {
    w: u32,
    h: u32,
    conf: ServerConf,
    conn: Option<Conn>,
    server: Option<String>,
    /// SERVER= as currently pinned in mirror.conf — a discovered server is
    /// persisted only after a successful exchange (see fetch), never on the
    /// mere say-so of a UDP reply.
    persisted_server: Option<String>,
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
    /// Quick-settings sheet open (inline overlay — draw paints it over
    /// the frame, on_gesture routes to it first). Inline, not a pushed
    /// Screen, so runtime state (control/preset) never leaves this
    /// screen and settle ticks stay owned here.
    settings: bool,
    /// True after a 401: the Mac enforces a pairing this Kindle doesn't
    /// satisfy. draw() shows the re-pair hint instead of the frame; cleared
    /// on the next successful exchange.
    auth_required: bool,
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
        let persisted_server = conf.server.clone();
        MirrorScreen {
            w,
            h,
            conf,
            conn: None,
            server: None,
            persisted_server,
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
            settings: false,
            auth_required: false,
            settle_deadline: None,
        }
    }

    fn frame_query(&self) -> String {
        format!("w={}&h={}&bpp=4", self.w, self.h)
    }

    /// Connect, using the remembered address if it still works, discovering
    /// the Mac otherwise with trusted device credentials.
    fn ensure_conn(&mut self) -> bool {
        if self.conn.is_some() {
            return true;
        }
        wifi::ensure_wifi();

        let devices_path = ybdev::devices::devices_path();
        let kindle_id_path = ybdev::devices::kindle_id_path();
        let store = ybdev::devices::DeviceStore::load(&devices_path);
        let profile = ybdev::devices::KindleProfile::load_or_create(&kindle_id_path, self.w, self.h);

        if let Some(s) = self.conf.server.clone() {
            let (host, port) = config::parse_server(&s);
            if let Some(host) = host {
                let mut conn = Conn::new(&host, port);
                let secret = self.conf.secret.clone().or_else(|| {
                    store.find_by_ip_for_control(&host).map(|d| d.token.clone())
                });
                conn.set_secret(secret);
                conn.set_kindle_id(Some(profile.id.clone()));
                if conn.open() {
                    self.conn = Some(conn);
                    self.host = Some(host);
                    self.port = port;
                    return true;
                }
                plog(&format!("conf server unreachable: {}", s));
            }
        }

        match protocol::discover_trusted(Duration::from_secs(1), Some(&profile.id), &store) {
            Some((srv, trusted_token)) => {
                let mut conn = Conn::new(&srv.ip, srv.port);
                // conf.secret flows to the discovery fallback ONLY when it
                // re-confirms the explicitly configured host (trust-by-config).
                // An anonymous racer that merely answered the broadcast faster
                // gets nothing — it must not collect a long-lived credential.
                let configured_host = self
                    .conf
                    .server
                    .as_deref()
                    .and_then(|s| config::parse_server(s).0);
                let secret = trusted_token.or_else(|| {
                    (Some(&srv.ip) == configured_host.as_ref())
                        .then(|| self.conf.secret.clone())
                        .flatten()
                });
                conn.set_secret(secret);
                conn.set_kindle_id(Some(profile.id.clone()));
                if conn.open() {
                    let server = format!("http://{}:{}", srv.ip, srv.port);
                    self.conn = Some(conn);
                    self.host = Some(srv.ip.clone());
                    self.port = srv.port;
                    self.server = Some(server.clone());
                    // NOTE: SERVER= is NOT persisted here. Persistence happens
                    // in fetch() after the host proves itself with a real
                    // exchange — a rogue responder that merely wins the UDP
                    // discovery race must stay session-scoped.
                    plog(&format!("discovered Mac at {}", server));
                    return true;
                }
                plog(&format!("discovered {} but connect failed", srv.ip));
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
        let (kindle_id, secret) = self.profile_and_secret(&host);

        let mut conn = Conn::new(&host, self.port);
        conn.set_secret(secret);
        conn.set_kindle_id(Some(kindle_id));
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

    /// The credential + identity for a host: the configured static SECRET=
    /// (trust-by-config) or the paired device token for that IP. probe_alive
    /// and the keepalive retry both need it — once the Mac enforces, a
    /// discovery-paired Kindle must not fall back to an anonymous request.
    fn profile_and_secret(&self, host: &str) -> (String, Option<String>) {
        let devices_path = ybdev::devices::devices_path();
        let kindle_id_path = ybdev::devices::kindle_id_path();
        let profile =
            ybdev::devices::KindleProfile::load_or_create(&kindle_id_path, self.w, self.h);
        let store = ybdev::devices::DeviceStore::load(&devices_path);
        let secret = self.conf.secret.clone().or_else(|| {
            store.find_by_ip_for_control(&host).map(|d| d.token.clone())
        });
        (profile.id, secret)
    }

    /// Stream one request's body into a buffer. Returns headers, or None
    /// after one safe retry.
    fn fetch(&mut self, method: &str, path: &str) -> Option<Resp> {
        if !self.ensure_conn() {
            return None;
        }
        let t0 = now_ms();
        let mut buf: Vec<u8> = Vec::new();
        let mut stage = "ok";
        // Mirror bodies are grayscale screen PNGs ("well under 1 MB" per
        // protocol's own note) plus tiny status/ack JSON. The protocol's
        // 16 MB ceiling exists for the ai_stream's bigger turns; a mirror
        // server declaring that much must not make the settle loop
        // allocate it per 500 ms re-fetch on a ~150 MB-RAM device.
        // Returning false aborts the request cleanly (Stage::Body).
        const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
        let mut sink = |chunk: &[u8]| {
            if buf.len() + chunk.len() > MAX_FRAME_BYTES {
                return false;
            }
            buf.extend_from_slice(chunk);
            true
        };
        let mut r = self.conn.as_mut().and_then(|c| {
            c.request(method, path, &mut sink)
                .inspect_err(|&s| {
                    // Capture the real failure stage for the log — before,
                    // the first attempt reported a generic "request1".
                    stage = match s {
                        protocol::Stage::Connect => "connect",
                        protocol::Stage::Send => "send",
                        protocol::Stage::Status => "status",
                        protocol::Stage::Headers => "headers",
                        protocol::Stage::Body => "body",
                    };
                })
                .ok()
        });
        if r.is_none() {
            // Retrying is safe for GETs, and for POSTs because page turns
            // carry an idempotency key. (A second closure, not reuse: the
            // first still borrows buf until its last use, and buf must be
            // cleared in between.)
            buf.clear();
            let mut sink = |chunk: &[u8]| {
                if buf.len() + chunk.len() > MAX_FRAME_BYTES {
                    return false;
                }
                buf.extend_from_slice(chunk);
                true
            };
            r = self.conn.as_mut().and_then(|c| {
                c.request(method, path, &mut sink)
                    .inspect_err(|&s| {
                        stage = match s {
                            protocol::Stage::Connect => "connect",
                            protocol::Stage::Send => "send",
                            protocol::Stage::Status => "status",
                            protocol::Stage::Headers => "headers",
                            protocol::Stage::Body => "body",
                        };
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
                self.auth_required = false;
                // Pin SERVER= only now — after the host proved itself with a
                // real (and, when the server enforces it, secret-authenticated)
                // exchange. A rogue first-responder can win one UDP race; it
                // cannot fake a working frame stream.
                if let Some(server) = &self.server {
                    if self.persisted_server.as_deref() != Some(server.as_str()) {
                        config::write_server(CONF_PATH, server);
                        self.persisted_server = Some(server.clone());
                    }
                }
                self.last_frame = Some(buf);
                Some(resp)
            }
            Some(resp) if resp.status == 401 => {
                // Once per process: the mirror cannot recover on its own
                // and the screen now shows the pairing hint — no point
                // repeating the line on every tap + settle poll.
                static AUTH_401_PLOGGED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !AUTH_401_PLOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    plog(concat!(
                        "mirror: unauthorized (401) - open the receive page ",
                        "from the Mac and re-pair this Kindle"));
                }
                self.auth_required = true;
                None
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
                        Some(prev) if prev.len() == gray.len() => {
                            prev.iter()
                                .zip(gray.iter())
                                .filter(|&(a, b)| a.abs_diff(*b) > 8)
                                .count()
                                < 2000
                        }
                        _ => false,
                    };
                if visually_same {
                    plog("mirror: correction visually identical — no flash");
                    self.gray = Some(gray);
                    self.last_shown = Some(frame);
                    return Action::Keep;
                }
                plog(&format!(
                    "mirror decode: {}ms ({}B)",
                    decode_ms,
                    frame.len()
                ));
                self.last_shown = Some(frame);
                self.gray = Some(gray);
                self.frame_count += 1;
                let every = self.conf.refresh_every.unwrap_or(60);
                let full = force_full
                    || self.frame_count == 1
                    || (every != 0 && self.frame_count.is_multiple_of(every));
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
                    self.settle_deadline = Some(Instant::now() + Duration::from_secs(3));
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
            // tear the connection down while its reply is in flight). The
            // retry must carry the same credential as the live connection —
            // once the Mac enforces, an anonymous keepalive ping would 401
            // and drop a perfectly alive pairing every heartbeat cycle.
            if let Some(host) = self.host.clone() {
                let (kindle_id, secret) = self.profile_and_secret(&host);
                let mut conn = Conn::new(&host, self.port);
                conn.set_secret(secret);
                conn.set_kindle_id(Some(kindle_id));
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

    /// The quick-settings sheet over the frame's bottom 104 pt: control
    /// mode on/off, turn-key preset. Modeled on the reader's
    /// quick_settings sheet (shadow, pill, row labels, radio buttons).
    fn draw_sheet(&self, p: &mut Painter) {
        let w = self.w as i32;
        let (sheet_y, row1_y, row2_y, control_btn) = sheet_rows(w, self.h as i32);

        p.hline_t(sheet_y - 2, 0, w, 1, 140);
        p.hline_t(sheet_y - 1, 0, w, 1, 100);
        p.rect(Rect::new(0, sheet_y, w, pt(SHEET_H_PT)), 255);
        p.hline_t(sheet_y, 0, w, 2, 0);
        let handle_w = pt(28.0);
        p.rect(
            Rect::new((w - handle_w) / 2, sheet_y + pt(4.0), handle_w, pt(2.5)),
            170,
        );

        // Row 1: CONTROL MODE on/off.
        p.text(
            pt(SHEET_PAD_PT),
            row1_y + pt(15.0),
            8.5,
            110,
            "CONTROL MODE",
        );
        if self.control {
            p.rect(control_btn, 0);
            p.text_center_in(
                control_btn.x,
                control_btn.x + control_btn.w,
                row1_y + pt(15.0),
                8.5,
                255,
                "ON",
            );
        } else {
            p.rect_outline_t(control_btn, 1, 120);
            p.text_center_in(
                control_btn.x,
                control_btn.x + control_btn.w,
                row1_y + pt(15.0),
                8.5,
                0,
                "OFF",
            );
        }

        // Row 2: TURN KEYS preset, persisted to mirror.conf on pick.
        p.text(pt(SHEET_PAD_PT), row2_y + pt(15.0), 8.5, 110, "TURN KEYS");
        for (preset, br) in preset_buttons(w, row2_y) {
            if preset == self.preset {
                p.rect(br, 0);
                p.text_center_in(
                    br.x,
                    br.x + br.w,
                    row2_y + pt(15.0),
                    7.5,
                    255,
                    preset.label(),
                );
            } else {
                p.rect_outline_t(br, 1, 120);
                p.text_center_in(br.x, br.x + br.w, row2_y + pt(15.0), 7.5, 0, preset.label());
            }
        }

        let footer_y = row2_y + pt(SHEET_ROW_H_PT) + pt(6.0) + pt(13.0);
        p.text_center_in(
            pt(SHEET_PAD_PT),
            w - pt(SHEET_PAD_PT),
            footer_y,
            7.5,
            120,
            "tap above · swipe down to close",
        );
    }

    /// Sheet-open gesture routing: the sheet owns every gesture until
    /// dismissed, and never falls through to the frame handlers (a tap
    /// above the sheet must dismiss, not turn a page).
    fn sheet_gesture(&mut self, g: Gesture) -> Action {
        if let Gesture::Swipe {
            dir: SwipeDir::South,
            ..
        } = g
        {
            return self.close_sheet();
        }
        let Gesture::Tap { x, y } = g else {
            // Two-finger stays the app-wide screen-clean even over the
            // sheet; anything else means nothing here.
            return match g {
                Gesture::TwoFingerTap => Action::RedrawFull,
                _ => Action::Keep,
            };
        };
        match self.sheet_hit(x as i32, y as i32) {
            Some(SheetHit::Dismiss) => self.close_sheet(),
            Some(SheetHit::ToggleControl) => {
                self.control = !self.control;
                plog(&format!(
                    "mirror: control mode {}",
                    if self.control { "on" } else { "off" }
                ));
                Action::Redraw
            }
            Some(SheetHit::Preset(p)) => {
                if p != self.preset {
                    self.preset = p;
                    config::write_turn_keys(CONF_PATH, p.as_conf());
                    plog(&format!("mirror: turn keys = {}", p.as_conf()));
                }
                Action::Redraw
            }
            None => Action::Keep,
        }
    }

    fn close_sheet(&mut self) -> Action {
        self.settings = false;
        // A settle deadline that expires while the sheet was open would
        // be silently cleared by the next tick — if it's about to, push
        // it out so the pending correction still gets its poll.
        if let Some(d) = self.settle_deadline {
            let left = d.checked_duration_since(Instant::now()).unwrap_or_default();
            if left < Duration::from_millis(500) {
                self.settle_deadline = Some(Instant::now() + Duration::from_secs(3));
            }
        }
        Action::Redraw
    }

    /// Map a tap to a sheet control. Above the sheet = dismiss; inside
    /// the toggle / a radio = that action; label gutters = nothing.
    fn sheet_hit(&self, x: i32, y: i32) -> Option<SheetHit> {
        let (sheet_y, _, row2_y, control_btn) = sheet_rows(self.w as i32, self.h as i32);
        if y < sheet_y {
            return Some(SheetHit::Dismiss);
        }
        if control_btn.contains(x, y) {
            return Some(SheetHit::ToggleControl);
        }
        for (preset, r) in preset_buttons(self.w as i32, row2_y) {
            if r.contains(x, y) {
                return Some(SheetHit::Preset(preset));
            }
        }
        None
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
        if self.auth_required {
            // The Mac enforces a pairing this Kindle doesn't satisfy.
            // Owning the screen (like the not-found state) keeps the
            // dismiss gesture from landing on a blank mirror; re-pairing
            // from the receive page is the only way out, so say so instead
            // of pretending the Mac vanished or hunting for Wi-Fi.
            p.text_center(h / 2 - pt(14.0), 10.0, 0, "Mirror: pairing required");
            p.text_center(h / 2 + pt(6.0), 8.0, 120,
                          "open the receive page from the Mac · swipe: exit");
        } else if let Some(gray) = &self.gray {
            p.blit_gray(0, 0, self.w as i32, self.h as i32, gray, self.w as usize);
            if self.control {
                // The frame is WYSIWYG, so mark the one state that changes
                // what touches do. Top-left, opposite the read-mode
                // screen-clean corner.
                let bw = p.text_width(7.0, "CTRL") as i32 + pt(10.0);
                p.rect(Rect::new(0, 0, bw, pt(15.0)), 0);
                p.text(pt(5.0), pt(10.5), 7.0, 255, "CTRL");
            }
            if self.settings {
                self.draw_sheet(p);
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
        // The sheet owns the glass while open: every gesture routes to
        // it, nothing reaches the frame handlers.
        if self.settings {
            return self.sheet_gesture(g);
        }
        // Bottom-left swipe-up opens the sheet, in either mode.
        if g.corner_settings_in(self.w, self.h) {
            self.settings = true;
            plog("mirror: settings");
            return Action::Redraw;
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
                Gesture::TwoFingerTap => Action::RedrawFull,
                _ => Action::Keep,
            };
        }
        let (w, h) = (self.w as i32, self.h as i32);
        match g {
            // App-wide screen-clean: flash the cached frame, no network.
            Gesture::TwoFingerTap => Action::RedrawFull,
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
            Gesture::Swipe {
                dir: SwipeDir::East,
                ..
            } => self.act(self.preset.prev()),
            Gesture::Swipe {
                dir: SwipeDir::West,
                ..
            } => self.act(self.preset.next()),
            // down/up/anything else: exit, even mid-sync
            Gesture::Swipe { .. } => Action::Pop,
            _ => Action::Keep,
        }
    }

    /// In Control mode every swipe belongs to the mirrored window — the
    /// app-level edges (top-edge brightness, corner-back) would eat page
    /// scrolls that start near an edge. The sheet likewise owns every
    /// gesture while open. Read mode (and the app-wide two-finger
    /// screen-clean) keeps the edges.
    fn default_edges(&self) -> bool {
        !self.control && !self.settings
    }

    fn on_tick(&mut self) -> Action {
        self.ping_tick();
        // The sheet owns the glass: a settle present would repaint the
        // frame over it. Pings (above) keep the radio warm; settle polls
        // resume the tick after the sheet closes.
        if self.settings {
            return Action::Keep;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> MirrorScreen {
        // new() reads /mnt/us/... conf — absent on the host, so this is
        // simply the defaults: read mode, Arrows preset, sheet closed.
        let mut s = MirrorScreen::new(1236, 1648);
        // A frame must be on glass for gesture routing; content is
        // irrelevant to every test below.
        s.gray = Some(vec![128u8; 1236 * 1648]);
        s
    }

    fn tap(x: i32, y: i32) -> Gesture {
        Gesture::Tap {
            x: x as u32,
            y: y as u32,
        }
    }

    fn swipe(dir: SwipeDir, x: i32, y: i32) -> Gesture {
        Gesture::Swipe {
            dir,
            x: x as u32,
            y: y as u32,
            ex: x as u32,
            ey: y as u32,
        }
    }

    #[test]
    fn sheet_hit_maps_rows_labels_and_dismiss() {
        let s = screen();
        let (sheet_y, _, row2_y, control_btn) = sheet_rows(1236, 1648);
        assert_eq!(s.sheet_hit(10, 10), Some(SheetHit::Dismiss));
        assert_eq!(s.sheet_hit(10, sheet_y - 1), Some(SheetHit::Dismiss));
        assert_eq!(
            s.sheet_hit(control_btn.x + 5, control_btn.y + 11),
            Some(SheetHit::ToggleControl)
        );
        // Each radio maps to its own preset, left to right.
        for (preset, r) in preset_buttons(1236, row2_y) {
            assert_eq!(
                s.sheet_hit(r.x + r.w / 2, r.y + r.h / 2),
                Some(SheetHit::Preset(preset))
            );
        }
        // Row labels and gutters are inert.
        assert_eq!(s.sheet_hit(pt(SHEET_PAD_PT), row2_y + pt(15.0)), None);
    }

    #[test]
    fn preset_buttons_are_disjoint_and_inside_the_sheet() {
        let (sheet_y, _, row2_y, _) = sheet_rows(1236, 1648);
        let btns = preset_buttons(1236, row2_y);
        for (p, r) in &btns {
            assert!(r.x > 0 && r.x + r.w <= 1236 - pt(SHEET_PAD_PT));
            assert!(r.y >= sheet_y && r.y + r.h <= 1648);
            assert!(r.w > 0 && r.h > 0, "{:?} must have area", p);
        }
        for i in 0..btns.len() {
            for j in i + 1..btns.len() {
                let a = &btns[i].1;
                let b = &btns[j].1;
                let disjoint = a.x + a.w <= b.x || b.x + b.w <= a.x;
                assert!(disjoint, "radio buttons must not overlap");
            }
        }
    }

    #[test]
    fn sheet_dismisses_on_south_swipe_or_tap_above_but_not_stray_swipes() {
        let mut s = screen();
        s.settings = true;
        // South swipe closes.
        assert!(matches!(
            s.sheet_gesture(swipe(SwipeDir::South, 600, 1500)),
            Action::Redraw
        ));
        assert!(!s.settings);
        // Tap above the sheet closes.
        s.settings = true;
        assert!(matches!(s.sheet_gesture(tap(600, 100)), Action::Redraw));
        assert!(!s.settings);
        // North/East/West over the sheet do nothing — and must NOT exit
        // the screen the way they would in read mode.
        s.settings = true;
        assert!(matches!(
            s.sheet_gesture(swipe(SwipeDir::North, 600, 1600)),
            Action::Keep
        ));
        assert!(matches!(
            s.sheet_gesture(swipe(SwipeDir::East, 600, 1600)),
            Action::Keep
        ));
        assert!(matches!(
            s.sheet_gesture(swipe(SwipeDir::West, 600, 1600)),
            Action::Keep
        ));
        assert!(s.settings, "stray swipes must leave the sheet open");
    }

    #[test]
    fn sheet_toggles_control_and_picks_preset() {
        let mut s = screen();
        s.settings = true;
        let (_, _, _, control_btn) = sheet_rows(1236, 1648);
        assert!(!s.control);
        assert!(matches!(
            s.sheet_gesture(tap(control_btn.x + 5, control_btn.y + 11)),
            Action::Redraw
        ));
        assert!(s.control);
        // Pick Space (middle radio); the runtime preset changes even
        // though the conf write no-ops on the host (no /mnt/us).
        let row2_y = sheet_rows(1236, 1648).2;
        let btns = preset_buttons(1236, row2_y);
        assert!(matches!(
            s.sheet_gesture(tap(btns[1].1.x + 5, btns[1].1.y + 11)),
            Action::Redraw
        ));
        assert_eq!(s.preset, TurnPreset::Space);
        assert_eq!(
            TurnPreset::from_conf(&Some("space".into())),
            TurnPreset::Space
        );
        assert_eq!(
            TurnPreset::from_conf(&Some("pages".into())),
            TurnPreset::Pages
        );
        assert_eq!(TurnPreset::from_conf(&None), TurnPreset::Arrows);
        assert_eq!(TurnPreset::Space.as_conf(), "space");
    }

    #[test]
    fn two_finger_is_full_refresh_in_both_modes_and_never_toggles_control() {
        let mut s = screen();
        assert!(matches!(
            s.on_gesture(Gesture::TwoFingerTap),
            Action::RedrawFull
        ));
        assert!(!s.control, "read mode: two-finger must not enter control");
        s.control = true;
        assert!(matches!(
            s.on_gesture(Gesture::TwoFingerTap),
            Action::RedrawFull
        ));
        assert!(s.control, "control mode: two-finger must not exit control");
        // And through the sheet path too.
        s.settings = true;
        assert!(matches!(
            s.sheet_gesture(Gesture::TwoFingerTap),
            Action::RedrawFull
        ));
    }

    #[test]
    fn bottom_left_swipe_up_opens_the_sheet_in_both_modes() {
        let mut s = screen();
        assert!(matches!(
            s.on_gesture(swipe(SwipeDir::North, 30, 1600)),
            Action::Redraw
        ));
        assert!(s.settings);
        s.settings = false;
        s.control = true;
        assert!(matches!(
            s.on_gesture(swipe(SwipeDir::North, 200, 1550)),
            Action::Redraw
        ));
        assert!(s.settings);
        // A mid-screen North swipe still exits read mode (unchanged
        // behavior), and does not open the sheet.
        s.settings = false;
        s.control = false;
        assert!(matches!(
            s.on_gesture(swipe(SwipeDir::North, 600, 800)),
            Action::Pop
        ));
        assert!(!s.settings);
    }

    #[test]
    fn on_tick_with_the_sheet_open_holds_settle_polls() {
        let mut s = screen();
        s.settle_deadline = Some(Instant::now() + Duration::from_secs(3));
        s.settings = true;
        // ping_chain is false, so ping_tick is inert; the guard must
        // fire before any frame fetch (which would need a server).
        assert!(matches!(s.on_tick(), Action::Keep));
        assert!(s.settle_deadline.is_some(), "deadline survives the sheet");
    }
}
