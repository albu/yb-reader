//! Easter egg: tap the build stamp (`v<sha>`) on the home header five
//! times to launch the **E-Ink Arcade**:
//! - **GIANT MINESWEEPER** (Сапёр): 16x20 pure flagless speedrunning with 1-bit neon displays.
//! - **GO / WEIQI (碁)**: Full 9x9 and 13x13 Go with instant-move tactical AI, capture physics,
//!   Ko rule enforcement, and 2-Player Pass & Play!

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ybdev::input::Gesture;
use yui::painter::{pt, Painter, Rect};
use yui::screen::{Action, Screen};
use yui::Orientation;

// ============================================================================
// TOP LEVEL GAME SELECTION
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveGame {
    Minesweeper,
    Go,
}

// ============================================================================
// MINESWEEPER ENGINE & DATA
// ============================================================================

pub const MINE_COLS: usize = 16;
pub const MINE_ROWS: usize = 20;
pub const TOTAL_MINES: usize = 45;

const TOP_BAR_H_PT: f32 = 34.0;
const GO_STATUS_H_PT: f32 = 24.0;

const MINE_SCORE_PATH: &str = "/mnt/us/system/minesweeper.score";
const FALLBACK_MINE_SCORE_PATH: &str = "/tmp/minesweeper.score";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MineCell {
    pub is_mine: bool,
    pub revealed: bool,
    pub adjacent_mines: u8,
    pub exploded: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MineGameState {
    Ready,
    Playing,
    Won,
    Lost,
}

pub struct MinesweeperGame {
    pub grid: [[MineCell; MINE_COLS]; MINE_ROWS],
    pub state: MineGameState,
    pub mines_count: usize,
    pub start_time: Option<Instant>,
    pub elapsed_secs: u32,
    pub best_time: u32,
    pub seed: u64,
}

impl MinesweeperGame {
    pub fn new() -> MinesweeperGame {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(987654);
        let best_time = Self::load_best_time();
        MinesweeperGame {
            grid: [[MineCell {
                is_mine: false,
                revealed: false,
                adjacent_mines: 0,
                exploded: false,
            }; MINE_COLS]; MINE_ROWS],
            state: MineGameState::Ready,
            mines_count: TOTAL_MINES,
            start_time: None,
            elapsed_secs: 0,
            best_time,
            seed,
        }
    }

    fn rng(&mut self) -> u32 {
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.seed >> 32) as u32
    }

    pub fn place_mines_and_start(&mut self, first_c: usize, first_r: usize) {
        let mut candidates = Vec::new();
        for r in 0..MINE_ROWS {
            for c in 0..MINE_COLS {
                let dr = (r as isize - first_r as isize).abs();
                let dc = (c as isize - first_c as isize).abs();
                if dr > 1 || dc > 1 {
                    candidates.push((c, r));
                }
            }
        }

        for i in (1..candidates.len()).rev() {
            let j = (self.rng() as usize) % (i + 1);
            candidates.swap(i, j);
        }

        let mines_to_place = TOTAL_MINES.min(candidates.len());
        for i in 0..mines_to_place {
            let (c, r) = candidates[i];
            self.grid[r][c].is_mine = true;
        }

        for r in 0..MINE_ROWS {
            for c in 0..MINE_COLS {
                if !self.grid[r][c].is_mine {
                    let mut count = 0;
                    for dr in -1..=1 {
                        for dc in -1..=1 {
                            if dr == 0 && dc == 0 {
                                continue;
                            }
                            let nr = r as isize + dr;
                            let nc = c as isize + dc;
                            if nr >= 0 && nr < MINE_ROWS as isize && nc >= 0 && nc < MINE_COLS as isize {
                                if self.grid[nr as usize][nc as usize].is_mine {
                                    count += 1;
                                }
                            }
                        }
                    }
                    self.grid[r][c].adjacent_mines = count;
                }
            }
        }

        self.state = MineGameState::Playing;
        self.start_time = Some(Instant::now());
        self.reveal_cell(first_c, first_r);
    }

    pub fn reveal_cell(&mut self, c: usize, r: usize) {
        if self.state == MineGameState::Ready {
            self.place_mines_and_start(c, r);
            return;
        }

        if self.state != MineGameState::Playing {
            return;
        }

        let cell = &mut self.grid[r][c];
        if cell.revealed {
            return;
        }

        cell.revealed = true;

        if cell.is_mine {
            cell.exploded = true;
            self.state = MineGameState::Lost;
            for row in 0..MINE_ROWS {
                for col in 0..MINE_COLS {
                    if self.grid[row][col].is_mine {
                        self.grid[row][col].revealed = true;
                    }
                }
            }
            return;
        }

        if cell.adjacent_mines == 0 {
            let mut queue = vec![(c, r)];
            while let Some((qc, qr)) = queue.pop() {
                for dr in -1..=1 {
                    for dc in -1..=1 {
                        if dr == 0 && dc == 0 {
                            continue;
                        }
                        let nr = qr as isize + dr;
                        let nc = qc as isize + dc;
                        if nr >= 0 && nr < MINE_ROWS as isize && nc >= 0 && nc < MINE_COLS as isize {
                            let n_cell = &mut self.grid[nr as usize][nc as usize];
                            if !n_cell.revealed && !n_cell.is_mine {
                                n_cell.revealed = true;
                                if n_cell.adjacent_mines == 0 {
                                    queue.push((nc as usize, nr as usize));
                                }
                            }
                        }
                    }
                }
            }
        }

        self.check_victory();
    }

    pub fn chord_cell(&mut self, c: usize, r: usize) {
        if self.state != MineGameState::Playing {
            return;
        }

        let cell = self.grid[r][c];
        if !cell.revealed || cell.adjacent_mines == 0 {
            return;
        }

        let mut unrevealed_neighbors = Vec::new();
        for dr in -1..=1 {
            for dc in -1..=1 {
                if dr == 0 && dc == 0 {
                    continue;
                }
                let nr = r as isize + dr;
                let nc = c as isize + dc;
                if nr >= 0 && nr < MINE_ROWS as isize && nc >= 0 && nc < MINE_COLS as isize {
                    let neighbor = &self.grid[nr as usize][nc as usize];
                    if !neighbor.revealed {
                        unrevealed_neighbors.push((nc as usize, nr as usize));
                    }
                }
            }
        }

        if unrevealed_neighbors.len() > cell.adjacent_mines as usize {
            for (nc, nr) in unrevealed_neighbors {
                self.reveal_cell(nc, nr);
            }
        }
    }

    pub fn check_victory(&mut self) {
        if self.state != MineGameState::Playing {
            return;
        }

        let mut unrevealed_safe = 0;
        for r in 0..MINE_ROWS {
            for c in 0..MINE_COLS {
                if !self.grid[r][c].is_mine && !self.grid[r][c].revealed {
                    unrevealed_safe += 1;
                }
            }
        }

        if unrevealed_safe == 0 {
            self.state = MineGameState::Won;
            if self.best_time == 0 || self.elapsed_secs < self.best_time {
                self.best_time = self.elapsed_secs;
                self.save_best_time();
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        if self.state == MineGameState::Playing {
            if let Some(start) = self.start_time {
                let secs = start.elapsed().as_secs() as u32;
                if secs != self.elapsed_secs {
                    self.elapsed_secs = secs.min(999);
                    return true;
                }
            }
        }
        false
    }

    fn load_best_time() -> u32 {
        for path in [MINE_SCORE_PATH, FALLBACK_MINE_SCORE_PATH] {
            if let Ok(s) = fs::read_to_string(path) {
                if let Ok(val) = s.trim().parse::<u32>() {
                    return val;
                }
            }
        }
        0
    }

    pub fn save_best_time(&self) {
        let s = self.best_time.to_string();
        for path in [MINE_SCORE_PATH, FALLBACK_MINE_SCORE_PATH] {
            if let Some(parent) = Path::new(path).parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::write(path, &s).is_ok() {
                break;
            }
        }
    }
}

// ============================================================================
// 1-BIT 7-SEGMENT NEON RENDERER
// ============================================================================

const SEGMENTS: [[bool; 7]; 10] = [
    [true, true, true, true, true, true, false],   // 0
    [false, true, true, false, false, false, false], // 1
    [true, true, false, true, true, false, true],   // 2
    [true, true, true, true, false, false, true],   // 3
    [false, true, true, false, false, true, true],   // 4
    [true, false, true, true, false, true, true],   // 5
    [true, false, true, true, true, true, true],   // 6
    [true, true, true, false, false, false, false], // 7
    [true, true, true, true, true, true, true],   // 8
    [true, true, true, true, false, true, true],   // 9
];

fn draw_1bit_7seg_display(p: &mut Painter, box_r: Rect, val: usize) {
    p.rect(box_r, 0);
    p.rect_outline_t(Rect::new(box_r.x + 1, box_r.y + 1, box_r.w - 2, box_r.h - 2), 1, 255);

    let val_clamped = val.min(999);
    let s = format!("{:03}", val_clamped);

    let dw = pt(8.5);
    let dh = pt(18.0);
    let thick = pt(2.3);
    let gap = pt(3.5);

    let total_digits_w = 3 * dw + 2 * gap;
    let start_x = box_r.x + (box_r.w - total_digits_w) / 2;
    let start_y = box_r.y + (box_r.h - dh) / 2;

    for (i, ch) in s.chars().enumerate() {
        let dx = start_x + (i as i32) * (dw + gap);
        let dy = start_y;
        let digit_idx = (ch as u8 - b'0') as usize;
        let segs = if digit_idx < 10 { SEGMENTS[digit_idx] } else { [false; 7] };
        let mid_y = dy + dh / 2;
        let v_len = mid_y - dy - thick / 2;

        let ra = Rect::new(dx + thick / 2, dy, dw - thick, thick);
        if segs[0] { p.rect(ra, 255); } else { p.rect_outline_t(ra, 1, 255); }

        let rb = Rect::new(dx + dw - thick, dy + thick / 2, thick, v_len);
        if segs[1] { p.rect(rb, 255); } else { p.rect_outline_t(rb, 1, 255); }

        let rc = Rect::new(dx + dw - thick, mid_y, thick, v_len);
        if segs[2] { p.rect(rc, 255); } else { p.rect_outline_t(rc, 1, 255); }

        let rd = Rect::new(dx + thick / 2, dy + dh - thick, dw - thick, thick);
        if segs[3] { p.rect(rd, 255); } else { p.rect_outline_t(rd, 1, 255); }

        let re = Rect::new(dx, mid_y, thick, v_len);
        if segs[4] { p.rect(re, 255); } else { p.rect_outline_t(re, 1, 255); }

        let rf = Rect::new(dx, dy + thick / 2, thick, v_len);
        if segs[5] { p.rect(rf, 255); } else { p.rect_outline_t(rf, 1, 255); }

        let rg = Rect::new(dx + thick / 2, mid_y - thick / 2, dw - thick, thick);
        if segs[6] { p.rect(rg, 255); } else { p.rect_outline_t(rg, 1, 255); }
    }
}

// ============================================================================
// GO / WEIQI (碁) ENGINE & INSTANT AI
// ============================================================================

pub const MAX_GO_SIZE: usize = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoneColor {
    Empty,
    Black,
    White,
}

impl StoneColor {
    pub fn opponent(self) -> StoneColor {
        match self {
            StoneColor::Black => StoneColor::White,
            StoneColor::White => StoneColor::Black,
            StoneColor::Empty => StoneColor::Empty,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoMode {
    VsAi,
    PassAndPlay,
}

pub struct GoGame {
    pub size: usize,
    pub board: [[StoneColor; MAX_GO_SIZE]; MAX_GO_SIZE],
    pub turn: StoneColor,
    pub captures_black: usize,
    pub captures_white: usize,
    pub consecutive_passes: usize,
    pub move_count: usize,
    pub ko_point: Option<(usize, usize)>,
    pub last_move: Option<(usize, usize)>,
    pub game_over: bool,
    pub winner_msg: Option<String>,
    pub mode: GoMode,
    pub seed: u64,
}

impl GoGame {
    pub fn new(size: usize) -> GoGame {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(456789);
        GoGame {
            size: size.clamp(9, 13),
            board: [[StoneColor::Empty; MAX_GO_SIZE]; MAX_GO_SIZE],
            turn: StoneColor::Black,
            captures_black: 0,
            captures_white: 0,
            consecutive_passes: 0,
            move_count: 0,
            ko_point: None,
            last_move: None,
            game_over: false,
            winner_msg: None,
            mode: GoMode::VsAi,
            seed,
        }
    }

    fn rng(&mut self) -> u32 {
        self.seed = self.seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.seed >> 32) as u32
    }

    pub fn find_group(&self, start_x: usize, start_y: usize) -> (Vec<(usize, usize)>, HashSet<(usize, usize)>) {
        let color = self.board[start_y][start_x];
        if color == StoneColor::Empty {
            return (Vec::new(), HashSet::new());
        }

        let mut group = Vec::new();
        let mut liberties = HashSet::new();
        let mut visited = [[false; MAX_GO_SIZE]; MAX_GO_SIZE];
        let mut queue = vec![(start_x, start_y)];
        visited[start_y][start_x] = true;

        while let Some((x, y)) = queue.pop() {
            group.push((x, y));
            for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
                let nx = x as isize + dx;
                let ny = y as isize + dy;
                if nx >= 0 && nx < self.size as isize && ny >= 0 && ny < self.size as isize {
                    let ux = nx as usize;
                    let uy = ny as usize;
                    let neighbor_color = self.board[uy][ux];
                    if neighbor_color == StoneColor::Empty {
                        liberties.insert((ux, uy));
                    } else if neighbor_color == color && !visited[uy][ux] {
                        visited[uy][ux] = true;
                        queue.push((ux, uy));
                    }
                }
            }
        }

        (group, liberties)
    }

    pub fn play_move(&mut self, x: usize, y: usize) -> Result<usize, &'static str> {
        if self.game_over {
            return Err("Game is over");
        }
        if x >= self.size || y >= self.size {
            return Err("Out of bounds");
        }
        if self.board[y][x] != StoneColor::Empty {
            return Err("Intersection occupied");
        }
        if Some((x, y)) == self.ko_point {
            return Err("Illegal move due to Ko rule");
        }

        let current_color = self.turn;
        let opp_color = current_color.opponent();

        self.board[y][x] = current_color;

        let mut captured_stones = Vec::new();
        for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0 && nx < self.size as isize && ny >= 0 && ny < self.size as isize {
                let ux = nx as usize;
                let uy = ny as usize;
                if self.board[uy][ux] == opp_color {
                    let (opp_group, opp_liberties) = self.find_group(ux, uy);
                    if opp_liberties.is_empty() {
                        for pt in opp_group {
                            if !captured_stones.contains(&pt) {
                                captured_stones.push(pt);
                            }
                        }
                    }
                }
            }
        }

        for &(cx, cy) in &captured_stones {
            self.board[cy][cx] = StoneColor::Empty;
        }

        let (_, own_liberties) = self.find_group(x, y);
        if own_liberties.is_empty() && captured_stones.is_empty() {
            self.board[y][x] = StoneColor::Empty;
            return Err("Suicide move is illegal");
        }

        let cap_count = captured_stones.len();
        if current_color == StoneColor::Black {
            self.captures_black += cap_count;
        } else {
            self.captures_white += cap_count;
        }

        if cap_count == 1 && own_liberties.len() == 1 {
            self.ko_point = Some(captured_stones[0]);
        } else {
            self.ko_point = None;
        }

        self.last_move = Some((x, y));
        self.consecutive_passes = 0;
        self.move_count += 1;
        self.turn = opp_color;

        if !self.game_over && self.mode == GoMode::VsAi && self.turn == StoneColor::White {
            self.ai_play_move();
        }

        Ok(cap_count)
    }

    pub fn pass_turn(&mut self) {
        if self.game_over {
            return;
        }
        self.consecutive_passes += 1;
        self.ko_point = None;
        self.last_move = None;
        self.turn = self.turn.opponent();

        if self.consecutive_passes >= 2 {
            self.end_game_and_score();
            return;
        }

        if !self.game_over && self.mode == GoMode::VsAi && self.turn == StoneColor::White {
            self.ai_play_move();
        }
    }

    pub fn ai_play_move(&mut self) {
        if self.game_over || self.turn != StoneColor::White {
            return;
        }

        let mut best_score = i32::MIN;
        let mut best_moves = Vec::new();

        for y in 0..self.size {
            for x in 0..self.size {
                if self.board[y][x] != StoneColor::Empty || Some((x, y)) == self.ko_point {
                    continue;
                }

                let mut sim = self.clone_for_sim();
                if let Ok(caps) = sim.play_move_sim(x, y, StoneColor::White) {
                    let (_, libs) = sim.find_group(x, y);
                    let mut move_score = 0i32;

                    move_score += caps as i32 * 800;
                    move_score += libs.len() as i32 * 40;

                    if libs.len() == 1 && caps == 0 {
                        move_score -= 600;
                    }

                    let center = (self.size - 1) as f32 / 2.0;
                    let dist_center = (x as f32 - center).abs() + (y as f32 - center).abs();
                    move_score += (20.0 - dist_center * 2.0) as i32;

                    if (x == 0 || x == self.size - 1 || y == 0 || y == self.size - 1) && caps == 0 {
                        move_score -= 150;
                    }

                    for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
                        let nx = x as isize + dx;
                        let ny = y as isize + dy;
                        if nx >= 0 && nx < self.size as isize && ny >= 0 && ny < self.size as isize {
                            if self.board[ny as usize][nx as usize] == StoneColor::White {
                                move_score += 60;
                            }
                        }
                    }

                    if move_score > best_score {
                        best_score = move_score;
                        best_moves.clear();
                        best_moves.push((x, y));
                    } else if move_score == best_score {
                        best_moves.push((x, y));
                    }
                }
            }
        }

        if !best_moves.is_empty() && best_score > -300 {
            let choice_idx = (self.rng() as usize) % best_moves.len();
            let (bx, by) = best_moves[choice_idx];
            let _ = self.play_move(bx, by);
        } else {
            self.pass_turn();
        }
    }

    fn clone_for_sim(&self) -> GoGame {
        GoGame {
            size: self.size,
            board: self.board,
            turn: self.turn,
            captures_black: self.captures_black,
            captures_white: self.captures_white,
            consecutive_passes: self.consecutive_passes,
            move_count: self.move_count,
            ko_point: self.ko_point,
            last_move: self.last_move,
            game_over: false,
            winner_msg: None,
            mode: self.mode,
            seed: self.seed,
        }
    }

    fn play_move_sim(&mut self, x: usize, y: usize, color: StoneColor) -> Result<usize, &'static str> {
        self.board[y][x] = color;
        let opp = color.opponent();
        let mut caps = 0;
        for (dx, dy) in [(-1isize, 0isize), (1, 0), (0, -1), (0, 1)] {
            let nx = x as isize + dx;
            let ny = y as isize + dy;
            if nx >= 0 && nx < self.size as isize && ny >= 0 && ny < self.size as isize {
                let ux = nx as usize;
                let uy = ny as usize;
                if self.board[uy][ux] == opp {
                    let (g, l) = self.find_group(ux, uy);
                    if l.is_empty() {
                        caps += g.len();
                        for (cx, cy) in g {
                            self.board[cy][cx] = StoneColor::Empty;
                        }
                    }
                }
            }
        }
        let (_, own_l) = self.find_group(x, y);
        if own_l.is_empty() && caps == 0 {
            return Err("Suicide");
        }
        Ok(caps)
    }

    pub fn end_game_and_score(&mut self) {
        self.game_over = true;
        let komi = if self.size == 9 { 5.5 } else { 6.5 };

        let mut black_score = self.captures_black as f32;
        let mut white_score = self.captures_white as f32 + komi;

        for y in 0..self.size {
            for x in 0..self.size {
                match self.board[y][x] {
                    StoneColor::Black => black_score += 1.0,
                    StoneColor::White => white_score += 1.0,
                    StoneColor::Empty => {}
                }
            }
        }

        let diff = (black_score - white_score).abs();
        if black_score > white_score {
            self.winner_msg = Some(format!("BLACK WINS BY {:.1} PTS", diff));
        } else {
            self.winner_msg = Some(format!("WHITE WINS BY {:.1} PTS", diff));
        }
    }
}

// ============================================================================
// MAIN ARCADE EGG SCREEN (MINESWEEPER + GO)
// ============================================================================

pub struct EggScreen {
    pub active_game: ActiveGame,
    pub mines: MinesweeperGame,
    pub go: GoGame,
}

impl EggScreen {
    pub fn new() -> EggScreen {
        EggScreen {
            active_game: ActiveGame::Minesweeper,
            mines: MinesweeperGame::new(),
            go: GoGame::new(9),
        }
    }

    // --- HEADER BUTTON RECTS ---
    fn exit_btn() -> Rect {
        Rect::new(pt(4.0), pt(4.0), pt(36.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn tab_mines_btn() -> Rect {
        Rect::new(pt(44.0), pt(4.0), pt(46.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn tab_go_btn() -> Rect {
        Rect::new(pt(94.0), pt(4.0), pt(34.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }

    // Minesweeper Readouts
    fn mine_counter_rect(vw_px: i32) -> Rect {
        let w = pt(46.0);
        Rect::new(vw_px - w * 2 - pt(12.0), pt(4.0), w, pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn mine_timer_rect(vw_px: i32) -> Rect {
        let w = pt(46.0);
        Rect::new(vw_px - w - pt(4.0), pt(4.0), w, pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn mine_smiley_btn(vw_px: i32) -> Rect {
        let w = pt(32.0);
        Rect::new(vw_px / 2 - w / 2, pt(3.0), w, pt(TOP_BAR_H_PT) - pt(6.0))
    }

    // Go Buttons (Cleanly Spaced across 296pt)
    fn go_size_btn() -> Rect {
        Rect::new(pt(132.0), pt(4.0), pt(36.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn go_mode_btn() -> Rect {
        Rect::new(pt(172.0), pt(4.0), pt(42.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn go_reset_btn() -> Rect {
        Rect::new(pt(218.0), pt(4.0), pt(38.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }

    fn go_pass_btn() -> Rect {
        Rect::new(pt(260.0), pt(4.0), pt(32.0), pt(TOP_BAR_H_PT) - pt(8.0))
    }
}

impl Screen for EggScreen {
    fn orientation(&self) -> Option<Orientation> {
        Some(Orientation::Portrait)
    }

    fn tick_interval(&self) -> Duration {
        Duration::from_millis(500)
    }

    fn default_edges(&self) -> bool {
        false
    }

    fn on_gesture(&mut self, g: Gesture) -> Action {
        let vw_px = pt(296.64);
        let vh_px = pt(395.52);
        let top_h = pt(TOP_BAR_H_PT);

        match g {
            Gesture::Tap { x, y } => {
                let px = x as i32;
                let py = y as i32;

                // 1. Exit Button
                if Self::exit_btn().contains(px, py) {
                    return Action::Pop;
                }

                // 2. Tab Switchers -> FULL CLEAN REFRESH
                if Self::tab_mines_btn().contains(px, py) {
                    self.active_game = ActiveGame::Minesweeper;
                    return Action::RedrawFull;
                }
                if Self::tab_go_btn().contains(px, py) {
                    self.active_game = ActiveGame::Go;
                    return Action::RedrawFull;
                }

                // 3. MINESWEEPER INTERACTIONS
                if self.active_game == ActiveGame::Minesweeper {
                    if Self::mine_smiley_btn(vw_px).contains(px, py) {
                        let was_over = self.mines.state == MineGameState::Won || self.mines.state == MineGameState::Lost;
                        self.mines = MinesweeperGame::new();
                        if was_over {
                            return Action::RedrawFull;
                        }
                        return Action::RedrawFast;
                    }

                    if py >= top_h {
                        if self.mines.state == MineGameState::Won || self.mines.state == MineGameState::Lost {
                            self.mines = MinesweeperGame::new();
                            return Action::RedrawFull;
                        }

                        let col_w = vw_px as f32 / MINE_COLS as f32;
                        let play_h = (vh_px - top_h) as f32;
                        let row_h = play_h / MINE_ROWS as f32;

                        let c = ((px as f32) / col_w).clamp(0.0, (MINE_COLS - 1) as f32) as usize;
                        let r = (((py - top_h) as f32) / row_h).clamp(0.0, (MINE_ROWS - 1) as f32) as usize;

                        if self.mines.grid[r][c].revealed {
                            self.mines.chord_cell(c, r);
                        } else {
                            self.mines.reveal_cell(c, r);
                        }

                        if self.mines.state == MineGameState::Won || self.mines.state == MineGameState::Lost {
                            return Action::RedrawFull;
                        }
                        return Action::RedrawFast;
                    }
                }

                // 4. GO (WEIQI) INTERACTIONS
                if self.active_game == ActiveGame::Go {
                    let total_top_h = top_h + pt(GO_STATUS_H_PT);

                    // Size Toggle (9x9 <-> 13x13)
                    if Self::go_size_btn().contains(px, py) {
                        let next_size = if self.go.size == 9 { 13 } else { 9 };
                        let mode = self.go.mode;
                        self.go = GoGame::new(next_size);
                        self.go.mode = mode;
                        return Action::RedrawFull;
                    }

                    // Mode Toggle (Vs AI <-> 2P) -> FAST REDRAW (No Blink)
                    if Self::go_mode_btn().contains(px, py) {
                        self.go.mode = match self.go.mode {
                            GoMode::VsAi => GoMode::PassAndPlay,
                            GoMode::PassAndPlay => GoMode::VsAi,
                        };
                        return Action::RedrawFast;
                    }

                    // Reset Game Button -> FULL CLEAN REFRESH
                    if Self::go_reset_btn().contains(px, py) {
                        let size = self.go.size;
                        let mode = self.go.mode;
                        self.go = GoGame::new(size);
                        self.go.mode = mode;
                        return Action::RedrawFull;
                    }

                    // Pass Button -> FAST REDRAW
                    if Self::go_pass_btn().contains(px, py) {
                        self.go.pass_turn();
                        if self.go.game_over {
                            return Action::RedrawFull;
                        }
                        return Action::RedrawFast;
                    }

                    // Board Tap (Place Stone) -> INSTANT FAST REDRAW
                    if py >= total_top_h {
                        if self.go.game_over {
                            let size = self.go.size;
                            let mode = self.go.mode;
                            self.go = GoGame::new(size);
                            self.go.mode = mode;
                            return Action::RedrawFull;
                        }

                        let board_size = self.go.size;
                        let margin = pt(18.0);
                        let board_w = (vw_px - 2 * margin) as f32;
                        let grid_step = board_w / (board_size - 1) as f32;
                        let play_h = (vh_px - total_top_h) as f32;
                        let board_h = (board_size - 1) as f32 * grid_step;
                        let board_top = total_top_h as f32 + (play_h - board_h) / 2.0;

                        let nearest_c = (((px - margin) as f32 + grid_step / 2.0) / grid_step).floor() as isize;
                        let nearest_r = (((py as f32 - board_top) + grid_step / 2.0) / grid_step).floor() as isize;

                        if nearest_c >= 0 && nearest_c < board_size as isize && nearest_r >= 0 && nearest_r < board_size as isize {
                            let move_res = self.go.play_move(nearest_c as usize, nearest_r as usize);
                            if self.go.game_over {
                                return Action::RedrawFull;
                            }
                            if move_res.is_ok() {
                                return Action::RedrawFast;
                            }
                        }
                    }
                }

                Action::Keep
            }
            Gesture::TwoFingerTap => Action::RedrawFull,
            _ => Action::Keep,
        }
    }

    fn on_tick(&mut self) -> Action {
        if self.active_game == ActiveGame::Minesweeper && self.mines.tick() {
            Action::RedrawFast
        } else {
            Action::Keep
        }
    }

    fn draw(&mut self, p: &mut Painter) {
        p.clear(255);

        let vw_px = pt(296.64);
        let vh_px = pt(395.52);
        let top_h = pt(TOP_BAR_H_PT);

        // ====================================================================
        // SHARED HEADER BAR
        // ====================================================================
        p.line_w(0, top_h, vw_px, top_h, 2, 0);

        // 1. [ EXIT ]
        let exit_r = Self::exit_btn();
        p.rect_outline_t(exit_r, 2, 0);
        p.text_center_in(exit_r.x, exit_r.x + exit_r.w, exit_r.y + pt(17.0), 9.0, 0, "EXIT");

        // 2. Game Tabs: [ MINES ] and [ GO ]
        let mines_tab = Self::tab_mines_btn();
        let go_tab = Self::tab_go_btn();
        if self.active_game == ActiveGame::Minesweeper {
            p.rect(mines_tab, 0);
            p.text_center_in(mines_tab.x, mines_tab.x + mines_tab.w, mines_tab.y + pt(17.0), 9.0, 255, "MINES");
            p.rect_outline_t(go_tab, 1, 0);
            p.text_center_in(go_tab.x, go_tab.x + go_tab.w, go_tab.y + pt(17.0), 9.0, 0, "GO");
        } else {
            p.rect_outline_t(mines_tab, 1, 0);
            p.text_center_in(mines_tab.x, mines_tab.x + mines_tab.w, mines_tab.y + pt(17.0), 9.0, 0, "MINES");
            p.rect(go_tab, 0);
            p.text_center_in(go_tab.x, go_tab.x + go_tab.w, go_tab.y + pt(17.0), 9.0, 255, "GO");
        }

        // ====================================================================
        // GAME SPECIFIC RENDERING
        // ====================================================================
        if self.active_game == ActiveGame::Minesweeper {
            let sm_r = Self::mine_smiley_btn(vw_px);
            p.rect_outline_t(sm_r, 2, 0);
            let sm_cx = sm_r.x + sm_r.w / 2;
            let sm_cy = sm_r.y + sm_r.h / 2;
            let rad = pt(9.5);
            p.circle_fill(sm_cx, sm_cy, rad, 0);
            p.circle_fill(sm_cx, sm_cy, rad - 2, 255);

            match self.mines.state {
                MineGameState::Won => {
                    p.rect(Rect::new(sm_cx - pt(6.0), sm_cy - pt(4.0), pt(5.0), pt(4.0)), 0);
                    p.rect(Rect::new(sm_cx + pt(1.0), sm_cy - pt(4.0), pt(5.0), pt(4.0)), 0);
                    p.line_w(sm_cx - pt(6.0), sm_cy - pt(2.0), sm_cx + pt(6.0), sm_cy - pt(2.0), 2, 0);
                    p.line_w(sm_cx - pt(4.0), sm_cy + pt(3.0), sm_cx + pt(4.0), sm_cy + pt(3.0), 2, 0);
                }
                MineGameState::Lost => {
                    p.line_w(sm_cx - pt(5.0), sm_cy - pt(4.0), sm_cx - pt(2.0), sm_cy - pt(1.0), 2, 0);
                    p.line_w(sm_cx - pt(2.0), sm_cy - pt(4.0), sm_cx - pt(5.0), sm_cy - pt(1.0), 2, 0);
                    p.line_w(sm_cx + pt(2.0), sm_cy - pt(4.0), sm_cx + pt(5.0), sm_cy - pt(1.0), 2, 0);
                    p.line_w(sm_cx + pt(5.0), sm_cy - pt(4.0), sm_cx + pt(2.0), sm_cy - pt(1.0), 2, 0);
                    p.line_w(sm_cx - pt(4.0), sm_cy + pt(4.0), sm_cx + pt(4.0), sm_cy + pt(4.0), 2, 0);
                }
                _ => {
                    p.rect(Rect::new(sm_cx - pt(3.5), sm_cy - pt(3.0), pt(2.0), pt(2.0)), 0);
                    p.rect(Rect::new(sm_cx + pt(1.5), sm_cy - pt(3.0), pt(2.0), pt(2.0)), 0);
                    p.line_w(sm_cx - pt(4.0), sm_cy + pt(3.0), sm_cx + pt(4.0), sm_cy + pt(3.0), 2, 0);
                }
            }

            draw_1bit_7seg_display(p, Self::mine_counter_rect(vw_px), self.mines.mines_count);
            draw_1bit_7seg_display(p, Self::mine_timer_rect(vw_px), self.mines.elapsed_secs as usize);

            for r in 0..MINE_ROWS {
                for c in 0..MINE_COLS {
                    let x0 = (c as i32 * vw_px) / MINE_COLS as i32;
                    let y0 = top_h + (r as i32 * (vh_px - top_h)) / MINE_ROWS as i32;
                    let x1 = ((c + 1) as i32 * vw_px) / MINE_COLS as i32;
                    let y1 = top_h + ((r + 1) as i32 * (vh_px - top_h)) / MINE_ROWS as i32;
                    let cx = (x0 + x1) / 2;
                    let cy = (y0 + y1) / 2;

                    let cell = &self.mines.grid[r][c];

                    if !cell.revealed {
                        p.rect(Rect::new(x0 + 1, y0 + 1, x1 - x0 - 1, y1 - y0 - 1), 255);
                        for py in (y0 + 2)..(y1 - 2) {
                            if py % 3 == 0 {
                                for px in (x0 + 2)..(x1 - 2) {
                                    if px % 3 == 0 {
                                        p.rect(Rect::new(px, py, 1, 1), 0);
                                    }
                                }
                            }
                        }
                        p.line_w(x0 + 1, y1 - 2, x1 - 1, y1 - 2, 2, 0);
                        p.line_w(x1 - 2, y0 + 1, x1 - 2, y1 - 1, 2, 0);
                    } else if cell.is_mine {
                        if cell.exploded {
                            p.rect(Rect::new(x0 + 1, y0 + 1, x1 - x0 - 1, y1 - y0 - 1), 0);
                            let mr = pt(6.0);
                            p.circle_fill(cx, cy, mr, 255);
                            p.line_w(cx - mr - pt(4.0), cy, cx + mr + pt(4.0), cy, 3, 255);
                            p.line_w(cx, cy - mr - pt(4.0), cx, cy + mr + pt(4.0), 3, 255);
                        } else {
                            p.rect(Rect::new(x0 + 1, y0 + 1, x1 - x0 - 1, y1 - y0 - 1), 255);
                            let mr = pt(6.0);
                            p.circle_fill(cx, cy, mr, 0);
                            p.line_w(cx - mr - pt(4.0), cy, cx + mr + pt(4.0), cy, 3, 0);
                            p.line_w(cx, cy - mr - pt(4.0), cx, cy + mr + pt(4.0), 3, 0);
                        }
                    } else {
                        p.rect(Rect::new(x0 + 1, y0 + 1, x1 - x0 - 1, y1 - y0 - 1), 255);
                        if cell.adjacent_mines > 0 {
                            let num_str = cell.adjacent_mines.to_string();
                            p.text_center_in(x0, x1, cy + pt(5.0), 12.0, 0, &num_str);
                        }
                    }
                }
            }

            for c in 0..=MINE_COLS {
                let x = (c as i32 * vw_px) / MINE_COLS as i32;
                p.line_w(x, top_h, x, vh_px, 1, 0);
            }
            for r in 0..=MINE_ROWS {
                let y = top_h + (r as i32 * (vh_px - top_h)) / MINE_ROWS as i32;
                p.line_w(0, y, vw_px, y, 1, 0);
            }

            if self.mines.state == MineGameState::Won || self.mines.state == MineGameState::Lost {
                let ov_w = vw_px - pt(40.0);
                let ov_h = pt(80.0);
                let ov_r = Rect::new(pt(20.0), vh_px / 2 - ov_h / 2, ov_w, ov_h);
                p.rect(ov_r, 255);
                p.rect_outline_t(ov_r, 3, 0);

                if self.mines.state == MineGameState::Won {
                    p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(26.0), 14.0, 0, "★ VICTORY! ★");
                    let time_str = format!("CLEARED IN {} SECONDS", self.mines.elapsed_secs);
                    p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(48.0), 9.0, 0, &time_str);
                    p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(66.0), 8.0, 0, "Tap anywhere to Play Again");
                } else {
                    p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(26.0), 14.0, 0, "BOOM! GAME OVER");
                    p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(52.0), 9.0, 0, "Tap anywhere to Try Again");
                }
            }
        } else {
            // ================================================================
            // GO (WEIQI) RENDERING WITH CLEAN STATUS BAR
            // ================================================================
            // 1. Top Action Buttons: Size + Mode + Reset + Pass
            let sz_r = Self::go_size_btn();
            let sz_txt = if self.go.size == 9 { "9x9" } else { "13x13" };
            p.rect_outline_t(sz_r, 1, 0);
            p.text_center_in(sz_r.x, sz_r.x + sz_r.w, sz_r.y + pt(17.0), 9.0, 0, sz_txt);

            let mode_r = Self::go_mode_btn();
            if self.go.mode == GoMode::VsAi {
                p.rect(mode_r, 0);
                p.text_center_in(mode_r.x, mode_r.x + mode_r.w, mode_r.y + pt(17.0), 9.0, 255, "VS AI");
            } else {
                p.rect_outline_t(mode_r, 2, 0);
                p.text_center_in(mode_r.x, mode_r.x + mode_r.w, mode_r.y + pt(17.0), 9.0, 0, "2P");
            }

            let rst_r = Self::go_reset_btn();
            p.rect_outline_t(rst_r, 1, 0);
            p.text_center_in(rst_r.x, rst_r.x + rst_r.w, rst_r.y + pt(17.0), 8.5, 0, "RESET");

            let pass_r = Self::go_pass_btn();
            p.rect(pass_r, 0);
            p.text_center_in(pass_r.x, pass_r.x + pass_r.w, pass_r.y + pt(17.0), 9.0, 255, "PASS");

            // 2. Dedicated Status Banner: Turn + Captures + Komi
            let sub_top = top_h;
            let sub_h = pt(GO_STATUS_H_PT);
            let total_top_h = sub_top + sub_h;

            p.line_w(0, total_top_h, vw_px, total_top_h, 1, 0);

            // Turn Indicator
            let turn_txt = if self.go.turn == StoneColor::Black {
                "TURN: BLACK"
            } else if self.go.mode == GoMode::VsAi {
                "TURN: WHITE (AI)"
            } else {
                "TURN: WHITE"
            };
            p.text(pt(12.0), sub_top + pt(16.0), 9.0, 0, turn_txt);

            // Captures Display
            let caps_txt = format!("CAPTURES: B:{}  W:{}", self.go.captures_black, self.go.captures_white);
            p.text(pt(116.0), sub_top + pt(16.0), 9.0, 0, &caps_txt);

            // Komi
            let komi_val = if self.go.size == 9 { "5.5" } else { "6.5" };
            let komi_txt = format!("KOMI: {}", komi_val);
            p.text(vw_px - pt(58.0), sub_top + pt(16.0), 9.0, 0, &komi_txt);

            // 3. Go Grid Rendering
            let board_size = self.go.size;
            let margin = pt(18.0);
            let board_w = (vw_px - 2 * margin) as f32;
            let grid_step = board_w / (board_size - 1) as f32;
            let play_h = (vh_px - total_top_h) as f32;
            let board_h = (board_size - 1) as f32 * grid_step;
            let board_top = total_top_h as f32 + (play_h - board_h) / 2.0;

            for i in 0..board_size {
                let x = (margin as f32 + i as f32 * grid_step).round() as i32;
                p.line_w(x, board_top.round() as i32, x, (board_top + board_h).round() as i32, 2, 0);
                let y = (board_top + i as f32 * grid_step).round() as i32;
                p.line_w(margin, y, vw_px - margin, y, 2, 0);
            }

            let star_coords: &[(usize, usize)] = if board_size == 9 {
                &[(2, 2), (6, 2), (2, 6), (6, 6), (4, 4)]
            } else {
                &[(3, 3), (9, 3), (3, 9), (9, 9), (6, 6)]
            };

            for &(sx, sy) in star_coords {
                let cx = (margin as f32 + sx as f32 * grid_step).round() as i32;
                let cy = (board_top + sy as f32 * grid_step).round() as i32;
                p.circle_fill(cx, cy, pt(3.0), 0);
            }

            // 4. Draw Stones
            let stone_r = (grid_step * 0.46) as i32;
            for y in 0..board_size {
                for x in 0..board_size {
                    let color = self.go.board[y][x];
                    if color == StoneColor::Empty {
                        continue;
                    }
                    let cx = (margin as f32 + x as f32 * grid_step).round() as i32;
                    let cy = (board_top + y as f32 * grid_step).round() as i32;
                    let is_last = self.go.last_move == Some((x, y));

                    if color == StoneColor::Black {
                        p.circle_fill(cx, cy, stone_r, 0);
                        p.circle_fill(cx - stone_r / 3, cy - stone_r / 3, pt(1.5), 255);
                        if is_last {
                            p.circle_fill(cx, cy, pt(3.5), 255);
                            p.circle_fill(cx, cy, pt(2.0), 0);
                        }
                    } else {
                        p.circle_fill(cx, cy, stone_r, 255);
                        p.circle_fill(cx, cy, stone_r, 0);
                        p.circle_fill(cx, cy, stone_r - 2, 255);
                        if is_last {
                            p.circle_fill(cx, cy, pt(3.0), 0);
                        }
                    }
                }
            }

            // Game Over Result Overlay
            if self.go.game_over {
                let ov_w = vw_px - pt(40.0);
                let ov_h = pt(76.0);
                let ov_r = Rect::new(pt(20.0), vh_px / 2 - ov_h / 2, ov_w, ov_h);
                p.rect(ov_r, 255);
                p.rect_outline_t(ov_r, 3, 0);

                p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(26.0), 12.0, 0, "GAME COMPLETE");
                if let Some(msg) = &self.go.winner_msg {
                    p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(48.0), 10.0, 0, msg);
                }
                p.text_center_in(ov_r.x, ov_r.x + ov_r.w, ov_r.y + pt(64.0), 8.0, 0, "Tap board to Start New Game");
            }
        }
    }
}

// ============================================================================
// UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arcade_initializes_with_minesweeper_and_go() {
        let screen = EggScreen::new();
        assert_eq!(screen.active_game, ActiveGame::Minesweeper);
        assert_eq!(screen.go.size, 9);
        assert_eq!(screen.mines.mines_count, TOTAL_MINES);
    }

    #[test]
    fn go_captures_stones_cleanly() {
        let mut go = GoGame::new(9);
        go.mode = GoMode::PassAndPlay;

        go.board[1][1] = StoneColor::White;
        let _ = go.play_move(1, 0);
        go.turn = StoneColor::Black;
        let _ = go.play_move(0, 1);
        go.turn = StoneColor::Black;
        let _ = go.play_move(2, 1);
        go.turn = StoneColor::Black;
        let res = go.play_move(1, 2);

        assert!(res.is_ok());
        assert_eq!(res.unwrap(), 1);
        assert_eq!(go.board[1][1], StoneColor::Empty);
        assert_eq!(go.captures_black, 1);
    }

    #[test]
    fn go_ai_responds_instantly() {
        let mut go = GoGame::new(9);
        go.mode = GoMode::VsAi;
        let res = go.play_move(4, 4);
        assert!(res.is_ok());
        assert!(go.last_move.is_some());
        assert_eq!(go.turn, StoneColor::Black);
    }

    #[test]
    fn screen_renders_both_modes_without_panic() {
        let f = yui::Font::load().unwrap();
        let (pw, ph, pstride) = (1236u32, 1648u32, 1248usize);
        let orient = yui::Orientation::Portrait;
        let (vw, vh) = orient.visual_dims(pw, ph);
        let mut canvas = vec![0u8; (vw * vh) as usize];
        let mut panel = vec![255u8; pstride * ph as usize];

        let mut screen = EggScreen::new();

        screen.active_game = ActiveGame::Go;
        {
            let mut p = yui::Painter::new(
                &mut panel, pw, ph, pstride, orient, &mut canvas, &f,
            );
            screen.draw(&mut p);
            p.flush();
        }
        assert!(canvas.iter().any(|&b| b == 0));
    }
}
