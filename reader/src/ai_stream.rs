//! AiStreamScreen — Live AI coding companion & reading dock for Kindle.
//! Connects to yb-mirror's AI streamer on Mac (port 8768 / UDP 8766),
//! formats incoming Assistant turns into crisp book-like pages, and renders
//! headings, code blocks, markdown tables, bullet lists, and live tool status badges.

use std::time::{Duration, Instant};

use crate::protocol::{self, Conn, DEFAULT_PORT, DISCOVER_PORT};
use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect, PX};
use yui::screen::{Action, Screen};
use yui::Orientation;

pub const AI_STREAM_PORT: u16 = 8768;
const CONF_PATH: &str = "/mnt/us/extensions/mirror/mirror.conf";

const PAD_PT: f32 = 18.0;
const HEADER_TOP_PT: f32 = 16.0;
const HEADER_H_PT: f32 = 36.0;
const FOOTER_H_PT: f32 = 28.0;

const PROMPT_SIZE_PT: f32 = 8.5;
const BODY_SIZE_PT: f32 = 9.0;
const CODE_SIZE_PT: f32 = 8.0;
const H1_SIZE_PT: f32 = 13.0;
const H2_SIZE_PT: f32 = 11.0;
const H3_SIZE_PT: f32 = 10.0;
const FOOTER_SIZE_PT: f32 = 7.0;

const INK: u8 = 0;
const DIM: u8 = 130;
const BORDER: u8 = 180;
const CODE_BG: u8 = 248;

#[derive(Debug, Clone)]
pub enum Block {
    Heading { level: usize, text: String },
    Paragraph { text: String },
    Code { lang: String, lines: Vec<String> },
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
    List { items: Vec<String> },
    Alert { kind: String, text: String },
}

#[derive(Debug, Clone)]
pub struct Turn {
    #[allow(dead_code)]
    pub id: String,
    pub assistant: String,
    pub prompt: String,
    pub timestamp: String,
    #[allow(dead_code)]
    pub status: String,
    pub tool_status: Option<String>,
    pub revision: u64,
    pub blocks: Vec<Block>,
}

impl Default for Turn {
    fn default() -> Self {
        Turn {
            id: "idle".to_string(),
            assistant: "AI Companion".to_string(),
            prompt: "Connecting to Mac...".to_string(),
            timestamp: "--:--".to_string(),
            status: "idle".to_string(),
            tool_status: None,
            revision: 0,
            blocks: vec![
                Block::Heading {
                    level: 2,
                    text: "Waiting for AI stream".to_string(),
                },
                Block::Paragraph {
                    text: "Start the AI Stream in the yb-mirror menu bar on your Mac, or run `claude` / `agy` in your terminal.".to_string(),
                },
            ],
        }
    }
}

/// A pre-computed page layout slice.
#[derive(Debug, Clone)]
pub struct PageLayout {
    pub items: Vec<RenderItem>,
}

#[derive(Debug, Clone)]
pub enum RenderItem {
    Heading { y: i32, level: usize, text: String },
    TextLine { x: i32, y: i32, size: f32, color: u8, text: String },
    BulletLine { x: i32, y: i32, size: f32, color: u8, text: String, is_bullet: bool },
    CodeBox { r: Rect, lang: String, lines: Vec<String> },
    TableRow { y: i32, col_widths: Vec<i32>, cells: Vec<String>, is_header: bool },
    AlertBox { r: Rect, kind: String, lines: Vec<String> },
}

pub struct AiStreamScreen {
    host: Option<String>,
    port: u16,
    turn: Turn,
    pages: Vec<PageLayout>,
    cur_page: usize,
    last_rev: u64,
    last_poll: Instant,
    w: i32,
    h: i32,
    connected: bool,
    pub sheet_open: bool,
    pub source_mode: String,
    pub turn_idx: Option<i32>,
}

impl AiStreamScreen {
    pub fn new(w: u32, h: u32) -> AiStreamScreen {
        let (host, port) = Self::load_config().unwrap_or((None, AI_STREAM_PORT));
        let mut s = AiStreamScreen {
            host,
            port,
            turn: Turn::default(),
            pages: Vec::new(),
            cur_page: 0,
            last_rev: 0,
            last_poll: Instant::now() - Duration::from_secs(10),
            w: w as i32,
            h: h as i32,
            connected: false,
            sheet_open: false,
            source_mode: "auto".to_string(),
            turn_idx: None,
        };
        s.repaginate();
        s
    }

    fn load_config() -> Option<(Option<String>, u16)> {
        let content = std::fs::read_to_string(CONF_PATH).ok()?;
        let mut host = None;
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("SERVER=") {
                let val = val.trim().trim_start_matches("http://");
                let h = val.split(':').next().unwrap_or("").to_string();
                if !h.is_empty() {
                    host = Some(h);
                }
            }
        }
        Some((host, AI_STREAM_PORT))
    }

    fn save_host(host: &str) {
        let _ = std::fs::create_dir_all("/mnt/us/extensions/mirror");
        let content = format!("SERVER=http://{}:{}\n", host, AI_STREAM_PORT);
        let _ = std::fs::write(CONF_PATH, content);
    }

    /// Change AI source on Mac server and reload.
    pub fn set_source(&mut self, source: &str) -> Action {
        self.source_mode = source.to_string();
        self.turn_idx = None;
        let host = match &self.host {
            Some(h) => h.clone(),
            None => return Action::Keep,
        };
        let mut conn = Conn::new(&host, self.port);
        let path = format!("/source?set={}", source);
        let _ = conn.request("POST", &path, &mut |_| true);
        self.last_rev = 0; // force reload
        self.poll_server();
        Action::RedrawFull
    }

    /// Jump to previous or next turn in history.
    pub fn step_turn(&mut self, delta: i32) -> Action {
        let host = match &self.host {
            Some(h) => h.clone(),
            None => return Action::Keep,
        };
        let mut conn = Conn::new(&host, self.port);
        
        let target_idx = match self.turn_idx {
            None => {
                if delta < 0 { -2 } else { -1 }
            }
            Some(cur) => cur + delta,
        };

        let path = format!("/turn?idx={}", target_idx);
        let mut body = Vec::new();
        if let Ok(resp) = conn.request("GET", &path, &mut |chunk| {
            body.extend_from_slice(chunk);
            true
        }) {
            if resp.status == 200 {
                if let Ok(turn_data) = parse_json_turn(&body) {
                    self.turn_idx = Some(target_idx);
                    self.turn = turn_data;
                    self.cur_page = 0;
                    self.repaginate();
                    return Action::RedrawFull;
                }
            }
        }
        Action::Keep
    }

    /// Return to live latest turn.
    pub fn go_live(&mut self) -> Action {
        self.turn_idx = None;
        self.last_rev = 0;
        self.poll_server();
        Action::RedrawFull
    }

    /// Poll the HTTP streamer on the Mac for updates.
    fn poll_server(&mut self) -> bool {
        if self.turn_idx.is_some() {
            return false; // Viewing historical turn; do not overwrite with live poll
        }

        // 1. Discover if no host is configured
        if self.host.is_none() {
            if let Some((h, _)) = protocol::discover(Duration::from_millis(600)) {
                Self::save_host(&h);
                self.host = Some(h);
            } else {
                return false;
            }
        }

        let host = match &self.host {
            Some(h) => h.clone(),
            None => return false,
        };

        let mut conn = Conn::new(&host, self.port);
        let mut body = Vec::new();
        let resp = match conn.request("GET", "/live", &mut |chunk| {
            body.extend_from_slice(chunk);
            true
        }) {
            Ok(r) => r,
            Err(_) => {
                // If direct port fails, attempt discovery
                if let Some((new_host, p)) = protocol::discover(Duration::from_millis(500)) {
                    self.host = Some(new_host);
                    self.port = if p != DEFAULT_PORT && p != DISCOVER_PORT { p } else { AI_STREAM_PORT };
                }
                self.connected = false;
                return false;
            }
        };

        if resp.status != 200 {
            self.connected = false;
            return false;
        }

        self.connected = true;
        if let Ok(turn_data) = parse_json_turn(&body) {
            if turn_data.revision != self.last_rev {
                let was_last = self.cur_page + 1 >= self.pages.len();
                self.last_rev = turn_data.revision;
                self.turn = turn_data;
                self.repaginate();
                if was_last {
                    self.cur_page = self.pages.len().saturating_sub(1);
                } else {
                    self.cur_page = self.cur_page.min(self.pages.len().saturating_sub(1));
                }
                return true;
            }
        }

        false
    }

    /// Break blocks into page layouts based on vertical coordinates.
    pub fn repaginate(&mut self) {
        let (w, h) = (self.w, self.h);
        let pad = pt(PAD_PT);
        let p1_top = pt(HEADER_TOP_PT + HEADER_H_PT + 8.0);
        let p2_top = pt(HEADER_TOP_PT + 20.0);
        let bottom_bound = h - pt(FOOTER_H_PT + 6.0);
        let content_w = w - 2 * pad;

        let mut pages = Vec::new();
        let mut cur_items = Vec::new();
        let mut cur_y = p1_top;

        for block in &self.turn.blocks {
            match block {
                Block::Heading { level, text } => {
                    let size = match level {
                        1 => H1_SIZE_PT,
                        2 => H2_SIZE_PT,
                        _ => H3_SIZE_PT,
                    };
                    let heading_h = pt(size + 10.0);
                    if cur_y + heading_h > bottom_bound && !cur_items.is_empty() {
                        pages.push(PageLayout { items: cur_items });
                        cur_items = Vec::new();
                        cur_y = p2_top;
                    }
                    cur_y += pt(4.0);
                    cur_items.push(RenderItem::Heading {
                        y: cur_y,
                        level: *level,
                        text: clean_inline_markdown(text),
                    });
                    cur_y += heading_h;
                }

                Block::Paragraph { text } => {
                    let lines = wrap_words(text, content_w, BODY_SIZE_PT);
                    let line_h = pt(BODY_SIZE_PT + 5.5);
                    for line in lines {
                        if cur_y + line_h > bottom_bound && !cur_items.is_empty() {
                            pages.push(PageLayout { items: cur_items });
                            cur_items = Vec::new();
                            cur_y = p2_top;
                        }
                        cur_items.push(RenderItem::TextLine {
                            x: pad,
                            y: cur_y + pt(BODY_SIZE_PT),
                            size: BODY_SIZE_PT,
                            color: INK,
                            text: line,
                        });
                        cur_y += line_h;
                    }
                    cur_y += pt(6.0);
                }

                Block::Code { lang, lines } => {
                    let line_h = pt(CODE_SIZE_PT + 4.0);
                    let header_h = pt(14.0);
                    let box_pad = pt(8.0);
                    
                    let mut i = 0;
                    while i < lines.len() {
                        let remaining_h = bottom_bound - cur_y;
                        if remaining_h < header_h + line_h + 2 * box_pad && !cur_items.is_empty() {
                            pages.push(PageLayout { items: cur_items });
                            cur_items = Vec::new();
                            cur_y = p2_top;
                        }

                        let available_lines = ((bottom_bound - cur_y - header_h - 2 * box_pad) / line_h).max(1) as usize;
                        let chunk_len = available_lines.min(lines.len() - i);
                        let chunk = lines[i..i + chunk_len].to_vec();
                        let box_h = header_h + chunk.len() as i32 * line_h + box_pad;

                        let r = Rect::new(pad, cur_y, content_w, box_h);
                        cur_items.push(RenderItem::CodeBox {
                            r,
                            lang: if i == 0 { lang.clone() } else { format!("{} (cont)", lang) },
                            lines: chunk,
                        });

                        cur_y += box_h + pt(8.0);
                        i += chunk_len;
                    }
                }

                Block::Table { headers, rows } => {
                    let n_cols = headers.len().max(1);
                    let mut max_lens = vec![1usize; n_cols];
                    for (c, h) in headers.iter().enumerate() {
                        max_lens[c] = max_lens[c].max(h.len());
                    }
                    for row in rows {
                        for (c, cell) in row.iter().enumerate() {
                            if c < n_cols {
                                max_lens[c] = max_lens[c].max(cell.len());
                            }
                        }
                    }
                    let total_len: usize = max_lens.iter().sum::<usize>().max(1);
                    let col_widths: Vec<i32> = max_lens
                        .iter()
                        .map(|l| {
                            ((content_w as f32 * (*l as f32 / total_len as f32)).round() as i32)
                                .max(pt(30.0))
                        })
                        .collect();
                    let row_h = pt(18.0);

                    if cur_y + row_h > bottom_bound && !cur_items.is_empty() {
                        pages.push(PageLayout { items: cur_items });
                        cur_items = Vec::new();
                        cur_y = p2_top;
                    }

                    cur_items.push(RenderItem::TableRow {
                        y: cur_y,
                        col_widths: col_widths.clone(),
                        cells: headers.iter().map(|h| clean_inline_markdown(h)).collect(),
                        is_header: true,
                    });
                    cur_y += row_h;

                    for row in rows {
                        if cur_y + row_h > bottom_bound && !cur_items.is_empty() {
                            pages.push(PageLayout { items: cur_items });
                            cur_items = Vec::new();
                            cur_y = p2_top;
                        }
                        cur_items.push(RenderItem::TableRow {
                            y: cur_y,
                            col_widths: col_widths.clone(),
                            cells: row.iter().map(|c| clean_inline_markdown(c)).collect(),
                            is_header: false,
                        });
                        cur_y += row_h;
                    }
                    cur_y += pt(6.0);
                }

                Block::List { items } => {
                    let line_h = pt(BODY_SIZE_PT + 5.0);
                    for item in items {
                        let lines = wrap_words(item, content_w - pt(18.0), BODY_SIZE_PT);
                        for (idx, line) in lines.iter().enumerate() {
                            if cur_y + line_h > bottom_bound && !cur_items.is_empty() {
                                pages.push(PageLayout { items: cur_items });
                                cur_items = Vec::new();
                                cur_y = p2_top;
                            }
                            cur_items.push(RenderItem::BulletLine {
                                x: pad + pt(14.0),
                                y: cur_y + pt(BODY_SIZE_PT),
                                size: BODY_SIZE_PT,
                                color: INK,
                                text: line.clone(),
                                is_bullet: idx == 0,
                            });
                            cur_y += line_h;
                        }
                    }
                    cur_y += pt(4.0);
                }

                Block::Alert { kind, text } => {
                    let lines = wrap_words(text, content_w - pt(18.0), BODY_SIZE_PT);
                    let line_h = pt(BODY_SIZE_PT + 4.0);
                    let box_h = lines.len() as i32 * line_h + pt(10.0);

                    if cur_y + box_h > bottom_bound && !cur_items.is_empty() {
                        pages.push(PageLayout { items: cur_items });
                        cur_items = Vec::new();
                        cur_y = p2_top;
                    }

                    cur_items.push(RenderItem::AlertBox {
                        r: Rect::new(pad, cur_y, content_w, box_h),
                        kind: kind.clone(),
                        lines,
                    });
                    cur_y += box_h + pt(8.0);
                }
            }
        }

        if !cur_items.is_empty() || pages.is_empty() {
            pages.push(PageLayout { items: cur_items });
        }

        self.pages = pages;
    }

    pub fn draw_sheet(&self, p: &mut Painter) {
        let (w, h) = (self.w, self.h);
        let pad = pt(PAD_PT);
        let sheet_h = pt(118.0);
        let sheet_y = h - sheet_h;

        // 1. Dim background
        for y in (0..sheet_y).step_by(2) {
            p.hline_t(y, 0, w, 1, 200);
        }

        // 2. Sheet card
        p.rect(Rect::new(0, sheet_y, w, sheet_h), 255);
        p.hline_t(sheet_y - 2, 0, w, 1, 150);
        p.hline_t(sheet_y - 1, 0, w, 1, 100);
        p.hline_t(sheet_y, 0, w, 2, INK);

        // 3. Grab handle
        let hw = pt(28.0);
        p.rect(Rect::new((w - hw) / 2, sheet_y + pt(4.0), hw, pt(2.5)), 170);

        // Section 1: SOURCE SELECTOR
        let y1 = sheet_y + pt(15.0);
        p.text(pad, y1 + pt(10.0), 6.5, DIM, "SOURCE");
        let btn_h = pt(15.0);

        let sources = [
            ("auto", "Auto (Latest)", pt(44.0), pt(56.0)),
            ("antigravity", "Antigravity", pt(104.0), pt(62.0)),
            ("claude", "Claude Code", pt(170.0), pt(62.0)),
        ];

        for (id, label, off_x, bw) in sources {
            let r = Rect::new(pad + off_x, y1, bw, btn_h);
            let is_sel = self.source_mode == id;
            if is_sel {
                p.rect(r, INK);
                p.text(r.x + pt(6.0), r.y + pt(10.5), 7.0, 255, label);
            } else {
                p.rect_outline_t(r, 1, BORDER);
                p.text(r.x + pt(6.0), r.y + pt(10.5), 7.0, INK, label);
            }
        }

        // Section 2: TURN NAVIGATOR
        let y2 = sheet_y + pt(48.0);
        p.text(pad, y2 + pt(10.0), 6.5, DIM, "TURNS");
        
        let turn_btns = [
            ("< Prev Turn", pt(44.0), pt(56.0)),
            ("Next Turn >", pt(104.0), pt(56.0)),
            ("Live Latest", pt(164.0), pt(58.0)),
        ];

        for (idx, (label, off_x, bw)) in turn_btns.iter().enumerate() {
            let r = Rect::new(pad + *off_x, y2, *bw, btn_h);
            let is_live_active = idx == 2 && self.turn_idx.is_none();
            if is_live_active {
                p.rect(r, 240);
                p.rect_outline_t(r, 1, INK);
            } else {
                p.rect_outline_t(r, 1, BORDER);
            }
            p.text(r.x + pt(6.0), r.y + pt(10.5), 7.0, INK, label);
        }

        // Section 3: ACTIONS
        let y3 = sheet_y + pt(80.0);
        p.text(pad, y3 + pt(10.0), 6.5, DIM, "ACTIONS");
        
        let act_btns = [
            ("Poll Now", pt(44.0), pt(48.0)),
            ("Clear Ghosting", pt(96.0), pt(68.0)),
            ("Done", pt(168.0), pt(42.0)),
        ];

        for (label, off_x, bw) in act_btns {
            let r = Rect::new(pad + off_x, y3, bw, btn_h);
            p.rect_outline_t(r, 1, BORDER);
            p.text(r.x + pt(6.0), r.y + pt(10.5), 7.0, INK, label);
        }
    }

    pub fn hit_sheet(&mut self, x: i32, y: i32) -> Action {
        let pad = pt(PAD_PT);
        let sheet_h = pt(118.0);
        let sheet_y = self.h - sheet_h;

        if y < sheet_y {
            self.sheet_open = false;
            return Action::Redraw;
        }

        let btn_h = pt(15.0);
        let y1 = sheet_y + pt(15.0);
        let y2 = sheet_y + pt(48.0);
        let y3 = sheet_y + pt(80.0);

        // Row 1: Source
        if y >= y1 && y < y1 + btn_h {
            if x >= pad + pt(44.0) && x < pad + pt(100.0) {
                return self.set_source("auto");
            }
            if x >= pad + pt(104.0) && x < pad + pt(166.0) {
                return self.set_source("antigravity");
            }
            if x >= pad + pt(170.0) && x < pad + pt(232.0) {
                return self.set_source("claude");
            }
        }

        // Row 2: Turn History
        if y >= y2 && y < y2 + btn_h {
            if x >= pad + pt(44.0) && x < pad + pt(100.0) {
                return self.step_turn(-1);
            }
            if x >= pad + pt(104.0) && x < pad + pt(160.0) {
                return self.step_turn(1);
            }
            if x >= pad + pt(164.0) && x < pad + pt(222.0) {
                return self.go_live();
            }
        }

        // Row 3: Actions
        if y >= y3 && y < y3 + btn_h {
            if x >= pad + pt(44.0) && x < pad + pt(92.0) {
                self.poll_server();
                return Action::Redraw;
            }
            if x >= pad + pt(96.0) && x < pad + pt(164.0) {
                return Action::RedrawFull;
            }
            if x >= pad + pt(168.0) && x < pad + pt(210.0) {
                self.sheet_open = false;
                return Action::Redraw;
            }
        }

        Action::Keep
    }
}

impl Screen for AiStreamScreen {
    fn default_edges(&self) -> bool {
        true
    }

    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::Portrait)
    }

    fn on_enter(&mut self) -> Action {
        self.poll_server();
        Action::RedrawFull
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(1500)
    }

    fn on_tick(&mut self) -> Action {
        if self.last_poll.elapsed() >= Duration::from_millis(1400) {
            self.last_poll = Instant::now();
            if self.poll_server() {
                return Action::Redraw;
            }
        }
        Action::Keep
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        let pad = pt(PAD_PT);
        p.clear(255);

        // 1. Ambient Clock / Status Header
        let time_str = crate::chrome::current_time_str();
        p.text(pad, pt(10.0), 6.5, DIM, &time_str);
        
        let status_tag = if self.connected {
            if let Some(tool) = &self.turn.tool_status {
                format!("[RUN] {}", tool)
            } else {
                format!("* {}", self.turn.assistant)
            }
        } else {
            "Offline (waiting for Mac)".to_string()
        };
        p.text_right(w - pad, pt(10.0), 6.5, if self.connected { INK } else { DIM }, &status_tag);

        // 2. User Prompt Banner Card (page 1 hero card, page 2+ compact crumb)
        if self.cur_page == 0 {
            let prompt_r = Rect::new(pad, pt(HEADER_TOP_PT), w - 2 * pad, pt(HEADER_H_PT));
            p.rect(prompt_r, 248);
            p.rect_outline_t(prompt_r, 1, BORDER);
            p.rect(Rect::new(prompt_r.x, prompt_r.y, pt(3.0), prompt_r.h), INK);
            p.text(prompt_r.x + pt(8.0), prompt_r.y + pt(12.0), 6.5, DIM, "PROMPT");
            let prompt_clean = sanitize_single_line(&self.turn.prompt);
            let prompt_truncated = p.truncate(PROMPT_SIZE_PT, &prompt_clean, (prompt_r.w - pt(18.0)) as f32);
            p.text(prompt_r.x + pt(8.0), prompt_r.y + pt(26.0), PROMPT_SIZE_PT, INK, &prompt_truncated);
        } else {
            let prompt_clean = sanitize_single_line(&self.turn.prompt);
            let crumb = format!("Q: {}", p.truncate(7.0, &prompt_clean, (w - 2 * pad) as f32));
            p.text(pad, pt(HEADER_TOP_PT + 8.0), 7.0, DIM, &crumb);
            p.hline_t(pt(HEADER_TOP_PT + 14.0), pad, w - pad, 1, 230);
        }

        // 3. Render Page Items
        if let Some(page) = self.pages.get(self.cur_page) {
            for item in &page.items {
                match item {
                    RenderItem::Heading { y, level, text } => {
                        let size = match level {
                            1 => H1_SIZE_PT,
                            2 => H2_SIZE_PT,
                            _ => H3_SIZE_PT,
                        };
                        p.text(pad, *y, size, INK, text);
                        if *level == 1 {
                            p.hline_t(y + pt(size + 2.0), pad, w - pad, 2, BORDER);
                        }
                    }

                    RenderItem::TextLine { x, y, size, color, text } => {
                        p.text(*x, *y, *size, *color, text);
                    }

                    RenderItem::BulletLine { x, y, size, color, text, is_bullet } => {
                        if *is_bullet {
                            p.circle_fill(x - pt(7.0), y - pt(3.5), pt(1.4), INK);
                        }
                        p.text(*x, *y, *size, *color, text);
                    }

                    RenderItem::CodeBox { r, lang, lines } => {
                        p.rect(*r, CODE_BG);
                        p.rect_outline_t(*r, 1, BORDER);
                        p.text_right(r.x + r.w - pt(6.0), r.y + pt(9.5), 6.5, DIM, lang);
                        let line_h = pt(CODE_SIZE_PT + 4.0);
                        let mut cy = r.y + pt(14.0);
                        for code_l in lines {
                            p.text(r.x + pt(8.0), cy + pt(CODE_SIZE_PT), CODE_SIZE_PT, INK, code_l);
                            cy += line_h;
                        }
                    }

                    RenderItem::TableRow { y, col_widths, cells, is_header } => {
                        let mut cx = pad;
                        for (i, cell) in cells.iter().enumerate() {
                            let cw = col_widths.get(i).copied().unwrap_or(100);
                            let cell_txt = p.truncate(BODY_SIZE_PT, cell, (cw - pt(8.0)) as f32);
                            p.text(cx + pt(4.0), y + pt(12.0), BODY_SIZE_PT, if *is_header { INK } else { 40 }, &cell_txt);
                            cx += cw;
                        }
                        if *is_header {
                            p.hline_t(y + pt(16.0), pad, w - pad, 2, INK);
                        } else {
                            p.hline_t(y + pt(16.0), pad, w - pad, 1, 230);
                        }
                    }

                    RenderItem::AlertBox { r, kind, lines } => {
                        p.rect(*r, 248);
                        p.rect(Rect::new(r.x, r.y, pt(3.0), r.h), INK);
                        p.text(r.x + pt(8.0), r.y + pt(10.0), 6.5, INK, kind);
                        let line_h = pt(BODY_SIZE_PT + 4.0);
                        let mut cy = r.y + pt(12.0);
                        for l in lines {
                            p.text(r.x + pt(8.0), cy + pt(BODY_SIZE_PT), BODY_SIZE_PT, INK, l);
                            cy += line_h;
                        }
                    }
                }
            }
        }

        // 4. Footer Pagination Bar
        let footer_y = h - pt(12.0);
        let total_pages = self.pages.len().max(1);
        let page_label = format!("Page {} of {} · {}", self.cur_page + 1, total_pages, self.turn.timestamp);
        p.text_center(footer_y, FOOTER_SIZE_PT, DIM, &page_label);

        // Subdued corner exit bracket (matching reader standard)
        let b = pt(12.0);
        let bracket_color = 200;
        p.hline_t(h - 4, w - b - 4, w - 4, 2, bracket_color);
        p.rect(Rect::new(w - 4, h - b - 4, 2, b), bracket_color);

        // 5. Quick Settings Sheet (when open)
        if self.sheet_open {
            self.draw_sheet(p);
        }
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let (w, h) = (self.w, self.h);
        if g.corner_back() || g.corner_back_in(w as u32, h as u32) {
            return Action::Pop;
        }

        if self.sheet_open {
            match g {
                Gesture::Tap { x, y } => {
                    return self.hit_sheet(x as i32, y as i32);
                }
                Gesture::Swipe { dir: SwipeDir::South, .. } => {
                    self.sheet_open = false;
                    return Action::Redraw;
                }
                _ => {
                    self.sheet_open = false;
                    return Action::Redraw;
                }
            }
        }

        match g {
            // Bottom-left swipe North opens quick settings sheet
            Gesture::Swipe { dir: SwipeDir::North, x, y, .. }
                if (x as i32) < w / 3 && (y as i32) > (h * 2 / 3) =>
            {
                self.sheet_open = true;
                Action::Redraw
            }

            Gesture::Tap { x, y } => {
                let x = x as i32;
                let y = y as i32;

                // Tapping top status header opens quick settings sheet
                if y < pt(HEADER_TOP_PT) + 4 {
                    self.sheet_open = true;
                    return Action::Redraw;
                }

                // Page turns: Left 35% = Prev, Right 35% = Next
                if x > (w * 65) / 100 {
                    if self.cur_page + 1 < self.pages.len() {
                        self.cur_page += 1;
                        return Action::Redraw;
                    }
                } else if x < (w * 35) / 100 {
                    if self.cur_page > 0 {
                        self.cur_page -= 1;
                        return Action::Redraw;
                    }
                } else {
                    // Center tap: Manual Refresh
                    self.poll_server();
                    return Action::RedrawFull;
                }
                Action::Keep
            }

            Gesture::Swipe { dir: SwipeDir::East, .. } => {
                if self.cur_page > 0 {
                    self.cur_page -= 1;
                    Action::Redraw
                } else {
                    Action::Keep
                }
            }

            Gesture::Swipe { dir: SwipeDir::West, .. } => {
                if self.cur_page + 1 < self.pages.len() {
                    self.cur_page += 1;
                    Action::Redraw
                } else {
                    Action::Keep
                }
            }

            Gesture::TwoFingerTap => {
                self.poll_server();
                Action::RedrawFull
            }

            _ => Action::Keep,
        }
    }
}

/// Clean markdown symbols from text (bold, italic, inline code, link syntax).
fn clean_inline_markdown(text: &str) -> String {
    let mut s = text.to_string();
    s = s.replace("**", "").replace("__", "").replace('`', "");
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i..].iter().position(|&c| c == ']') {
                let close_idx = i + close_bracket;
                if close_idx + 1 < chars.len() && chars[close_idx + 1] == '(' {
                    if let Some(close_paren) = chars[close_idx + 1..].iter().position(|&c| c == ')') {
                        let label: String = chars[i + 1..close_idx].iter().collect();
                        out.push_str(&label);
                        i = close_idx + 1 + close_paren + 1;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Sanitize text for a 1-line header (removes newlines, control characters, and metadata suffixes).
fn sanitize_single_line(s: &str) -> String {
    let cleaned = clean_inline_markdown(s);
    let first_line = cleaned.lines().next().unwrap_or(&cleaned);
    let mut out: String = first_line.chars().filter(|c| !c.is_control()).collect();
    if let Some(pos) = out.find("The current local time") {
        out.truncate(pos);
    }
    out.trim().to_string()
}

/// Helper to wrap words onto lines based on pixel budget.
fn wrap_words(text: &str, max_width_px: i32, size_pt: f32) -> Vec<String> {
    let cleaned = clean_inline_markdown(text);
    let mut lines = Vec::new();
    let mut cur_line = String::new();
    let char_w = (size_pt * PX * 0.54).max(1.0);
    let max_chars = ((max_width_px as f32) / char_w).max(10.0) as usize;

    for word in cleaned.split_whitespace() {
        if cur_line.is_empty() {
            cur_line = word.to_string();
        } else if cur_line.len() + 1 + word.len() <= max_chars {
            cur_line.push(' ');
            cur_line.push_str(word);
        } else {
            lines.push(cur_line);
            cur_line = word.to_string();
        }
    }
    if !cur_line.is_empty() {
        lines.push(cur_line);
    }
    lines
}

/// Parse JSON turn payload from server.
fn parse_json_turn(bytes: &[u8]) -> Result<Turn, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    
    // Minimal custom parser / serde-free extractor for embedded efficiency
    let id = extract_json_str(s, "id").unwrap_or_else(|| "turn_0".to_string());
    let assistant = extract_json_str(s, "assistant").unwrap_or_else(|| "Assistant".to_string());
    let prompt = extract_json_str(s, "prompt").unwrap_or_default();
    let timestamp = extract_json_str(s, "timestamp").unwrap_or_else(|| "--:--".to_string());
    let status = extract_json_str(s, "status").unwrap_or_else(|| "idle".to_string());
    let tool_status = extract_json_str(s, "tool_status");
    let revision = extract_json_u64(s, "revision").unwrap_or(0);
    let raw_md = extract_json_str(s, "raw_markdown").unwrap_or_default();

    let mut blocks = Vec::new();
    if !raw_md.is_empty() {
        blocks = parse_md_to_blocks(&raw_md);
    }

    Ok(Turn {
        id,
        assistant,
        prompt,
        timestamp,
        status,
        tool_status,
        revision,
        blocks,
    })
}

fn extract_json_str(json: &str, key: &str) -> Option<String> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let chars: Vec<char> = rest[1..].chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut escaped = false;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            match c {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                _ => { out.push('\\'); out.push(c); }
            }
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
        i += 1;
    }
    Some(out)
}

fn extract_json_u64(json: &str, key: &str) -> Option<u64> {
    let pattern = format!("\"{}\":", key);
    let idx = json.find(&pattern)?;
    let rest = json[idx + pattern.len()..].trim_start();
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

fn parse_md_to_blocks(md: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        if line.starts_with("```") {
            let lang = line.trim_start_matches('`').trim().to_string();
            let mut code_lines = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim().starts_with("```") {
                code_lines.push(lines[i].to_string());
                i += 1;
            }
            blocks.push(Block::Code {
                lang: if lang.is_empty() { "code".to_string() } else { lang },
                lines: code_lines,
            });
            i += 1;
            continue;
        }

        if line.starts_with('#') {
            let level = line.chars().take_while(|&c| c == '#').count();
            let text = line.trim_start_matches('#').trim().to_string();
            blocks.push(Block::Heading { level, text });
            i += 1;
            continue;
        }

        if line.starts_with('>') {
            let mut text = String::new();
            let mut kind = "NOTE".to_string();
            while i < lines.len() && lines[i].trim().starts_with('>') {
                let cleaned = lines[i].trim().trim_start_matches('>').trim();
                if cleaned.starts_with("[!") && cleaned.contains(']') {
                    let end = cleaned.find(']').unwrap();
                    kind = cleaned[2..end].to_uppercase();
                } else {
                    if !text.is_empty() { text.push(' '); }
                    text.push_str(cleaned);
                }
                i += 1;
            }
            blocks.push(Block::Alert { kind, text });
            continue;
        }

        let is_list = |l: &str| -> bool {
            let t = l.trim();
            t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") ||
            (t.len() > 2 && t.chars().next().map_or(false, |c| c.is_ascii_digit()) && t[1..].starts_with(". "))
        };

        if is_list(line) {
            let mut items = Vec::new();
            while i < lines.len() && is_list(lines[i]) {
                items.push(strip_list_prefix(lines[i]));
                i += 1;
            }
            blocks.push(Block::List { items });
            continue;
        }

        if line.contains('|') && i + 1 < lines.len() && lines[i+1].contains('|') && lines[i+1].contains('-') {
            let headers: Vec<String> = line.trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].contains('|') && !lines[i].trim().is_empty() {
                let row: Vec<String> = lines[i].trim_matches('|').split('|').map(|c| c.trim().to_string()).collect();
                rows.push(row);
                i += 1;
            }
            blocks.push(Block::Table { headers, rows });
            continue;
        }

        if !line.is_empty() {
            let mut para = line.to_string();
            i += 1;
            while i < lines.len() && !lines[i].trim().is_empty() && !lines[i].trim().starts_with('#') && !lines[i].trim().starts_with("```") && !lines[i].trim().starts_with('>') && !is_list(lines[i]) {
                para.push(' ');
                para.push_str(lines[i].trim());
                i += 1;
            }
            blocks.push(Block::Paragraph { text: para });
            continue;
        }

        i += 1;
    }

    blocks
}

fn strip_list_prefix(s: &str) -> String {
    let t = s.trim();
    if t.starts_with("- ") || t.starts_with("* ") || t.starts_with("+ ") {
        t[2..].trim().to_string()
    } else if let Some(dot_idx) = t.find(". ") {
        if t[..dot_idx].chars().all(|c| c.is_ascii_digit()) {
            t[dot_idx + 2..].trim().to_string()
        } else {
            t.to_string()
        }
    } else {
        t.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_md_parser_headings_and_code() {
        let md = "# Title\n\nSome paragraph text here.\n\n```rust\nfn main() {}\n```\n";
        let blocks = parse_md_to_blocks(md);
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            Block::Heading { level, text } => {
                assert_eq!(*level, 1);
                assert_eq!(text, "Title");
            }
            _ => panic!("expected heading"),
        }
        match &blocks[2] {
            Block::Code { lang, lines } => {
                assert_eq!(lang, "rust");
                assert_eq!(lines, &vec!["fn main() {}".to_string()]);
            }
            _ => panic!("expected code"),
        }
    }

    #[test]
    fn renders_ai_stream_device_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        s.turn = Turn {
            id: "turn_sample".to_string(),
            assistant: "Antigravity".to_string(),
            prompt: "Can we use Kindle as a second monitor for reading AI turns?".to_string(),
            timestamp: "20:25".to_string(),
            status: "idle".to_string(),
            tool_status: Some("Running cargo test...".to_string()),
            revision: 1,
            blocks: vec![
                Block::Heading {
                    level: 2,
                    text: "Kindle Live AI Companion Stream".to_string(),
                },
                Block::Paragraph {
                    text: "A long AI response paginated into 1236x1648 book pages for e-ink.".to_string(),
                },
                Block::Alert {
                    kind: "TIP".to_string(),
                    text: "Tap the right side of the screen to advance pages, left side to go back, or swipe bottom-right to return to the library.".to_string(),
                },
                Block::Code {
                    lang: "rust".to_string(),
                    lines: vec![
                        "// Kindle E-Ink Streamer Loop".to_string(),
                        "fn poll_live_stream() -> Result<Turn, Error> {".to_string(),
                        "    let resp = conn.request(\"GET\", \"/live\", &mut sink)?;".to_string(),
                        "    Ok(parse_json_turn(&resp))".to_string(),
                        "}".to_string(),
                    ],
                },
                Block::Table {
                    headers: vec!["Feature".to_string(), "Kindle Mode".to_string(), "Status".to_string()],
                    rows: vec![
                        vec!["Pagination".to_string(), "Auto 1236x1648".to_string(), "Active".to_string()],
                        vec!["Code Highlighting".to_string(), "Monospace Box".to_string(), "Active".to_string()],
                        vec!["Discovery".to_string(), "UDP Broadcast :8766".to_string(), "Active".to_string()],
                    ],
                },
            ],
        };
        s.repaginate();

        let mut canvas = vec![255u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        s.draw(&mut p);

        let artifact_path = "/tmp/dev_artifacts/ai_stream_device_preview.png";
        let file = std::fs::File::create(artifact_path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&canvas)
            .unwrap();
    }

    #[test]
    fn renders_markdown_cleanup_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "Example of a parsed turn after markdown cleanup.\n\n### Visual Improvements to the AI Stream:\n\n1. **Fixed Broken Glyphs on Bullets**:\n   * Replaced Unicode bullets (`•` which were rendering as missing-glyph placeholder squares) with crisp 3px solid ink disks drawn directly via `circle_fill`.\n2. **Inline Markdown Cleaning**:\n   * Raw formatting syntax (`**bold**`, `` `code` ``, `[link](url)`) is now cleanly stripped and normalized into readable text.\n3. **Clean Prompt Header**:\n   * On **Page 1**: An Apple/Nordic style prompt card with a solid left accent stripe and clean typography.\n   * On **Page 2+**: Shrinks into a minimal 1-line breadcrumb (`Q: ...`) so you have 100% of the screen height for reading the response.\n4. **Metadata & Timestamp Filtering**:\n   * Prompt text is automatically cleaned of system wrapper tags (`<USER_REQUEST>`, timestamps, etc.).\n5. **Code Boxes & Tables**:\n   * Monospace boxes have light 248-gray fill, border outlines, and language badges (e.g. `rust`, `python`).";
        
        let json_payload = format!(
            "{{\"id\":\"turn_101\",\"assistant\":\"Antigravity\",\"prompt\":\"markdown cleanup sample\\n\\n\\n\",\"timestamp\":\"20:46\",\"status\":\"idle\",\"tool_status\":null,\"revision\":5,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );

        let turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        s.turn = turn;
        s.repaginate();

        assert!(s.pages.len() >= 1);
        assert!(!s.turn.blocks.is_empty());

        let mut canvas = vec![255u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        s.draw(&mut p);

        let artifact_path = "/tmp/dev_artifacts/markdown_cleanup_preview.png";
        let file = std::fs::File::create(artifact_path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&canvas)
            .unwrap();
    }

    #[test]
    fn renders_quick_settings_sheet_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        s.sheet_open = true;
        s.source_mode = "antigravity".to_string();
        s.turn = Turn {
            id: "turn_sample".to_string(),
            assistant: "Antigravity".to_string(),
            prompt: "how do i switch between antigravity, claude code and everything else?".to_string(),
            timestamp: "20:58".to_string(),
            status: "idle".to_string(),
            tool_status: None,
            revision: 2,
            blocks: vec![
                Block::Heading {
                    level: 2,
                    text: "Quick Settings & Source Selector".to_string(),
                },
                Block::Paragraph {
                    text: "Swipe up from the bottom-left corner of the screen to open the Quick Settings sheet anytime.".to_string(),
                },
            ],
        };
        s.repaginate();

        let mut canvas = vec![255u8; 1236 * 1648];
        let mut panel = vec![255u8; 1248 * 1648];
        let mut p = yui::Painter::new(
            &mut panel,
            1236,
            1648,
            1248,
            yui::Orientation::Portrait,
            &mut canvas,
            &font,
        );
        s.draw(&mut p);

        let artifact_path = "/tmp/dev_artifacts/ai_quick_settings_preview.png";
        let file = std::fs::File::create(artifact_path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header()
            .unwrap()
            .write_image_data(&canvas)
            .unwrap();
    }
}

