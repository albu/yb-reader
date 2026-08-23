//! AiStreamScreen — Live AI coding companion & reading dock for Kindle.
//! Connects to yb-mirror's AI streamer on Mac (port 8768 / UDP 8766),
//! formats incoming Assistant turns into crisp book-like pages, and renders
//! headings, code blocks, markdown tables, bullet lists, and live tool status badges.

use std::time::Duration;

use crate::protocol::{self, Conn};
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ybdev::input::{Gesture, SwipeDir};
use yui::painter::{pt, Painter, Rect, PX};
use yui::screen::{Action, Screen};
use yui::Orientation;

#[allow(dead_code)]
pub const AI_STREAM_PORT: u16 = 8768;
#[allow(dead_code)]
const CONF_PATH: &str = "/mnt/us/extensions/mirror/mirror.conf";

const PAD_PT: f32 = 18.0;
const HEADER_TOP_PT: f32 = 24.0;
const HEADER_H_PT: f32 = 36.0;
const FOOTER_H_PT: f32 = 28.0;

const PROMPT_SIZE_PT: f32 = 8.5;
const BODY_SIZE_PT: f32 = 8.5;
const CODE_SIZE_PT: f32 = 7.5;
const H1_SIZE_PT: f32 = 14.0;
const H2_SIZE_PT: f32 = 12.0;
const H3_SIZE_PT: f32 = 10.5;
const FOOTER_SIZE_PT: f32 = 7.0;

const INK: u8 = 0;
const DIM: u8 = 130;
const BORDER: u8 = 180;
const CODE_BG: u8 = 248;

#[derive(Debug, Clone, PartialEq)]
pub struct StyledSpan {
    pub text: String,
    pub is_bold: bool,
    pub is_italic: bool,
    pub is_code: bool,
}

#[derive(Debug, Clone)]
pub struct StyledLine {
    pub spans: Vec<StyledSpan>,
}

#[derive(Debug, Clone)]
pub struct DocListItem {
    pub blocks: Vec<DocBlock>,
}

#[derive(Debug, Clone)]
pub enum DocBlock {
    Heading { level: usize, spans: Vec<StyledSpan> },
    Paragraph { spans: Vec<StyledSpan> },
    Code { lang: String, lines: Vec<String> },
    Alert { kind: String, blocks: Vec<DocBlock> },
    List { is_ordered: bool, start_num: u64, items: Vec<DocListItem> },
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
    HorizontalRule,
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
    #[allow(dead_code)]
    pub tool_status: Option<String>,
    #[allow(dead_code)]
    pub revision: u64,
    pub blocks: Vec<DocBlock>,
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
                DocBlock::Heading {
                    level: 2,
                    spans: vec![StyledSpan { text: "Waiting for AI stream".to_string(), is_bold: true, is_italic: false, is_code: false }],
                },
                DocBlock::Paragraph {
                    spans: vec![
                        StyledSpan { text: "Start the AI Stream in the yb-mirror menu bar on your Mac, or run ".to_string(), is_bold: false, is_italic: false, is_code: false },
                        StyledSpan { text: "claude".to_string(), is_bold: false, is_italic: false, is_code: true },
                        StyledSpan { text: " / ".to_string(), is_bold: false, is_italic: false, is_code: false },
                        StyledSpan { text: "agy".to_string(), is_bold: false, is_italic: false, is_code: true },
                        StyledSpan { text: " in your terminal.".to_string(), is_bold: false, is_italic: false, is_code: false },
                    ],
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
    Heading { y: i32, level: usize, lines: Vec<StyledLine> },
    StyledTextLine { x: i32, y: i32, size: f32, color: u8, line: StyledLine },
    ListMarker { x: i32, y: i32, size: f32, is_bullet: bool, number_str: Option<String> },
    CodeBox { r: Rect, lang: String, lines: Vec<String> },
    TableRow { x: i32, y: i32, w: i32, h: i32, col_xs: Vec<i32>, cell_lines: Vec<Vec<String>>, is_header: bool },
    AlertBox { r: Rect, kind: String },
    Rule { y: i32 },
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum PollerCmd {
    SetSource(String),
    StepTurn(i32),
    GoLive,
    PollNow,
    Stop,
}

#[allow(dead_code)]
#[derive(Debug)]
pub enum PollerMsg {
    TurnUpdate {
        turn: Box<Turn>,
        turn_idx: Option<i32>,
    },
    ConnectionStatus(bool),
}

#[allow(dead_code)]
fn spawn_poller_thread(
    initial_host: Option<String>,
    port: u16,
    cmd_rx: std::sync::mpsc::Receiver<PollerCmd>,
    msg_tx: std::sync::mpsc::Sender<PollerMsg>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut host = initial_host;
        let mut conn: Option<Conn> = None;
        let mut last_rev: u64 = 0;
        let mut current_turn_idx: Option<i32> = None;
        let mut fail_count: u32 = 0;

        loop {
            let sleep_dur = if fail_count > 0 {
                let multiplier = 1u64 << (fail_count.min(5) - 1);
                Duration::from_millis((1500 * multiplier).min(30_000))
            } else {
                Duration::from_millis(1400)
            };

            match cmd_rx.recv_timeout(sleep_dur) {
                Ok(PollerCmd::Stop) => break,
                Ok(PollerCmd::SetSource(mode)) => {
                    if let Some(h) = &host {
                        let mut temp_conn = Conn::new(h, port);
                        let path = format!("/source?set={}", mode);
                        let _ = temp_conn.request("POST", &path, &mut |_| true);
                        current_turn_idx = None;
                        last_rev = 0;
                    }
                }
                Ok(PollerCmd::StepTurn(delta)) => {
                    let target_idx = match current_turn_idx {
                        None => {
                            if delta < 0 {
                                -2
                            } else {
                                -1
                            }
                        }
                        Some(cur) => cur + delta,
                    };
                    if let Some(h) = &host {
                        let mut temp_conn = Conn::new(h, port);
                        let path = format!("/turn?idx={}", target_idx);
                        let mut body = Vec::new();
                        if let Ok(resp) = temp_conn.request("GET", &path, &mut |chunk| {
                            body.extend_from_slice(chunk);
                            true
                        }) {
                            if resp.status == 200 {
                                if let Ok(turn_data) = parse_json_turn(&body) {
                                    current_turn_idx = Some(target_idx);
                                    let _ = msg_tx.send(PollerMsg::TurnUpdate {
                                        turn: Box::new(turn_data),
                                        turn_idx: current_turn_idx,
                                    });
                                    let _ = msg_tx.send(PollerMsg::ConnectionStatus(true));
                                }
                            }
                        }
                    }
                    continue;
                }
                Ok(PollerCmd::GoLive) => {
                    current_turn_idx = None;
                    last_rev = 0;
                }
                Ok(PollerCmd::PollNow) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            }

            if current_turn_idx.is_some() {
                continue;
            }

            if host.is_none() {
                if let Some((h, _)) = protocol::discover(Duration::from_millis(500)) {
                    AiStreamScreen::save_host(&h);
                    host = Some(h);
                } else {
                    fail_count += 1;
                    let _ = msg_tx.send(PollerMsg::ConnectionStatus(false));
                    continue;
                }
            }

            let current_host = match &host {
                Some(h) => h.clone(),
                None => continue,
            };

            let mut body = Vec::new();
            let mut ok = false;

            if let Some(c) = conn.as_mut() {
                if let Ok(resp) = c.request("GET", "/live", &mut |chunk| {
                    body.extend_from_slice(chunk);
                    true
                }) {
                    if resp.status == 200 {
                        ok = true;
                    }
                }
            }

            if !ok {
                let mut fresh_conn = Conn::new(&current_host, port);
                body.clear();
                if let Ok(resp) = fresh_conn.request("GET", "/live", &mut |chunk| {
                    body.extend_from_slice(chunk);
                    true
                }) {
                    if resp.status == 200 {
                        ok = true;
                        conn = Some(fresh_conn);
                    } else {
                        conn = None;
                    }
                } else {
                    conn = None;
                    if let Some((new_h, _p)) = protocol::discover(Duration::from_millis(400)) {
                        host = Some(new_h);
                    }
                }
            }

            if ok {
                fail_count = 0;
                let _ = msg_tx.send(PollerMsg::ConnectionStatus(true));
                if let Ok(turn_data) = parse_json_turn(&body) {
                    if turn_data.revision != last_rev {
                        last_rev = turn_data.revision;
                        let _ = msg_tx.send(PollerMsg::TurnUpdate {
                            turn: Box::new(turn_data),
                            turn_idx: None,
                        });
                    }
                }
            } else {
                fail_count += 1;
                let _ = msg_tx.send(PollerMsg::ConnectionStatus(false));
            }
        }
    })
}

pub struct AiStreamScreen {
    pub turn: Turn,
    pub pages: Vec<PageLayout>,
    pub cur_page: usize,
    pub w: i32,
    pub h: i32,
    pub connected: bool,
    pub sheet_open: bool,
    pub source_mode: String,
    pub turn_idx: Option<i32>,
    cmd_tx: Option<std::sync::mpsc::Sender<PollerCmd>>,
    msg_rx: Option<std::sync::mpsc::Receiver<PollerMsg>>,
}

impl AiStreamScreen {
    pub fn new(w: u32, h: u32) -> AiStreamScreen {
        #[cfg(test)]
        {
            return Self::new_mock(w, h);
        }

        #[cfg(not(test))]
        {
            let (host, port) = Self::load_config().unwrap_or((None, AI_STREAM_PORT));
            let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
            let (msg_tx, msg_rx) = std::sync::mpsc::channel();
            let _ = spawn_poller_thread(host, port, cmd_rx, msg_tx);

            let mut s = AiStreamScreen {
                turn: Turn::default(),
                pages: Vec::new(),
                cur_page: 0,
                w: w as i32,
                h: h as i32,
                connected: false,
                sheet_open: false,
                source_mode: "auto".to_string(),
                turn_idx: None,
                cmd_tx: Some(cmd_tx),
                msg_rx: Some(msg_rx),
            };
            s.repaginate();
            s
        }
    }

    /// Create an offline AiStreamScreen without background poller thread (for offline rendering / tests).
    #[allow(dead_code)]
    pub fn new_mock(w: u32, h: u32) -> AiStreamScreen {
        let mut s = AiStreamScreen {
            turn: Turn::default(),
            pages: Vec::new(),
            cur_page: 0,
            w: w as i32,
            h: h as i32,
            connected: false,
            sheet_open: false,
            source_mode: "auto".to_string(),
            turn_idx: None,
            cmd_tx: None,
            msg_rx: None,
        };
        s.repaginate();
        s
    }

    #[allow(dead_code)]
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

    #[allow(dead_code)]
    fn save_host(host: &str) {
        let _ = std::fs::create_dir_all("/mnt/us/extensions/mirror");
        let content = format!("SERVER=http://{}:{}\n", host, AI_STREAM_PORT);
        let _ = std::fs::write(CONF_PATH, content);
    }

    /// Change AI source on Mac server.
    pub fn set_source(&mut self, source: &str) -> Action {
        self.source_mode = source.to_string();
        self.turn_idx = None;
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(PollerCmd::SetSource(source.to_string()));
        }
        Action::Redraw
    }

    /// Jump to previous or next turn in history.
    pub fn step_turn(&mut self, delta: i32) -> Action {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(PollerCmd::StepTurn(delta));
        }
        Action::Keep
    }

    /// Return to live latest turn.
    pub fn go_live(&mut self) -> Action {
        self.turn_idx = None;
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(PollerCmd::GoLive);
        }
        Action::Redraw
    }

    /// Trigger immediate poll.
    pub fn poll_now(&mut self) {
        if let Some(tx) = &self.cmd_tx {
            let _ = tx.send(PollerCmd::PollNow);
        }
    }

    /// Break blocks into page layouts based on vertical coordinates.
    pub fn repaginate(&mut self) {
        let (w, h) = (self.w, self.h);
        let pad = pt(PAD_PT);
        let content_w = w - 2 * pad;
        let prompt_lines = wrap_prompt_lines(&self.turn.prompt, content_w - pt(18.0), PROMPT_SIZE_PT, 4);
        let prompt_h = prompt_card_height(prompt_lines.len());
        let p1_top = pt(HEADER_TOP_PT) + prompt_h + pt(10.0);
        let p2_top = pt(HEADER_TOP_PT + 22.0);
        let bottom_bound = h - pt(FOOTER_H_PT + 6.0);

        let mut pages = Vec::new();
        let mut cur_items = Vec::new();
        let mut cur_y = p1_top;

        layout_blocks(
            &self.turn.blocks,
            0,
            pad,
            content_w,
            bottom_bound,
            p2_top,
            &mut cur_y,
            &mut cur_items,
            &mut pages,
        );

        if !cur_items.is_empty() || pages.is_empty() {
            pages.push(PageLayout { items: cur_items });
        }

        self.pages = pages;
    }
}

fn layout_blocks(
    blocks: &[DocBlock],
    depth: usize,
    pad: i32,
    content_w: i32,
    bottom_bound: i32,
    p2_top: i32,
    cur_y: &mut i32,
    cur_items: &mut Vec<RenderItem>,
    pages: &mut Vec<PageLayout>,
) {
    for block in blocks {
        match block {
            DocBlock::Heading { level, spans } => {
                let size = match level {
                    1 => H1_SIZE_PT,
                    2 => H2_SIZE_PT,
                    _ => H3_SIZE_PT,
                };
                let lines = wrap_styled_spans(spans, content_w, size);
                let line_h = pt(size + 6.0);
                let top_margin = pt(if *level <= 2 { 14.0 } else { 10.0 });
                let bottom_margin = pt(if *level <= 2 { 8.0 } else { 5.0 });
                let total_h = lines.len() as i32 * line_h + top_margin + bottom_margin;

                if *cur_y + total_h > bottom_bound && !cur_items.is_empty() {
                    pages.push(PageLayout { items: std::mem::take(cur_items) });
                    *cur_y = p2_top;
                }

                *cur_y += top_margin;
                cur_items.push(RenderItem::Heading {
                    y: *cur_y,
                    level: *level,
                    lines,
                });
                *cur_y += total_h - top_margin;
            }

            DocBlock::Paragraph { spans } => {
                let indent_px = pad + (depth as i32) * pt(14.0);
                let max_w = (content_w - (depth as i32) * pt(14.0)).max(pt(50.0));
                let lines = wrap_styled_spans(spans, max_w, BODY_SIZE_PT);
                let line_h = pt(BODY_SIZE_PT + 5.0);

                for line in lines {
                    if *cur_y + line_h > bottom_bound && !cur_items.is_empty() {
                        pages.push(PageLayout { items: std::mem::take(cur_items) });
                        *cur_y = p2_top;
                    }
                    cur_items.push(RenderItem::StyledTextLine {
                        x: indent_px,
                        y: *cur_y + pt(BODY_SIZE_PT),
                        size: BODY_SIZE_PT,
                        color: INK,
                        line,
                    });
                    *cur_y += line_h;
                }
                *cur_y += pt(5.0);
            }

            DocBlock::List { is_ordered, start_num, items } => {
                let mut num = *start_num;
                let line_h = pt(BODY_SIZE_PT + 5.0);

                for item in items {
                    let indent_px = pad + (depth as i32) * pt(14.0);
                    let prefix_w = if *is_ordered { pt(18.0) } else { pt(12.0) };

                    if let Some((first_block, rest_blocks)) = item.blocks.split_first() {
                        match first_block {
                            DocBlock::Paragraph { spans } => {
                                let max_w = (content_w - (depth as i32) * pt(14.0) - prefix_w).max(pt(50.0));
                                let lines = wrap_styled_spans(spans, max_w, BODY_SIZE_PT);

                                for (idx, line) in lines.into_iter().enumerate() {
                                    if *cur_y + line_h > bottom_bound && !cur_items.is_empty() {
                                        pages.push(PageLayout { items: std::mem::take(cur_items) });
                                        *cur_y = p2_top;
                                    }

                                    if idx == 0 {
                                        if *is_ordered {
                                            cur_items.push(RenderItem::ListMarker {
                                                x: indent_px,
                                                y: *cur_y + pt(BODY_SIZE_PT),
                                                size: BODY_SIZE_PT,
                                                is_bullet: false,
                                                number_str: Some(format!("{}.", num)),
                                            });
                                        } else {
                                            cur_items.push(RenderItem::ListMarker {
                                                x: indent_px + pt(5.0),
                                                y: *cur_y + pt(BODY_SIZE_PT),
                                                size: BODY_SIZE_PT,
                                                is_bullet: true,
                                                number_str: None,
                                            });
                                        }
                                    }

                                    cur_items.push(RenderItem::StyledTextLine {
                                        x: indent_px + prefix_w,
                                        y: *cur_y + pt(BODY_SIZE_PT),
                                        size: BODY_SIZE_PT,
                                        color: INK,
                                        line,
                                    });
                                    *cur_y += line_h;
                                }
                            }
                            _ => {
                                if *is_ordered {
                                    cur_items.push(RenderItem::ListMarker {
                                        x: indent_px,
                                        y: *cur_y + pt(BODY_SIZE_PT),
                                        size: BODY_SIZE_PT,
                                        is_bullet: false,
                                        number_str: Some(format!("{}.", num)),
                                    });
                                } else {
                                    cur_items.push(RenderItem::ListMarker {
                                        x: indent_px + pt(5.0),
                                        y: *cur_y + pt(BODY_SIZE_PT),
                                        size: BODY_SIZE_PT,
                                        is_bullet: true,
                                        number_str: None,
                                    });
                                }
                                layout_blocks(&[first_block.clone()], depth + 1, pad, content_w, bottom_bound, p2_top, cur_y, cur_items, pages);
                            }
                        }

                        if !rest_blocks.is_empty() {
                            layout_blocks(rest_blocks, depth + 1, pad, content_w, bottom_bound, p2_top, cur_y, cur_items, pages);
                        }
                    }
                    num += 1;
                    *cur_y += pt(2.0);
                }
                *cur_y += pt(4.0);
            }

            DocBlock::Code { lang, lines } => {
                let indent_px = pad + (depth as i32) * pt(14.0);
                let box_w = (content_w - (depth as i32) * pt(14.0)).max(pt(50.0));
                let line_h = pt(CODE_SIZE_PT + 4.0);
                let header_h = pt(14.0);
                let box_pad = pt(8.0);

                let mut i = 0;
                while i < lines.len() {
                    let remaining_h = bottom_bound - *cur_y;
                    if remaining_h < header_h + line_h + 2 * box_pad && !cur_items.is_empty() {
                        pages.push(PageLayout { items: std::mem::take(cur_items) });
                        *cur_y = p2_top;
                    }

                    let available_lines = ((bottom_bound - *cur_y - header_h - 2 * box_pad) / line_h).max(1) as usize;
                    let chunk_len = available_lines.min(lines.len() - i);
                    let chunk = lines[i..i + chunk_len].to_vec();
                    let box_h = header_h + chunk.len() as i32 * line_h + box_pad;

                    cur_items.push(RenderItem::CodeBox {
                        r: Rect::new(indent_px, *cur_y, box_w, box_h),
                        lang: if i == 0 { lang.clone() } else { format!("{} (cont)", lang) },
                        lines: chunk,
                    });

                    *cur_y += box_h + pt(8.0);
                    i += chunk_len;
                }
            }

            DocBlock::Alert { kind, blocks } => {
                let indent_px = pad + (depth as i32) * pt(14.0);
                let box_w = (content_w - (depth as i32) * pt(14.0)).max(pt(50.0));
                let header_h = pt(14.0);

                // Break to next page if not enough room to start callout card
                if *cur_y + pt(60.0) > bottom_bound && !cur_items.is_empty() {
                    pages.push(PageLayout { items: std::mem::take(cur_items) });
                    *cur_y = p2_top;
                }

                let alert_start_y = *cur_y;
                let alert_idx = cur_items.len();
                cur_items.push(RenderItem::AlertBox {
                    r: Rect::new(indent_px, alert_start_y, box_w, header_h + pt(16.0)),
                    kind: kind.clone(),
                });

                *cur_y += header_h + pt(4.0);

                let start_page_idx = pages.len();
                layout_blocks(blocks, depth + 1, pad + pt(6.0), content_w - pt(12.0), bottom_bound, p2_top, cur_y, cur_items, pages);

                if pages.len() == start_page_idx {
                    let total_h = (*cur_y - alert_start_y + pt(6.0)).max(header_h + pt(16.0));
                    if let Some(RenderItem::AlertBox { r, .. }) = cur_items.get_mut(alert_idx) {
                        r.h = total_h;
                    }
                } else {
                    if let Some(RenderItem::AlertBox { r, .. }) = cur_items.get_mut(alert_idx) {
                        r.h = bottom_bound - alert_start_y;
                    }
                }
                *cur_y += pt(8.0);
            }

            DocBlock::Table { headers, rows } => {
                let indent_px = pad + (depth as i32) * pt(14.0);
                let table_w = (content_w - (depth as i32) * pt(14.0)).max(pt(50.0));
                let n_cols = headers.len().max(rows.iter().map(|r| r.len()).max().unwrap_or(1)).max(1);

                // Compute relative column weights based on clean content length
                let mut col_lens = vec![1usize; n_cols];
                for (c, h) in headers.iter().enumerate() {
                    if c < n_cols {
                        col_lens[c] = col_lens[c].max(clean_inline_markdown(h).len()).max(4);
                    }
                }
                for row in rows {
                    for (c, cell) in row.iter().enumerate() {
                        if c < n_cols {
                            col_lens[c] = col_lens[c].max(clean_inline_markdown(cell).len()).max(4);
                        }
                    }
                }
                let total_len: usize = col_lens.iter().sum::<usize>().max(1);

                // Allocate proportional widths with a floor
                let min_col_w = pt(55.0);
                let mut col_widths: Vec<i32> = col_lens
                    .iter()
                    .map(|l| {
                        ((table_w as f32 * (*l as f32 / total_len as f32)).round() as i32).max(min_col_w)
                    })
                    .collect();

                // Normalize col_widths sum to exactly table_w
                let sum_w: i32 = col_widths.iter().sum();
                if sum_w != table_w && !col_widths.is_empty() {
                    let diff = table_w - sum_w;
                    let last_idx = col_widths.len() - 1;
                    col_widths[last_idx] = (col_widths[last_idx] + diff).max(min_col_w);
                }

                // Compute starting X position for each column
                let mut col_xs = Vec::with_capacity(n_cols);
                let mut running_x = indent_px;
                for w in &col_widths {
                    col_xs.push(running_x);
                    running_x += *w;
                }

                let wrap_row_cells = |cells: &[String]| -> (Vec<Vec<String>>, i32) {
                    let mut cell_lines = Vec::with_capacity(n_cols);
                    let mut max_lines = 1;
                    for (c, cw) in col_widths.iter().enumerate() {
                        let text = cells.get(c).map(|s| s.as_str()).unwrap_or("");
                        let lines = wrap_cell_text(text, *cw - pt(12.0), BODY_SIZE_PT);
                        max_lines = max_lines.max(lines.len());
                        cell_lines.push(lines);
                    }
                    let row_h = (max_lines as i32) * pt(BODY_SIZE_PT + 4.0) + pt(8.0);
                    (cell_lines, row_h)
                };

                // Header Row
                if !headers.is_empty() {
                    let (header_lines, header_h) = wrap_row_cells(headers);
                    if *cur_y + header_h > bottom_bound && !cur_items.is_empty() {
                        pages.push(PageLayout { items: std::mem::take(cur_items) });
                        *cur_y = p2_top;
                    }
                    cur_items.push(RenderItem::TableRow {
                        x: indent_px,
                        y: *cur_y,
                        w: table_w,
                        h: header_h,
                        col_xs: col_xs.clone(),
                        cell_lines: header_lines,
                        is_header: true,
                    });
                    *cur_y += header_h;
                }

                // Data Rows
                for row in rows {
                    let (row_lines, row_h) = wrap_row_cells(row);
                    if *cur_y + row_h > bottom_bound && !cur_items.is_empty() {
                        pages.push(PageLayout { items: std::mem::take(cur_items) });
                        *cur_y = p2_top;
                    }
                    cur_items.push(RenderItem::TableRow {
                        x: indent_px,
                        y: *cur_y,
                        w: table_w,
                        h: row_h,
                        col_xs: col_xs.clone(),
                        cell_lines: row_lines,
                        is_header: false,
                    });
                    *cur_y += row_h;
                }

                *cur_y += pt(8.0);
            }

            DocBlock::HorizontalRule => {
                if *cur_y + pt(8.0) > bottom_bound && !cur_items.is_empty() {
                    pages.push(PageLayout { items: std::mem::take(cur_items) });
                    *cur_y = p2_top;
                }
                cur_items.push(RenderItem::Rule { y: *cur_y + pt(4.0) });
                *cur_y += pt(8.0);
            }
        }
    }
}

impl AiStreamScreen {
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

        let y1 = sheet_y + pt(10.0);
        let y2 = sheet_y + pt(44.0);
        let y3 = sheet_y + pt(76.0);

        // Row 1: Source (y1 .. y2)
        if y >= y1 && y < y2 {
            if x < pad + pt(90.0) {
                return self.set_source("auto");
            } else if x < pad + pt(170.0) {
                return self.set_source("antigravity");
            } else {
                return self.set_source("claude");
            }
        }

        // Row 2: Turn History (y2 .. y3)
        if y >= y2 && y < y3 {
            if x < pad + pt(105.0) {
                return self.step_turn(-1);
            } else if x < pad + pt(170.0) {
                return self.step_turn(1);
            } else {
                return self.go_live();
            }
        }

        // Row 3: Actions (y3 .. bottom)
        if y >= y3 {
            if x < pad + pt(95.0) {
                self.poll_now();
                return Action::Redraw;
            } else if x < pad + pt(175.0) {
                return Action::RedrawFull;
            } else {
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
        crate::awake::screen_wants_awake(true);
        std::thread::spawn(|| {
            crate::wifi::turn_on_wifi();
        });
        self.poll_now();
        Action::RedrawFull
    }

    fn on_leave(&mut self) {
        crate::awake::screen_wants_awake(false);
    }

    fn holds_awake(&self) -> bool {
        self.connected || self.turn.status == "running"
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(500)
    }

    fn on_tick(&mut self) -> Action {
        let mut updates = Vec::new();
        if let Some(rx) = &self.msg_rx {
            while let Ok(msg) = rx.try_recv() {
                updates.push(msg);
            }
        }
        let mut needs_redraw = false;
        for msg in updates {
            match msg {
                PollerMsg::TurnUpdate { turn, turn_idx } => {
                    let is_explicit_nav = turn_idx.is_some() || self.turn_idx.is_some();
                    let is_initial = self.turn.assistant == "AI Companion";
                    let is_same_assistant = turn.assistant == self.turn.assistant;
                    let is_new_prompt = !turn.prompt.is_empty() && turn.prompt != self.turn.prompt;
                    let is_completed_response = turn.status == "idle" && !turn.blocks.is_empty();

                    let should_accept = is_explicit_nav
                        || is_initial
                        || is_same_assistant
                        || is_new_prompt
                        || is_completed_response;

                    if should_accept {
                        let is_new_turn = turn.id != self.turn.id || turn_idx != self.turn_idx;
                        self.turn = *turn;
                        self.turn_idx = turn_idx;
                        self.repaginate();
                        if is_new_turn {
                            self.cur_page = 0;
                        } else {
                            self.cur_page = self.cur_page.min(self.pages.len().saturating_sub(1));
                        }
                        needs_redraw = true;
                    }
                }
                PollerMsg::ConnectionStatus(c) => {
                    if self.connected != c {
                        self.connected = c;
                        needs_redraw = true;
                    }
                }
            }
        }
        if needs_redraw {
            Action::Redraw
        } else {
            Action::Keep
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        let (w, h) = p.size();
        let pad = pt(PAD_PT);
        p.clear(255);

        // 1. Ambient Clock / Center Assistant Title / Battery + WiFi Status Header
        let time_str = crate::chrome::current_time_str();
        p.text(pad, pt(14.0), 7.0, DIM, &time_str);
        
        let title = if self.connected {
            format!("* {}", self.turn.assistant)
        } else {
            "AI Stream (offline)".to_string()
        };
        let title_trunc = p.truncate(7.0, &title, p.width_pt() - 80.0);
        p.text_center(pt(14.0), 7.0, INK, &title_trunc);

        let (bat_cap, _) = ybdev::sysinfo::battery();
        let bat_str = format!("{}%", bat_cap);
        let xr = w - pad;
        let bat_w = p.text_width(7.0, &bat_str) as i32;
        p.text_right(xr, pt(14.0), 7.0, DIM, &bat_str);
        let _ = crate::chrome::draw_wifi_glyph(p, xr - bat_w - pt(6.0), pt(11.5), 7.0, DIM);
        p.hline_t(pt(20.0), pad, w - pad, 1, 230);

        // 2. User Prompt Banner Card (page 1 multi-line hero card, page 2+ compact crumb)
        let content_w = w - 2 * pad;
        if self.cur_page == 0 {
            let prompt_lines = wrap_prompt_lines(&self.turn.prompt, content_w - pt(18.0), PROMPT_SIZE_PT, 4);
            let prompt_h = prompt_card_height(prompt_lines.len());
            let prompt_r = Rect::new(pad, pt(HEADER_TOP_PT), content_w, prompt_h);
            p.rect(prompt_r, 248);
            p.rect_outline_t(prompt_r, 1, BORDER);
            p.rect(Rect::new(prompt_r.x, prompt_r.y, pt(3.0), prompt_r.h), INK);
            p.text(prompt_r.x + pt(8.0), prompt_r.y + pt(10.0), 6.5, DIM, "PROMPT");
            
            let line_h = pt(PROMPT_SIZE_PT + 4.5);
            let mut cy = prompt_r.y + pt(13.0);
            for l in &prompt_lines {
                p.text(prompt_r.x + pt(8.0), cy + pt(PROMPT_SIZE_PT), PROMPT_SIZE_PT, INK, l);
                cy += line_h;
            }
        } else {
            let prompt_clean = clean_prompt_text(&self.turn.prompt);
            let crumb = format!("Q: {}", p.truncate(7.0, &prompt_clean, (w - 2 * pad) as f32));
            p.text(pad, pt(HEADER_TOP_PT + 6.0), 7.0, DIM, &crumb);
            p.hline_t(pt(HEADER_TOP_PT + 12.0), pad, w - pad, 1, 230);
        }

        // 3. Render Page Items
        if let Some(page) = self.pages.get(self.cur_page) {
            for item in &page.items {
                match item {
                    RenderItem::Heading { y, level, lines } => {
                        let size = match level {
                            1 => H1_SIZE_PT,
                            2 => H2_SIZE_PT,
                            _ => H3_SIZE_PT,
                        };
                        let line_h = pt(size + 6.0);
                        let mut cy = *y;
                        for line in lines {
                            draw_styled_line(p, pad, cy + pt(size), size, INK, line, true);
                            cy += line_h;
                        }
                        if *level == 1 {
                            p.hline_t(cy - pt(2.0), pad, w - pad, 2, INK);
                        } else if *level == 2 {
                            p.hline_t(cy - pt(2.0), pad, w - pad, 1, BORDER);
                        }
                    }

                    RenderItem::StyledTextLine { x, y, size, color, line } => {
                        draw_styled_line(p, *x, *y, *size, *color, line, false);
                    }

                    RenderItem::ListMarker { x, y, size, is_bullet, number_str } => {
                        if *is_bullet {
                            p.circle_fill(*x, y - pt(3.5), pt(1.6), INK);
                        } else if let Some(num) = number_str {
                            p.text(*x, *y, *size, INK, num);
                            p.text(*x + 1, *y, *size, INK, num);
                        }
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

                    RenderItem::TableRow { x, y, w, h, col_xs, cell_lines, is_header } => {
                        let total_w = *w;
                        if *is_header {
                            p.rect(Rect::new(*x, *y, total_w, *h), 244);
                            p.hline_t(*y, *x, *x + total_w, 1, BORDER);
                            p.hline_t(*y + *h, *x, *x + total_w, 2, INK);
                        } else {
                            p.hline_t(*y + *h, *x, *x + total_w, 1, 230);
                        }
                        for (i, lines) in cell_lines.iter().enumerate() {
                            let cx = col_xs.get(i).copied().unwrap_or(*x);
                            if i > 0 {
                                p.rect(Rect::new(cx, *y, 1, *h), 230);
                            }
                            let mut cy = *y + pt(BODY_SIZE_PT + 1.0);
                            for l in lines {
                                if *is_header {
                                    p.text(cx + pt(6.0), cy, BODY_SIZE_PT, INK, l);
                                    p.text(cx + pt(6.0) + 1, cy, BODY_SIZE_PT, INK, l);
                                } else {
                                    p.text(cx + pt(6.0), cy, BODY_SIZE_PT, INK, l);
                                }
                                cy += pt(BODY_SIZE_PT + 4.0);
                            }
                        }
                        p.rect(Rect::new(*x, *y, 1, *h), 230);
                        p.rect(Rect::new(*x + total_w, *y, 1, *h), 230);
                    }

                    RenderItem::AlertBox { r, kind } => {
                        p.rect(*r, 248);
                        p.rect_outline_t(*r, 1, BORDER);
                        p.rect(Rect::new(r.x, r.y, pt(3.0), r.h), INK);
                        let badge = format!("[!{}]", kind);
                        p.text(r.x + pt(8.0), r.y + pt(10.0), 6.5, INK, &badge);
                    }

                    RenderItem::Rule { y } => {
                        p.hline_t(*y, pad, w - pad, 1, BORDER);
                    }
                }
            }
        }

        // 4. Footer Pagination Bar & Live Tool Activity Chip
        let footer_y = h - pt(12.0);
        let total_pages = self.pages.len().max(1);
        let page_label = if let Some(tool) = &self.turn.tool_status {
            if !tool.is_empty() {
                format!("Page {} of {} · {} · [*] {}", self.cur_page + 1, total_pages, self.turn.timestamp, tool)
            } else {
                format!("Page {} of {} · {}", self.cur_page + 1, total_pages, self.turn.timestamp)
            }
        } else if self.turn.status == "running" {
            format!("Page {} of {} · {} · [*] running...", self.cur_page + 1, total_pages, self.turn.timestamp)
        } else {
            format!("Page {} of {} · {}", self.cur_page + 1, total_pages, self.turn.timestamp)
        };
        let page_label_clean = sanitize_text(&page_label);
        let page_label_trunc = p.truncate(FOOTER_SIZE_PT, &page_label_clean, p.width_pt() - 40.0);
        p.text_center(footer_y, FOOTER_SIZE_PT, DIM, &page_label_trunc);

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
            // Swipe North (up) anywhere opens quick settings sheet
            Gesture::Swipe { dir: SwipeDir::North, .. } => {
                self.sheet_open = true;
                Action::Redraw
            }

            Gesture::Tap { x, y } => {
                let x = x as i32;
                let y = y as i32;

                // Tapping top status header (or prompt card header) opens quick settings sheet
                if y < pt(HEADER_TOP_PT + HEADER_H_PT) {
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
                    self.poll_now();
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
                self.poll_now();
                Action::RedrawFull
            }

            _ => Action::Keep,
        }
    }
}

impl Drop for AiStreamScreen {
    fn drop(&mut self) {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(PollerCmd::Stop);
        }
    }
}

/// Clean raw user prompt from metadata tags and markdown wrappers.
pub fn clean_prompt_text(s: &str) -> String {
    let mut cleaned = s.replace("<USER_REQUEST>", "").replace("</USER_REQUEST>", "");
    if let Some(pos) = cleaned.find("<ADDITIONAL_METADATA>") {
        cleaned.truncate(pos);
    }
    if let Some(pos) = cleaned.find("The current local time is:") {
        cleaned.truncate(pos);
    }
    let without_md = clean_inline_markdown(&cleaned);
    let mut words = Vec::new();
    for line in without_md.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            words.push(trimmed);
        }
    }
    words.join(" ")
}

/// Wrap prompt into multiple lines up to `max_lines`, adding ellipsis if overflowing.
pub fn wrap_prompt_lines(prompt: &str, max_w: i32, size_pt: f32, max_lines: usize) -> Vec<String> {
    let text = clean_prompt_text(prompt);
    let spans = vec![StyledSpan { text, is_bold: false, is_italic: false, is_code: false }];
    let lines = wrap_styled_spans(&spans, max_w, size_pt);
    let total_lines = lines.len();
    let mut result = Vec::new();
    for (idx, l) in lines.into_iter().enumerate() {
        if idx + 1 == max_lines && idx < total_lines - 1 {
            let mut line_str = l.spans.into_iter().map(|s| s.text).collect::<Vec<_>>().join("");
            line_str.push_str("...");
            result.push(line_str);
            break;
        }
        let line_str = l.spans.into_iter().map(|s| s.text).collect::<Vec<_>>().join("");
        result.push(line_str);
        if idx + 1 >= max_lines {
            break;
        }
    }
    if result.is_empty() {
        result.push("No prompt".to_string());
    }
    result
}

pub fn prompt_card_height(lines_count: usize) -> i32 {
    let line_h = pt(PROMPT_SIZE_PT + 4.5);
    pt(14.0) + (lines_count as i32) * line_h + pt(6.0)
}

/// Convert unsupported Unicode symbols into clean ASCII equivalents.
pub fn sanitize_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '↑' => out.push_str("[^]"),
            '↓' => out.push_str("[v]"),
            '←' => out.push_str("<-"),
            '→' => out.push_str("->"),
            '🟢' => out.push_str("(*)"),
            '⚪' => out.push_str("( )"),
            '✓' | '✔' => out.push_str("[v]"),
            '✗' | '✘' => out.push_str("[x]"),
            '•' => out.push(' '),
            '…' => out.push_str("..."),
            '🚀' | '⚡' | '💡' | '★' | '☆' => out.push('*'),
            '⚙' => out.push_str("[*]"),
            '▶' => out.push('>'),
            '◀' => out.push('<'),
            _ if (c as u32) >= 0x1F000 => out.push('*'),
            _ => out.push(c),
        }
    }
    out
}

/// Clean markdown symbols from text (bold, italic, inline code, link syntax).
pub fn clean_inline_markdown(text: &str) -> String {
    let mut s = sanitize_text(text);
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

/// Wrap table cell text across multiple lines inside a column width constraint.
pub fn wrap_cell_text(text: &str, max_w: i32, size_pt: f32) -> Vec<String> {
    let cleaned = clean_inline_markdown(text);
    if cleaned.trim().is_empty() {
        return vec![String::new()];
    }
    let breakable = cleaned
        .replace("::", ":: ")
        .replace("<", "< ")
        .replace(">", "> ")
        .replace("/", "/ ");
    let spans = vec![StyledSpan { text: breakable, is_bold: false, is_italic: false, is_code: false }];
    let lines = wrap_styled_spans(&spans, max_w, size_pt);
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines.into_iter().map(|l| l.spans.into_iter().map(|s| s.text).collect::<Vec<_>>().join("")).collect()
    }
}

fn draw_styled_line(
    p: &mut Painter,
    x: i32,
    y: i32,
    size: f32,
    color: u8,
    line: &StyledLine,
    force_bold: bool,
) {
    let mut cx = x;

    for span in &line.spans {
        let span_clean = sanitize_text(&span.text);
        if span_clean.is_empty() {
            continue;
        }

        if span.is_code {
            let has_leading_space = span_clean.starts_with(' ');
            let badge_text = span_clean.trim_start();
            if has_leading_space {
                cx += p.text_width(size, " ") as i32;
            }
            let tw = p.text_width(size, badge_text) as i32;
            let pad_x = pt(2.0);
            let bg_r = Rect::new(cx, y - pt(size * 0.80), tw + 2 * pad_x, pt(size * 1.02));
            p.rect(bg_r, 242);
            p.rect_outline_t(bg_r, 1, 200);
            p.text(cx + pad_x, y, size, INK, badge_text);
            cx += tw + 2 * pad_x;
        } else {
            let tw = p.text_width(size, &span_clean) as i32;
            if span.is_bold || force_bold {
                p.text(cx, y, size, color, &span_clean);
                p.text(cx + 1, y, size, color, &span_clean);
            } else {
                p.text(cx, y, size, color, &span_clean);
            }
            cx += tw;
        }
    }
}

fn cached_font() -> &'static yui::font::Font {
    static FONT: std::sync::OnceLock<yui::font::Font> = std::sync::OnceLock::new();
    FONT.get_or_init(|| yui::font::Font::load().unwrap())
}

pub fn wrap_styled_spans(spans: &[StyledSpan], max_width_px: i32, size_pt: f32) -> Vec<StyledLine> {
    struct WordToken {
        text: String,
        starts_with_space: bool,
        is_bold: bool,
        is_italic: bool,
        is_code: bool,
    }

    let mut words: Vec<WordToken> = Vec::new();
    let mut pending_space = false;
    let mut is_first_word = true;

    for span in spans {
        let cleaned = sanitize_text(&span.text);
        if cleaned.starts_with(char::is_whitespace) {
            pending_space = true;
        }

        let raw_words: Vec<&str> = cleaned.split_whitespace().collect();
        for w in raw_words {
            let starts_with_space = if is_first_word {
                false
            } else {
                pending_space
            };
            is_first_word = false;
            pending_space = true;

            words.push(WordToken {
                text: w.to_string(),
                starts_with_space,
                is_bold: span.is_bold,
                is_italic: span.is_italic,
                is_code: span.is_code,
            });
        }

        if cleaned.ends_with(char::is_whitespace) {
            pending_space = true;
        } else if !cleaned.is_empty() {
            pending_space = false;
        }
    }

    let font = cached_font();
    let space_px = font.text_width(size_pt * PX, " ");
    let max_w = max_width_px as f32;

    let mut lines = Vec::new();
    let mut cur_line_spans: Vec<StyledSpan> = Vec::new();
    let mut cur_line_px: f32 = 0.0;

    for token in words {
        let token_px = font.text_width(size_pt * PX, &token.text)
            + if token.is_code { pt(4.0) as f32 } else { 0.0 };
        let add_space = token.starts_with_space && cur_line_px > 0.0;
        let needed_px = if cur_line_px == 0.0 {
            token_px
        } else if add_space {
            cur_line_px + space_px + token_px
        } else {
            cur_line_px + token_px
        };

        if needed_px > max_w && !cur_line_spans.is_empty() {
            lines.push(StyledLine { spans: std::mem::take(&mut cur_line_spans) });
            cur_line_px = 0.0;
        }

        let is_line_start = cur_line_px == 0.0;
        if let Some(last_span) = cur_line_spans.last_mut() {
            if last_span.is_bold == token.is_bold
                && last_span.is_italic == token.is_italic
                && last_span.is_code == token.is_code
            {
                if add_space && !is_line_start {
                    last_span.text.push(' ');
                }
                last_span.text.push_str(&token.text);
            } else {
                cur_line_spans.push(StyledSpan {
                    text: if add_space && !is_line_start {
                        format!(" {}", token.text)
                    } else {
                        token.text.clone()
                    },
                    is_bold: token.is_bold,
                    is_italic: token.is_italic,
                    is_code: token.is_code,
                });
            }
        } else {
            cur_line_spans.push(StyledSpan {
                text: token.text.clone(),
                is_bold: token.is_bold,
                is_italic: token.is_italic,
                is_code: token.is_code,
            });
        }

        cur_line_px = if cur_line_px == 0.0 {
            token_px
        } else if add_space {
            cur_line_px + space_px + token_px
        } else {
            cur_line_px + token_px
        };
    }

    if !cur_line_spans.is_empty() {
        lines.push(StyledLine { spans: cur_line_spans });
    }

    lines
}

pub fn parse_json_turn(bytes: &[u8]) -> Result<Turn, String> {
    let s = std::str::from_utf8(bytes).map_err(|e| e.to_string())?;
    
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
        blocks = parse_md_to_doc_blocks(&raw_md);
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

pub fn parse_md_to_doc_blocks(md: &str) -> Vec<DocBlock> {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(md, options);

    enum Node {
        Root(Vec<DocBlock>),
        Heading(usize, Vec<StyledSpan>),
        Paragraph(Vec<StyledSpan>),
        Code(String, Vec<String>),
        Alert(String, Vec<DocBlock>),
        List(bool, u64, Vec<DocListItem>),
        ListItem(Vec<DocBlock>),
        Table(Vec<String>, Vec<Vec<String>>, Vec<String>, String, bool),
    }

    let mut stack: Vec<Node> = vec![Node::Root(Vec::new())];
    let mut is_bold = false;
    let mut is_italic = false;

    let push_block_to_parent = |stack: &mut Vec<Node>, block: DocBlock| {
        if let Some(parent) = stack.last_mut() {
            match parent {
                Node::Root(blocks) => blocks.push(block),
                Node::ListItem(blocks) => blocks.push(block),
                Node::Alert(_, blocks) => blocks.push(block),
                _ => {}
            }
        }
    };

    let push_span_to_parent = |stack: &mut Vec<Node>, span: StyledSpan| {
        if let Some(top) = stack.last_mut() {
            match top {
                Node::Heading(_, spans) => spans.push(span),
                Node::Paragraph(spans) => spans.push(span),
                Node::Alert(_, blocks) => {
                    if let Some(DocBlock::Paragraph { spans }) = blocks.last_mut() {
                        spans.push(span);
                    } else {
                        blocks.push(DocBlock::Paragraph { spans: vec![span] });
                    }
                }
                Node::Table(_, _, _, cell, _) => cell.push_str(&span.text),
                Node::ListItem(blocks) => {
                    if let Some(DocBlock::Paragraph { spans }) = blocks.last_mut() {
                        spans.push(span);
                    } else {
                        blocks.push(DocBlock::Paragraph { spans: vec![span] });
                    }
                }
                Node::Root(blocks) => {
                    if let Some(DocBlock::Paragraph { spans }) = blocks.last_mut() {
                        spans.push(span);
                    } else {
                        blocks.push(DocBlock::Paragraph { spans: vec![span] });
                    }
                }
                _ => {}
            }
        }
    };

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let lvl = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                stack.push(Node::Heading(lvl, Vec::new()));
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(Node::Heading(level, spans)) = stack.pop() {
                    push_block_to_parent(&mut stack, DocBlock::Heading { level, spans });
                }
            }
            Event::Start(Tag::Paragraph) => {
                stack.push(Node::Paragraph(Vec::new()));
            }
            Event::End(TagEnd::Paragraph) => {
                if let Some(Node::Paragraph(spans)) = stack.pop() {
                    if !spans.is_empty() {
                        push_block_to_parent(&mut stack, DocBlock::Paragraph { spans });
                    }
                }
            }
            Event::Start(Tag::List(start_num)) => {
                stack.push(Node::List(start_num.is_some(), start_num.unwrap_or(1), Vec::new()));
            }
            Event::End(TagEnd::List(_)) => {
                if let Some(Node::List(is_ordered, start_num, items)) = stack.pop() {
                    push_block_to_parent(&mut stack, DocBlock::List { is_ordered, start_num, items });
                }
            }
            Event::Start(Tag::Item) => {
                stack.push(Node::ListItem(Vec::new()));
            }
            Event::End(TagEnd::Item) => {
                if let Some(Node::ListItem(blocks)) = stack.pop() {
                    if let Some(Node::List(_, _, items)) = stack.last_mut() {
                        items.push(DocListItem { blocks });
                    }
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(l) => l.to_string(),
                    _ => "code".to_string(),
                };
                stack.push(Node::Code(lang, Vec::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(Node::Code(lang, lines)) = stack.pop() {
                    push_block_to_parent(&mut stack, DocBlock::Code {
                        lang: if lang.is_empty() { "code".to_string() } else { lang },
                        lines,
                    });
                }
            }
            Event::Start(Tag::BlockQuote(_)) => {
                stack.push(Node::Alert("NOTE".to_string(), Vec::new()));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                if let Some(Node::Alert(mut kind, mut blocks)) = stack.pop() {
                    // Check if first paragraph starts with GFM alert header: [!TIP] / [!NOTE] / [!WARNING] etc.
                    if let Some(DocBlock::Paragraph { spans }) = blocks.first_mut() {
                        let combined_text: String = spans.iter().map(|s| s.text.as_str()).collect();
                        let trimmed = combined_text.trim_start();
                        if trimmed.starts_with("[!") && trimmed.contains(']') {
                            if let Some(end) = trimmed.find(']') {
                                kind = trimmed[2..end].to_uppercase();
                                let mut chars_to_skip = (combined_text.len() - trimmed.len()) + end + 1;
                                while !spans.is_empty() && chars_to_skip > 0 {
                                    if spans[0].text.len() <= chars_to_skip {
                                        chars_to_skip -= spans[0].text.len();
                                        spans.remove(0);
                                    } else {
                                        spans[0].text = spans[0].text[chars_to_skip..].trim_start().to_string();
                                        chars_to_skip = 0;
                                    }
                                }
                                if let Some(first_span) = spans.first_mut() {
                                    first_span.text = first_span.text.trim_start().to_string();
                                }
                            }
                        }
                    }
                    if let Some(DocBlock::Paragraph { spans }) = blocks.first() {
                        if spans.is_empty() {
                            blocks.remove(0);
                        }
                    }
                    push_block_to_parent(&mut stack, DocBlock::Alert { kind, blocks });
                }
            }
            Event::Start(Tag::Table(_)) => {
                stack.push(Node::Table(Vec::new(), Vec::new(), Vec::new(), String::new(), false));
            }
            Event::End(TagEnd::Table) => {
                if let Some(Node::Table(headers, rows, _, _, _)) = stack.pop() {
                    push_block_to_parent(&mut stack, DocBlock::Table { headers, rows });
                }
            }
            Event::Start(Tag::TableHead) => {
                if let Some(Node::Table(_, _, _, _, in_head)) = stack.last_mut() {
                    *in_head = true;
                }
            }
            Event::End(TagEnd::TableHead) => {
                if let Some(Node::Table(_, _, _, _, in_head)) = stack.last_mut() {
                    *in_head = false;
                }
            }
            Event::Start(Tag::TableRow) => {
                if let Some(Node::Table(_, _, row, _, _)) = stack.last_mut() {
                    row.clear();
                }
            }
            Event::End(TagEnd::TableRow) => {
                if let Some(Node::Table(headers, rows, row, _, in_head)) = stack.last_mut() {
                    if *in_head || headers.is_empty() {
                        *headers = std::mem::take(row);
                    } else {
                        rows.push(std::mem::take(row));
                    }
                }
            }
            Event::Start(Tag::TableCell) => {
                if let Some(Node::Table(_, _, _, cell, _)) = stack.last_mut() {
                    cell.clear();
                }
            }
            Event::End(TagEnd::TableCell) => {
                if let Some(Node::Table(_, _, row, cell, _)) = stack.last_mut() {
                    row.push(std::mem::take(cell));
                }
            }
            Event::Start(Tag::Strong) => {
                is_bold = true;
            }
            Event::End(TagEnd::Strong) => {
                is_bold = false;
            }
            Event::Start(Tag::Emphasis) => {
                is_italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                is_italic = false;
            }
            Event::Text(t) => {
                let s = sanitize_text(t.as_ref());
                if let Some(Node::Code(_, lines)) = stack.last_mut() {
                    for l in s.lines() {
                        lines.push(l.to_string());
                    }
                } else {
                    push_span_to_parent(&mut stack, StyledSpan {
                        text: s,
                        is_bold,
                        is_italic,
                        is_code: false,
                    });
                }
            }
            Event::Code(c) => {
                let s = sanitize_text(c.as_ref());
                push_span_to_parent(&mut stack, StyledSpan {
                    text: s,
                    is_bold: false,
                    is_italic: false,
                    is_code: true,
                });
            }
            Event::SoftBreak | Event::HardBreak => {
                push_span_to_parent(&mut stack, StyledSpan {
                    text: " ".to_string(),
                    is_bold,
                    is_italic,
                    is_code: false,
                });
            }
            Event::Rule => {
                push_block_to_parent(&mut stack, DocBlock::HorizontalRule);
            }
            _ => {}
        }
    }

    match stack.pop() {
        Some(Node::Root(blocks)) => blocks,
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save_preview_artifact(name: &str, canvas: &[u8]) {
        let artifact_dir = match std::env::var("YB_AI_PREVIEW_DIR").or_else(|_| std::env::var("ARTIFACT_DIR")) {
            Ok(d) if !d.is_empty() => d,
            _ => return,
        };
        let path = std::path::Path::new(&artifact_dir).join(name);
        if let Ok(file) = std::fs::File::create(&path) {
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), 1236, 1648);
            enc.set_color(png::ColorType::Grayscale);
            enc.set_depth(png::BitDepth::Eight);
            if let Ok(mut w) = enc.write_header() {
                let _ = w.write_image_data(canvas);
            }
        }
    }

    #[test]
    fn test_md_parser_headings_and_code() {
        let md = "# Title\n\nSome paragraph text here.\n\n```rust\nfn main() {}\n```\n";
        let blocks = parse_md_to_doc_blocks(md);
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            DocBlock::Heading { level, spans } => {
                assert_eq!(*level, 1);
                assert_eq!(spans[0].text, "Title");
            }
            _ => panic!("expected heading"),
        }
        match &blocks[2] {
            DocBlock::Code { lang, lines } => {
                assert_eq!(lang, "rust");
                assert_eq!(lines, &vec!["fn main() {}".to_string()]);
            }
            _ => panic!("expected code"),
        }
    }

    #[test]
    fn renders_nested_code_list_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "### Root Cause & Fix\n\nLooking at your photo, two specific issues occurred:\n\n1. **Missing Parent Item Titles in Nested Lists**:\n   - In Markdown structures like:\n     ```markdown\n     1. **Continuation Line Bullets Fixed**:\n        - Previously, wrapped lines...\n     ```\n     When the parser encountered the nested list (- Previously...), it previously overwrote the parent item's text buffer (1. Continuation Line Bullets Fixed:), causing the parent title to disappear completely and turning all child items into flat bullets.\n   - **Fixed**: Implemented an explicit item_text_stack that commits parent item headers with their proper number (1., 2., 3.) before descending into child lists, preserving the full tree hierarchy and indentation.\n\n2. **Heading & Typography Hierarchy**:\n   - Headings now render with bold weight (+1px stem stroke), distinct font scaling (14pt / 12pt / 10.5pt), and balanced vertical margins.\n   - List numbers (1., 2.) are rendered in bold next to the first line, with continuation lines aligned flush underneath.\n\n";
        
        let json_payload = format!(
            "{{\"id\":\"turn_104\",\"assistant\":\"Antigravity\",\"prompt\":\"it got worse: photo.jpg\",\"timestamp\":\"21:15\",\"status\":\"idle\",\"tool_status\":null,\"revision\":8,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );

        let turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        eprintln!("PARSED BLOCKS: {:#?}", turn.blocks);
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
        save_preview_artifact("nested_code_preview.png", &canvas);

        // Render Page 2
        if s.pages.len() > 1 {
            s.cur_page = 1;
            let mut canvas2 = vec![255u8; 1236 * 1648];
            let mut panel2 = vec![255u8; 1248 * 1648];
            let mut p2 = yui::Painter::new(
                &mut panel2,
                1236,
                1648,
                1248,
                yui::Orientation::Portrait,
                &mut canvas2,
                &font,
            );
            s.draw(&mut p2);
            save_preview_artifact("nested_code_p2_preview.png", &canvas2);
        }
    }

    #[test]
    fn renders_ai_stream_device_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "## Kindle Live AI Companion Stream\n\nA long AI response paginated into 1236x1648 book pages for e-ink.\n\n> [!TIP]\n> Tap the right side of the screen to advance pages, left side to go back, or swipe bottom-right to return to the library.\n\n```rust\n// Kindle E-Ink Streamer Loop\nfn poll_live_stream() -> Result<Turn, Error> {\n    let resp = conn.request(\"GET\", \"/live\", &mut sink)?;\n    Ok(parse_json_turn(&resp))\n}\n```\n\n| Feature | Kindle Mode | Status |\n|---|---|---|\n| Pagination | Auto 1236x1648 | Active |\n| Code Highlighting | Monospace Box | Active |\n| Discovery | UDP Broadcast :8766 | Active |\n";
        let json_payload = format!(
            "{{\"id\":\"turn_sample\",\"assistant\":\"Antigravity\",\"prompt\":\"Can we use Kindle as a second monitor for reading AI turns?\",\"timestamp\":\"20:25\",\"status\":\"idle\",\"tool_status\":\"Running cargo test...\",\"revision\":1,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );
        s.turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        s.repaginate();
        assert!(s.pages.len() >= 1);

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
        save_preview_artifact("ai_stream_device_preview.png", &canvas);
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
        save_preview_artifact("markdown_cleanup_preview.png", &canvas);
    }

    #[test]
    fn renders_quick_settings_sheet_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        s.sheet_open = true;
        s.source_mode = "antigravity".to_string();
        let md = "## Quick Settings & Source Selector\n\nSwipe up from the bottom-left corner of the screen to open the Quick Settings sheet anytime.";
        let json_payload = format!(
            "{{\"id\":\"turn_sample\",\"assistant\":\"Antigravity\",\"prompt\":\"how do i switch between antigravity, claude code and everything else?\",\"timestamp\":\"20:58\",\"status\":\"idle\",\"tool_status\":null,\"revision\":2,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );
        s.turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        s.repaginate();
        assert!(s.pages.len() >= 1);

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
        save_preview_artifact("ai_quick_settings_preview.png", &canvas);
    }

    #[test]
    fn renders_sheet_gestures_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "### Quick Settings Sheet Gestures\n\nWhat the sheet supports.\n\n### What Was Adjusted:\n1. **Edge Gestures Unlocked (`default_edges: false`)**:\n   - `yb-reader`'s app-level gesture interceptor was previously intercepting edge swipes (thinking they were system curtain/brightness gestures). Unlocking `default_edges` gives the AI stream screen full ownership of all gestures.\n2. **Simplified Sheet Triggers**:\n   - **Swipe up (`↑`) anywhere on the screen** (no longer requiring a pixel-perfect corner angle).\n   - **Tap anywhere on the top header** (clock, prompt card).\n3. **Full Row Touch Targets**:\n   - Every pixel in the 3 sheet rows is now mapped to the corresponding button:\n     - **Row 1**: `[ Auto (Latest) ]` / `[ Antigravity ]` / `[ Claude Code ]`\n     - **Row 2**: `[ < Prev Turn ]` / `[ Next Turn > ]` / `[ Live Latest ]`\n     - **Row 3**: `[ Poll Now ]` / `[ Clear Ghosting ]` / `[ Done ]`\n4. **Dismissing Sheet**:\n   - Swipe down (`↓`), tap `[ Done ]`, or tap anywhere on the shaded area above the sheet.\n\n";
        
        let json_payload = format!(
            "{{\"id\":\"turn_102\",\"assistant\":\"Antigravity\",\"prompt\":\"quick settings sheet behavior\",\"timestamp\":\"21:08\",\"status\":\"idle\",\"tool_status\":\"Running tool\",\"revision\":6,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );

        let turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        s.turn = turn;
        s.repaginate();
        assert!(s.pages.len() >= 1);

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
        save_preview_artifact("sheet_gestures_preview.png", &canvas);
    }

    #[test]
    fn renders_markdown_engine_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "### Markdown Engine\n\nI replaced the ad-hoc line parser with **pulldown-cmark** (the official, pull-parsing CommonMark standard engine in Rust) and integrated structured AST layout:\n\n### What Has Been Fixed:\n\n1. **Continuation Line Bullets Fixed**:\n   - Previously, wrapped lines in a bullet item were mistakenly treated as new bullet points.\n   - Now, a bullet disk or ordered number (`1.`, `2.`) is rendered **only on the first line**, and all wrapped continuation lines indent flush underneath the text.\n\n2. **Nested List Support**:\n   - Sub-bullets and nested list levels are indented by `depth * pt(12.0)` with proper spacing.\n\n3. **Unicode Arrows & Symbols Sanitized**:\n   - Unsupported Unicode glyphs (e.g. `↑`, `↓`, `✓`, `🟢`) that previously caused missing-glyph `[]` boxes now cleanly map to legible ASCII symbols (`[^]`, `[v]`, `[v]`, `(*)`).\n\n4. **Typography & Hierarchy**:\n   - Sized headings (`H1: 14pt`, `H2: 12pt`, `H3: 10.5pt`, `Body: 8.5pt`).\n   - Clean horizontal rules and section margins.\n\n";
        
        let json_payload = format!(
            "{{\"id\":\"turn_103\",\"assistant\":\"Antigravity\",\"prompt\":\"you can see that your rendering is not really accurate. should we use some lightweight md renderer or something else?\",\"timestamp\":\"21:12\",\"status\":\"idle\",\"tool_status\":null,\"revision\":7,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );

        let turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        s.turn = turn;
        s.repaginate();
        assert!(s.pages.len() >= 1);

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
        save_preview_artifact("markdown_engine_preview.png", &canvas);
    }

    #[test]
    fn renders_claude_response_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "Example assistant reply:\n\n## What's good (better than I expected)\n\n- **The rendering model is textbook e-ink**: `repaginate()` precomputes `RenderItem` pages with absolute coordinates, and `draw()` just walks them. No layout work per frame, revision-diffed so it only repaints on change, and same-turn updates preserve your page while new turns reset to page 0. That's the same discipline yread's paginator uses.\n- **pulldown-cmark was the right call** — the old ad-hoc line parser (visible in the test history) was replaced by a real AST walker. Nested lists, GFM `[!TIP]` alerts, tables, code blocks with language badges and `(cont)` continuation across pages — all handled structurally, not regex-soup.\n- Text sanitizing `sanitize_text()` maps arrows/checkmarks/emoji to ASCII, bullets are `circle_fill` disks, not glyphs.\n- Nice product details: prompt card on page 1 collapsing to a `Q:` crumb on page 2+, turn-history stepping with negative indices, source switching (auto/antigravity/claude), reusing `mirror.conf` + UDP discovery instead of inventing config.";
        
        let json_payload = format!(
            "{{\"id\":\"turn_claude\",\"assistant\":\"Claude Code\",\"prompt\":\"is it resource-efficient?\",\"timestamp\":\"18:31\",\"status\":\"idle\",\"tool_status\":null,\"revision\":1,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );

        let turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        s.turn = turn;
        s.repaginate();
        assert_eq!(s.pages.len(), 2);
        assert_eq!(s.turn.blocks.len(), 3);

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
        save_preview_artifact("claude_response_preview.png", &canvas);
    }

    #[test]
    fn renders_long_form_markdown_preview() {
        let font = yui::font::Font::load().unwrap();
        let mut s = AiStreamScreen::new(1236, 1648);
        s.connected = true;
        let md = "## Long-Form Markdown Turn\n\nThis turn exercises the CommonMark layout pipeline.\n\n---\n\n### 1. Feature Verification Checklist\n\nHere is what this test turn exercises across your display:\n\n- **Heading Scale & Weights**:\n  - Headings feature sized hierarchy with double-weight stem stroke for crisp e-ink legibility.\n- **Inline Badges & Word Spacing**:\n  - Tokens like `repaginate()`, `Conn::new()`, and `StyledSpan` render with padded bounding boxes and clean inter-word spacing.\n- **Nested Lists & Tree Structure**:\n  1. **Primary Layer**: Ordered list item with bold numbering.\n     - **Child Layer 1**: Sub-bullet with `depth * pt(12.0)` indentation.\n     - **Child Layer 2**: Preserved tree hierarchy with flush wrapped lines.\n  2. **Continuation Handling**: Long sentences wrap seamlessly beneath the text baseline without re-triggering bullet disks.\n\n---\n\n### 2. Architecture Comparison Table\n\n| Component | UI Main Thread | Worker Thread | Channel Protocol |\n|---|---|---|---|\n| **Network Polling** | Non-blocking `0ms` | Background TCP / UDP | `mpsc::Receiver<PollerMsg>` |\n| **Paginator** | `repaginate()` on rev-diff | Pre-computes items | In-memory layout cache |\n| **Keepalive** | `holds_awake = true` | Persistent `Conn` | Exponential backoff (1.5s–30s) |\n| **Gestures** | Direct swipe / tap dispatch | Independent of socket | Instantaneous response |\n\n---\n\n### 3. Core Worker Implementation Snippet\n\n```rust\n// Background E-Ink Streamer Worker Loop\nfn worker_loop(rx: Receiver<PollerCmd>, tx: Sender<PollerMsg>) {\n    let mut conn: Option<Conn> = None;\n    let mut fail_count: u32 = 0;\n\n    loop {\n        let sleep_dur = match fail_count {\n            0 => Duration::from_millis(1400),\n            n => Duration::from_millis((1500 * (1 << (n.min(5) - 1))).min(30_000)),\n        };\n\n        if let Ok(resp) = conn.request(\"GET\", \"/live\", &mut sink) {\n            tx.send(PollerMsg::TurnUpdate(resp));\n            fail_count = 0;\n        } else {\n            fail_count += 1;\n        }\n    }\n}\n```\n\n> [!TIP]\n> **Navigation Controls:**\n> - **Tap Right 35%**: Next page\n> - **Tap Left 35%**: Previous page\n> - **Tap Top Bar / Swipe Up**: Open Quick Settings sheet (switch source or view history)\n> - **Swipe Down from Top Edge**: Pull down the Kindle system curtain (brightness / Wi-Fi)\n\n";
        
        let json_payload = format!(
            "{{\"id\":\"turn_sim\",\"assistant\":\"Antigravity\",\"prompt\":\"simulate a long response\",\"timestamp\":\"18:42\",\"status\":\"running\",\"tool_status\":\"Running tool\",\"revision\":1,\"raw_markdown\":\"{}\"}}",
            md.replace("\n", "\\n").replace("\"", "\\\"")
        );

        let turn = parse_json_turn(json_payload.as_bytes()).unwrap();
        eprintln!("PARSED BLOCKS: {:#?}", turn.blocks);
        s.turn = turn;
        s.repaginate();
        eprintln!("TOTAL PAGES: {}", s.pages.len());

        for page_idx in 0..s.pages.len() {
            s.cur_page = page_idx;
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
            save_preview_artifact(&format!("sim_page_{}.png", page_idx + 1), &canvas);
        }
    }

    #[test]
    fn test_anti_flap_turn_filter() {
        let (msg_tx, msg_rx) = std::sync::mpsc::channel();
        let mut s = AiStreamScreen::new_mock(1236, 1648);
        s.msg_rx = Some(msg_rx);

        // 1. Initial connection with Antigravity turn
        let turn_a = Turn {
            id: "turn_a1".to_string(),
            assistant: "Antigravity".to_string(),
            prompt: "Refactor poller".to_string(),
            timestamp: "18:50".to_string(),
            status: "running".to_string(),
            tool_status: Some("Running tool".to_string()),
            revision: 1,
            blocks: vec![DocBlock::Paragraph { spans: vec![StyledSpan { text: "Starting refactor...".to_string(), is_bold: false, is_italic: false, is_code: false }] }],
        };
        msg_tx.send(PollerMsg::TurnUpdate { turn: Box::new(turn_a), turn_idx: None }).unwrap();
        assert!(matches!(s.on_tick(), Action::Redraw));
        assert_eq!(s.turn.assistant, "Antigravity");

        // 2. Claude Code runs a background tool call (status=running, no completed message, same prompt)
        let claude_tool_flap = Turn {
            id: "turn_c1".to_string(),
            assistant: "Claude Code".to_string(),
            prompt: "Refactor poller".to_string(),
            timestamp: "18:50".to_string(),
            status: "running".to_string(),
            tool_status: Some("View File".to_string()),
            revision: 2,
            blocks: vec![],
        };
        msg_tx.send(PollerMsg::TurnUpdate { turn: Box::new(claude_tool_flap), turn_idx: None }).unwrap();
        // Screen should IGNORE the flap and remain on Antigravity!
        assert!(matches!(s.on_tick(), Action::Keep));
        assert_eq!(s.turn.assistant, "Antigravity");

        // 3. Antigravity updates its own tool status (same assistant)
        let turn_a2 = Turn {
            id: "turn_a1".to_string(),
            assistant: "Antigravity".to_string(),
            prompt: "Refactor poller".to_string(),
            timestamp: "18:51".to_string(),
            status: "running".to_string(),
            tool_status: Some("edit_file".to_string()),
            revision: 3,
            blocks: vec![DocBlock::Paragraph { spans: vec![StyledSpan { text: "Editing file...".to_string(), is_bold: false, is_italic: false, is_code: false }] }],
        };
        msg_tx.send(PollerMsg::TurnUpdate { turn: Box::new(turn_a2), turn_idx: None }).unwrap();
        assert!(matches!(s.on_tick(), Action::Redraw));
        assert_eq!(s.turn.assistant, "Antigravity");
        assert_eq!(s.turn.tool_status.as_deref(), Some("edit_file"));

        // 4. Claude Code delivers a completed response message (status=idle, non-empty blocks)
        let claude_completed = Turn {
            id: "turn_c1".to_string(),
            assistant: "Claude Code".to_string(),
            prompt: "Refactor poller".to_string(),
            timestamp: "18:52".to_string(),
            status: "idle".to_string(),
            tool_status: None,
            revision: 4,
            blocks: vec![DocBlock::Paragraph { spans: vec![StyledSpan { text: "Refactor complete!".to_string(), is_bold: true, is_italic: false, is_code: false }] }],
        };
        msg_tx.send(PollerMsg::TurnUpdate { turn: Box::new(claude_completed), turn_idx: None }).unwrap();
        assert!(matches!(s.on_tick(), Action::Redraw));
        assert_eq!(s.turn.assistant, "Claude Code");
        assert_eq!(s.turn.status, "idle");
    }
}

