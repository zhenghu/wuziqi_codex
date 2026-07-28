//! 五子棋的落点评分、战术识别与限宽搜索。

use crate::game::{BOARD, CENTER, Cell, DIRECTIONS, in_board, opponent, winning_line};
use std::collections::HashMap;
use std::time::{Duration, Instant};

const ROOT_CANDIDATE_LIMIT: usize = 12;
const MATE_SCORE: i64 = 1_000_000_000_000;
const MAX_SEARCH_DEPTH: u8 = 4;
const SEARCH_NODE_BUDGET: usize = 100;
const SEARCH_SAFETY_TIMEOUT: Duration = Duration::from_secs(2);

/// 在 (x,y) 为 p 落子后, 单方向的 (连子数, 开放端数)
pub(crate) fn line_stat(
    board: &[[Cell; BOARD]; BOARD],
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    p: Cell,
) -> (u32, u32) {
    let mut count = 1u32;
    let mut open = 0u32;
    for dir in [1i32, -1i32] {
        let (mut cx, mut cy) = (x as i32 + dx * dir, y as i32 + dy * dir);
        while in_board(cx, cy) && board[cy as usize][cx as usize] == p {
            count += 1;
            cx += dx * dir;
            cy += dy * dir;
        }
        if in_board(cx, cy) && board[cy as usize][cx as usize] == Cell::Empty {
            open += 1;
        }
    }
    (count, open)
}

pub(crate) fn pattern_score(count: u32, open: u32) -> i64 {
    match (count, open) {
        (c, _) if c >= 5 => 10_000_000,
        (4, 2) => 1_000_000,
        (4, 1) => 120_000,
        (3, 2) => 60_000,
        (3, 1) => 2_000,
        (2, 2) => 800,
        (2, 1) => 150,
        (1, 2) => 40,
        (1, 1) => 10,
        _ => 0,
    }
}

pub(crate) fn point_score(board: &[[Cell; BOARD]; BOARD], x: usize, y: usize, p: Cell) -> i64 {
    let mut total = 0i64;
    for (dx, dy) in DIRECTIONS {
        let (count, open) = line_stat(board, x, y, dx, dy, p);
        total += pattern_score(count, open) + broken_pattern_score(board, x, y, dx, dy, p);
    }
    total
}

/// 补充连续计数无法识别的跳四、跳三等带内部空点棋形。
fn broken_pattern_score(
    board: &[[Cell; BOARD]; BOARD],
    x: usize,
    y: usize,
    dx: i32,
    dy: i32,
    p: Cell,
) -> i64 {
    let mut line = String::with_capacity(9);
    for offset in -4i32..=4 {
        let (cx, cy) = (x as i32 + dx * offset, y as i32 + dy * offset);
        let symbol = if offset == 0 {
            'X'
        } else if !in_board(cx, cy) {
            'O'
        } else {
            match board[cy as usize][cx as usize] {
                cell if cell == p => 'X',
                Cell::Empty => '.',
                _ => 'O',
            }
        };
        line.push(symbol);
    }

    if ["XXX.X", "XX.XX", "X.XXX"]
        .iter()
        .any(|pattern| line.contains(pattern))
    {
        110_000
    } else if [".XX.X.", ".X.XX."]
        .iter()
        .any(|pattern| line.contains(pattern))
    {
        35_000
    } else if line.contains(".X.X.X.") {
        12_000
    } else {
        0
    }
}

/// 只考虑已有棋子周围 2 格内的空位
pub(crate) fn near_stone(board: &[[Cell; BOARD]; BOARD], x: usize, y: usize) -> bool {
    for dy in -2i32..=2 {
        for dx in -2i32..=2 {
            let (cx, cy) = (x as i32 + dx, y as i32 + dy);
            if in_board(cx, cy) && board[cy as usize][cx as usize] != Cell::Empty {
                return true;
            }
        }
    }
    false
}

fn candidate_moves(board: &[[Cell; BOARD]; BOARD]) -> Vec<(usize, usize)> {
    let mut moves = Vec::new();
    for y in 0..BOARD {
        for x in 0..BOARD {
            if board[y][x] == Cell::Empty && near_stone(board, x, y) {
                moves.push((x, y));
            }
        }
    }
    if moves.is_empty() && board[CENTER][CENTER] == Cell::Empty {
        moves.push((CENTER, CENTER));
    }
    moves
}

fn is_winning_move(board: &[[Cell; BOARD]; BOARD], x: usize, y: usize, p: Cell) -> bool {
    board[y][x] == Cell::Empty
        && DIRECTIONS
            .iter()
            .any(|&(dx, dy)| line_stat(board, x, y, dx, dy, p).0 >= 5)
}

pub(crate) fn immediate_winning_moves(
    board: &[[Cell; BOARD]; BOARD],
    p: Cell,
) -> Vec<(usize, usize)> {
    candidate_moves(board)
        .into_iter()
        .filter(|&(x, y)| is_winning_move(board, x, y, p))
        .collect()
}

fn count_immediate_wins(board: &[[Cell; BOARD]; BOARD], p: Cell, stop_after: usize) -> usize {
    let mut count = 0;
    for (x, y) in candidate_moves(board) {
        if is_winning_move(board, x, y, p) {
            count += 1;
            if count == stop_after {
                break;
            }
        }
    }
    count
}

/// 返回落子后能同时产生至少两个立即获胜点的位置（双杀）。
pub(crate) fn double_threat_moves(board: &[[Cell; BOARD]; BOARD], p: Cell) -> Vec<(usize, usize)> {
    let mut threats = Vec::new();
    for (x, y) in candidate_moves(board) {
        if is_winning_move(board, x, y, p) {
            continue;
        }
        let mut next = *board;
        next[y][x] = p;
        if count_immediate_wins(&next, p, 2) >= 2 {
            threats.push((x, y));
        }
    }
    threats
}

fn move_order_score(board: &[[Cell; BOARD]; BOARD], x: usize, y: usize, p: Cell) -> i64 {
    let attack = point_score(board, x, y, p);
    let defend = point_score(board, x, y, opponent(p));
    let center_bias =
        (BOARD - 1) as i64 - ((x as i64 - CENTER as i64).abs() + (y as i64 - CENTER as i64).abs());
    attack * 10 + defend * 9 + center_bias
}

/// 必选战术点排在最前，随后追加评分最高的普通候选。
fn ranked_moves(
    board: &[[Cell; BOARD]; BOARD],
    p: Cell,
    limit: usize,
    required: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    let mut scored: Vec<_> = candidate_moves(board)
        .into_iter()
        .map(|(x, y)| ((x, y), move_order_score(board, x, y, p)))
        .collect();
    scored.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.1.cmp(&b.0.1))
            .then_with(|| a.0.0.cmp(&b.0.0))
    });

    let mut selected = Vec::new();
    for &((x, y), _) in &scored {
        if required.contains(&(x, y)) {
            selected.push((x, y));
        }
    }
    let mut regular = 0;
    for &((x, y), _) in &scored {
        if selected.contains(&(x, y)) {
            continue;
        }
        if regular == limit {
            break;
        }
        selected.push((x, y));
        regular += 1;
    }
    selected
}

fn threat_value(board: &[[Cell; BOARD]; BOARD], p: Cell) -> i64 {
    let (mut first, mut second) = (0, 0);
    for (x, y) in candidate_moves(board) {
        let score = point_score(board, x, y, p);
        if score > first {
            second = first;
            first = score;
        } else if score > second {
            second = score;
        }
    }
    first * 4 + second
}

fn evaluate_position(board: &[[Cell; BOARD]; BOARD], ai: Cell) -> i64 {
    threat_value(board, ai) * 10 - threat_value(board, opponent(ai)) * 9
}

#[derive(Clone, Copy)]
struct CachedScore {
    depth: u8,
    score: i64,
}

struct SearchContext {
    deadline: Instant,
    nodes_remaining: usize,
    table: HashMap<u64, CachedScore>,
}

impl SearchContext {
    fn new(node_budget: usize, safety_timeout: Duration) -> Self {
        Self {
            deadline: Instant::now() + safety_timeout,
            nodes_remaining: node_budget,
            table: HashMap::new(),
        }
    }

    fn enter_node(&mut self) -> bool {
        if self.nodes_remaining == 0 || Instant::now() >= self.deadline {
            return false;
        }
        self.nodes_remaining -= 1;
        true
    }
}

fn board_hash(board: &[[Cell; BOARD]; BOARD], to_move: Cell) -> u64 {
    let mut hash = match to_move {
        Cell::Black => 0x9e37_79b9_7f4a_7c15,
        Cell::White => 0xc2b2_ae3d_27d4_eb4f,
        Cell::Empty => 0,
    };
    for (index, cell) in board.iter().flatten().enumerate() {
        let value = match cell {
            Cell::Black => (index as u64) * 2 + 1,
            Cell::White => (index as u64) * 2 + 2,
            Cell::Empty => continue,
        };
        hash ^= mix_hash(value);
    }
    hash
}

fn mix_hash(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn occupied_count(board: &[[Cell; BOARD]; BOARD]) -> usize {
    board
        .iter()
        .flatten()
        .filter(|&&cell| cell != Cell::Empty)
        .count()
}

fn candidate_limit(board: &[[Cell; BOARD]; BOARD], depth: u8) -> usize {
    let base: usize = match occupied_count(board) {
        0..=8 => 10,
        9..=30 => 14,
        _ => 18,
    };
    base.saturating_sub(depth.saturating_sub(1) as usize * 2)
        .max(6)
}

fn search_moves(board: &[[Cell; BOARD]; BOARD], p: Cell, depth: u8) -> Vec<(usize, usize)> {
    let wins = immediate_winning_moves(board, p);
    if !wins.is_empty() {
        return ranked_moves(board, p, 0, &wins);
    }
    let opponent_wins = immediate_winning_moves(board, opponent(p));
    if !opponent_wins.is_empty() {
        return ranked_moves(board, p, 0, &opponent_wins);
    }
    let forks = double_threat_moves(board, p);
    if !forks.is_empty() {
        return ranked_moves(board, p, 0, &forks);
    }
    let opponent_forks = double_threat_moves(board, opponent(p));
    ranked_moves(board, p, candidate_limit(board, depth), &opponent_forks)
}

fn minimax(
    board: &[[Cell; BOARD]; BOARD],
    to_move: Cell,
    ai: Cell,
    depth: u8,
    mut alpha: i64,
    mut beta: i64,
    context: &mut SearchContext,
) -> Option<i64> {
    if !context.enter_node() {
        return None;
    }

    let key = board_hash(board, to_move);
    if let Some(cached) = context.table.get(&key) {
        if cached.depth == depth {
            return Some(cached.score);
        }
    }

    let wins = immediate_winning_moves(board, to_move);
    if !wins.is_empty() {
        let score = if to_move == ai {
            MATE_SCORE + depth as i64
        } else {
            -MATE_SCORE - depth as i64
        };
        return Some(score);
    }
    if depth == 0 {
        return Some(evaluate_position(board, ai));
    }

    let moves = search_moves(board, to_move, depth);
    if moves.is_empty() {
        return Some(evaluate_position(board, ai));
    }

    let maximizing = to_move == ai;
    let mut best = if maximizing {
        -MATE_SCORE * 2
    } else {
        MATE_SCORE * 2
    };
    let mut cutoff = false;
    for (x, y) in moves {
        let mut next = *board;
        next[y][x] = to_move;
        let score = if winning_line(&next, x, y).is_some() {
            if maximizing {
                MATE_SCORE + depth as i64
            } else {
                -MATE_SCORE - depth as i64
            }
        } else {
            minimax(
                &next,
                opponent(to_move),
                ai,
                depth - 1,
                alpha,
                beta,
                context,
            )?
        };

        if maximizing {
            best = best.max(score);
            alpha = alpha.max(best);
        } else {
            best = best.min(score);
            beta = beta.min(best);
        }
        if alpha >= beta {
            cutoff = true;
            break;
        }
    }

    if !cutoff {
        context
            .table
            .insert(key, CachedScore { depth, score: best });
    }
    Some(best)
}

fn search_root(
    board: &[[Cell; BOARD]; BOARD],
    ai: Cell,
    roots: &[(usize, usize)],
    depth: u8,
    context: &mut SearchContext,
) -> Option<((usize, usize), i64)> {
    let mut best = *roots.first()?;
    let mut best_score = -MATE_SCORE * 2;
    let mut alpha = -MATE_SCORE * 2;
    for &(x, y) in roots {
        if !context.enter_node() {
            return None;
        }
        let mut next = *board;
        next[y][x] = ai;
        let score = minimax(
            &next,
            opponent(ai),
            ai,
            depth.saturating_sub(1),
            alpha,
            MATE_SCORE * 2,
            context,
        )?;
        if score > best_score {
            best = (x, y);
            best_score = score;
        }
        alpha = alpha.max(best_score);
    }
    Some((best, best_score))
}

pub(crate) fn ai_move(
    board: &[[Cell; BOARD]; BOARD],
    ai: Cell,
    move_count: usize,
) -> (usize, usize) {
    // 开局下中心。
    if move_count == 0 {
        return (CENTER, CENTER);
    }
    let human = opponent(ai);

    // 立即获胜永远优先于防守。
    let ai_wins = immediate_winning_moves(board, ai);
    if !ai_wins.is_empty() {
        return ranked_moves(board, ai, 0, &ai_wins)[0];
    }

    // 对手有直接胜点时必须占据；多个胜点已无法全防，选择其中评分最高者。
    let human_wins = immediate_winning_moves(board, human);
    if !human_wins.is_empty() {
        return ranked_moves(board, ai, 0, &human_wins)[0];
    }

    // 一步制造两个立即胜点即为强制胜。
    let ai_forks = double_threat_moves(board, ai);
    if !ai_forks.is_empty() {
        return ranked_moves(board, ai, 0, &ai_forks)[0];
    }

    // 把对手潜在双杀点强制并入根候选，避免被限宽剪掉。
    let human_forks = double_threat_moves(board, human);
    if human_forks.len() == 1 {
        return human_forks[0];
    }
    let roots = ranked_moves(
        board,
        ai,
        candidate_limit(board, 1).max(ROOT_CANDIDATE_LIMIT),
        &human_forks,
    );
    let Some(&mut_best) = roots.first() else {
        return (CENTER, CENTER);
    };
    let mut best = mut_best;
    let mut context = SearchContext::new(SEARCH_NODE_BUDGET, SEARCH_SAFETY_TIMEOUT);
    for depth in 2..=MAX_SEARCH_DEPTH {
        if let Some((completed_best, _)) = search_root(board, ai, &roots, depth, &mut context) {
            best = completed_best;
        } else {
            break;
        }
    }
    best
}

/// 为大模型提供经过战术约束的候选点。必胜、必防和唯一双杀防守不会被普通限宽淘汰。
pub(crate) fn llm_candidate_moves(
    board: &[[Cell; BOARD]; BOARD],
    ai: Cell,
    move_count: usize,
) -> Vec<(usize, usize)> {
    if move_count == 0 {
        return vec![(CENTER, CENTER)];
    }
    let human = opponent(ai);

    let ai_wins = immediate_winning_moves(board, ai);
    if !ai_wins.is_empty() {
        return ranked_moves(board, ai, 0, &ai_wins);
    }
    let human_wins = immediate_winning_moves(board, human);
    if !human_wins.is_empty() {
        return ranked_moves(board, ai, 0, &human_wins);
    }
    let ai_forks = double_threat_moves(board, ai);
    if !ai_forks.is_empty() {
        return ranked_moves(board, ai, 0, &ai_forks);
    }
    let human_forks = double_threat_moves(board, human);
    if human_forks.len() == 1 {
        return human_forks;
    }
    ranked_moves(
        board,
        ai,
        candidate_limit(board, 1).max(ROOT_CANDIDATE_LIMIT),
        &human_forks,
    )
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn board_hash_includes_stones_and_side_to_move() {
        let mut board = [[Cell::Empty; BOARD]; BOARD];
        let empty_black = board_hash(&board, Cell::Black);
        assert_ne!(empty_black, board_hash(&board, Cell::White));

        board[CENTER][CENTER] = Cell::Black;
        assert_ne!(empty_black, board_hash(&board, Cell::Black));
    }

    #[test]
    fn candidate_width_grows_with_game_phase_and_narrows_with_depth() {
        let mut board = [[Cell::Empty; BOARD]; BOARD];
        let opening_width = candidate_limit(&board, 1);
        assert!(opening_width > 0);

        for index in 0..9 {
            board[index / BOARD][index % BOARD] = Cell::Black;
        }
        let midgame_width = candidate_limit(&board, 1);
        let deeper_midgame_width = candidate_limit(&board, 4);
        assert!(midgame_width > opening_width);
        assert!(deeper_midgame_width < midgame_width);

        for index in 9..31 {
            board[index / BOARD][index % BOARD] = Cell::White;
        }
        let late_game_width = candidate_limit(&board, 1);
        assert!(late_game_width > midgame_width);
        assert!(candidate_limit(&board, 4) < late_game_width);
    }

    #[test]
    fn exhausted_search_returns_without_using_a_partial_iteration() {
        let board = [[Cell::Empty; BOARD]; BOARD];
        let mut context = SearchContext::new(0, SEARCH_SAFETY_TIMEOUT);

        assert!(
            minimax(
                &board,
                Cell::Black,
                Cell::Black,
                4,
                -MATE_SCORE * 2,
                MATE_SCORE * 2,
                &mut context,
            )
            .is_none()
        );
    }
}

#[cfg(test)]
#[path = "ai/tests.rs"]
mod tests;
