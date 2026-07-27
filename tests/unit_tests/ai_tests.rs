use super::super::ai::{
    ai_move, double_threat_moves, immediate_winning_moves, line_stat, llm_candidate_moves,
    near_stone, pattern_score, point_score,
};
use super::super::game::{BOARD, CENTER, Cell};
use super::support::{empty_board, put};

#[test]
fn line_stat_counts_contiguous_stones_and_open_ends() {
    let mut board = empty_board();
    put(&mut board, &[(5, 7), (6, 7), (8, 7)], Cell::Black);
    assert_eq!(line_stat(&board, 7, 7, 1, 0, Cell::Black), (4, 2));

    board[7][9] = Cell::White;
    assert_eq!(line_stat(&board, 7, 7, 1, 0, Cell::Black), (4, 1));

    let mut edge = empty_board();
    edge[0][1] = Cell::Black;
    assert_eq!(line_stat(&edge, 0, 0, 1, 0, Cell::Black), (2, 1));
}

#[test]
fn pattern_score_table_is_stable() {
    let cases = [
        ((5, 0), 10_000_000),
        ((6, 2), 10_000_000),
        ((4, 2), 1_000_000),
        ((4, 1), 120_000),
        ((3, 2), 60_000),
        ((3, 1), 2_000),
        ((2, 2), 800),
        ((2, 1), 150),
        ((1, 2), 40),
        ((1, 1), 10),
        ((4, 0), 0),
        ((0, 2), 0),
    ];

    for ((count, open), expected) in cases {
        assert_eq!(pattern_score(count, open), expected);
    }
}

#[test]
fn point_score_sums_patterns_across_axes() {
    let mut board = empty_board();
    put(&mut board, &[(6, 7), (8, 7), (7, 6), (7, 8)], Cell::Black);
    assert_eq!(point_score(&board, 7, 7, Cell::Black), 120_080);
}

#[test]
fn point_score_recognizes_a_broken_four() {
    let mut board = empty_board();
    put(&mut board, &[(4, 7), (5, 7), (8, 7)], Cell::White);

    assert!(point_score(&board, 7, 7, Cell::White) >= 110_000);
}

#[test]
fn near_stone_uses_a_two_cell_square_radius() {
    let mut board = empty_board();
    assert!(!near_stone(&board, 7, 7));
    board[7][7] = Cell::Black;
    assert!(near_stone(&board, 9, 9));
    assert!(!near_stone(&board, 10, 7));
}

#[test]
fn ai_opens_at_the_center() {
    assert_eq!(ai_move(&empty_board(), Cell::White, 0), (CENTER, CENTER));
}

#[test]
fn ai_takes_a_unique_immediate_win() {
    let mut board = empty_board();
    put(&mut board, &[(0, 7), (1, 7), (2, 7), (3, 7)], Cell::White);
    assert_eq!(ai_move(&board, Cell::White, 4), (4, 7));
}

#[test]
fn ai_blocks_a_unique_immediate_loss() {
    let mut board = empty_board();
    put(&mut board, &[(0, 7), (1, 7), (2, 7), (3, 7)], Cell::Black);
    assert_eq!(ai_move(&board, Cell::White, 4), (4, 7));
}

#[test]
fn ai_prefers_its_own_win_over_blocking() {
    let mut board = empty_board();
    put(&mut board, &[(0, 7), (1, 7), (2, 7), (3, 7)], Cell::White);
    put(&mut board, &[(0, 8), (1, 8), (2, 8), (3, 8)], Cell::Black);
    assert_eq!(ai_move(&board, Cell::White, 8), (4, 7));
}

#[test]
fn ai_is_deterministic_and_returns_a_legal_move() {
    let mut board = empty_board();
    board[7][7] = Cell::Black;
    let before = board;
    let first = ai_move(&board, Cell::White, 1);
    let second = ai_move(&board, Cell::White, 1);
    assert_eq!(first, second);
    assert_eq!(board[first.1][first.0], Cell::Empty);
    assert_eq!(board, before);
}

#[test]
fn llm_candidates_are_legal_and_preserve_forced_defense() {
    let mut board = empty_board();
    put(&mut board, &[(0, 7), (1, 7), (2, 7), (3, 7)], Cell::Black);
    let candidates = llm_candidate_moves(&board, Cell::White, 4);
    assert_eq!(candidates, vec![(4, 7)]);
    assert!(
        candidates
            .iter()
            .all(|&(x, y)| x < BOARD && y < BOARD && board[y][x] == Cell::Empty)
    );
}

#[test]
fn immediate_winning_moves_finds_both_open_four_ends() {
    let mut board = empty_board();
    put(&mut board, &[(5, 7), (6, 7), (7, 7), (8, 7)], Cell::White);
    assert_eq!(
        immediate_winning_moves(&board, Cell::White),
        vec![(4, 7), (9, 7)]
    );
}

#[test]
fn ai_creates_a_forced_win_with_a_double_threat() {
    let mut board = empty_board();
    put(
        &mut board,
        &[(4, 7), (5, 7), (8, 7), (7, 4), (7, 5), (7, 8)],
        Cell::White,
    );
    put(
        &mut board,
        &[
            (10, 10),
            (11, 10),
            (0, 0),
            (14, 14),
            (0, 14),
            (14, 0),
            (1, 12),
        ],
        Cell::Black,
    );
    assert_eq!(double_threat_moves(&board, Cell::White), vec![(7, 7)]);
    let chosen = ai_move(&board, Cell::White, 13);
    assert_eq!(chosen, (7, 7));
    board[chosen.1][chosen.0] = Cell::White;
    assert_eq!(
        immediate_winning_moves(&board, Cell::White),
        vec![(7, 6), (6, 7)]
    );
}

#[test]
fn ai_prevents_the_opponents_next_move_double_threat() {
    let mut board = empty_board();
    put(
        &mut board,
        &[(4, 7), (5, 7), (8, 7), (7, 4), (7, 5), (7, 8)],
        Cell::Black,
    );
    put(
        &mut board,
        &[(10, 10), (11, 10), (0, 0), (14, 14), (0, 14)],
        Cell::White,
    );
    assert_eq!(double_threat_moves(&board, Cell::Black), vec![(7, 7)]);
    let chosen = ai_move(&board, Cell::White, 11);
    assert!([(7, 7), (6, 7), (7, 6)].contains(&chosen));
    board[chosen.1][chosen.0] = Cell::White;
    assert!(double_threat_moves(&board, Cell::Black).is_empty());
}
