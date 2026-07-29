use super::{BOARD, Cell, Game, Mode, Status, opponent, winning_line};
use crate::test_support::{empty_board, play_black_horizontal_win, put};

#[test]
fn opponent_swaps_colors_and_keeps_empty() {
    assert_eq!(opponent(Cell::Black), Cell::White);
    assert_eq!(opponent(Cell::White), Cell::Black);
    assert_eq!(opponent(Cell::Empty), Cell::Empty);
}

#[test]
fn mode_cycles_through_human_and_llm_matchups() {
    assert_eq!(Mode::HumanVsAi.next(), Mode::HumanVsHuman);
    assert_eq!(Mode::HumanVsHuman.next(), Mode::LlmVsLlm);
    assert_eq!(Mode::LlmVsLlm.next(), Mode::HumanVsAi);
}

#[test]
fn winning_line_detects_all_four_axes() {
    for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
        let mut board = empty_board();
        let expected: Vec<_> = (-2..=2)
            .map(|offset| ((7i32 + dx * offset) as usize, (7i32 + dy * offset) as usize))
            .collect();
        put(&mut board, &expected, Cell::Black);
        let line = winning_line(&board, 7, 7).expect("five stones should win");
        assert_eq!(line.len(), 5);
        assert!(expected.iter().all(|point| line.contains(point)));
    }
}

#[test]
fn winning_line_rejects_a_gap_and_accepts_an_overline() {
    let mut broken = empty_board();
    put(&mut broken, &[(3, 7), (4, 7), (6, 7), (7, 7)], Cell::Black);
    assert!(winning_line(&broken, 3, 7).is_none());
    let mut overline = empty_board();
    let six: Vec<_> = (2..=7).map(|x| (x, 7)).collect();
    put(&mut overline, &six, Cell::White);
    let line = winning_line(&overline, 5, 7).expect("six stones should also win");
    assert_eq!(line.len(), 6);
}

#[test]
fn place_updates_state_and_rejects_an_occupied_cell() {
    let mut game = Game::new(Mode::HumanVsHuman);
    assert!(game.place(7, 7));
    assert_eq!(game.board[7][7], Cell::Black);
    assert_eq!(game.history, vec![(7, 7)]);
    assert_eq!(game.turn, Cell::White);
    assert_eq!(game.status, Status::Playing);
    assert!(!game.place(7, 7));
    assert_eq!(game.history, vec![(7, 7)]);
    assert_eq!(game.turn, Cell::White);
}

#[test]
fn llm_vs_llm_uses_the_standard_placement_and_win_rules() {
    let mut game = Game::new(Mode::LlmVsLlm);
    assert_eq!(game.turn, Cell::Black);
    assert!(game.place(3, 7));
    assert_eq!(game.board[7][3], Cell::Black);
    assert_eq!(game.turn, Cell::White);
    assert!(!game.place(3, 7));
    assert!(game.place(3, 8));

    for x in 4..7 {
        assert!(game.place(x, 7));
        assert!(game.place(x, 8));
    }
    assert!(game.place(7, 7));

    assert_eq!(game.status, Status::Won(Cell::Black));
    assert_eq!(game.win_line.len(), 5);
    assert_eq!(game.mode, Mode::LlmVsLlm);
    assert!(!game.place(8, 8));
}

#[test]
fn place_rejects_out_of_bounds_without_mutating_state() {
    let mut game = Game::new(Mode::HumanVsHuman);
    let board_before = game.board;
    assert!(!game.place(BOARD, 0));
    assert!(!game.place(0, BOARD));
    assert_eq!(game.board, board_before);
    assert!(game.history.is_empty());
    assert_eq!(game.turn, Cell::Black);
    assert_eq!(game.status, Status::Playing);
    assert!(winning_line(&game.board, BOARD, 0).is_none());
    assert!(winning_line(&game.board, 0, BOARD).is_none());
}

#[test]
fn final_non_winning_move_transitions_a_reachable_full_board_to_draw() {
    let final_move = (0, 0);
    let mut black_moves = Vec::new();
    let mut white_moves = Vec::new();

    for y in 0..BOARD {
        for x in 0..BOARD {
            if (x, y) == final_move {
                continue;
            }
            if (x + 2 * y) % 4 < 2 {
                black_moves.push((x, y));
            } else {
                white_moves.push((x, y));
            }
        }
    }

    assert_eq!(black_moves.len(), white_moves.len());
    let mut game = Game::new(Mode::HumanVsHuman);
    for ((black_x, black_y), (white_x, white_y)) in black_moves.into_iter().zip(white_moves) {
        assert!(game.place(black_x, black_y));
        assert!(game.place(white_x, white_y));
    }

    assert_eq!(game.status, Status::Playing);
    assert_eq!(game.turn, Cell::Black);
    assert_eq!(game.history.len(), BOARD * BOARD - 1);
    assert_eq!(game.board[final_move.1][final_move.0], Cell::Empty);

    assert!(game.place(final_move.0, final_move.1));
    assert_eq!(game.status, Status::Draw);
    assert_eq!(game.history.len(), BOARD * BOARD);
    assert!(game.win_line.is_empty());
    assert!(game.board.iter().flatten().all(|&cell| cell != Cell::Empty));
}

#[test]
fn place_transitions_to_won_and_rejects_later_moves() {
    let mut game = play_black_horizontal_win(Mode::HumanVsHuman);
    assert_eq!(game.status, Status::Won(Cell::Black));
    assert_eq!(game.turn, Cell::Black);
    assert_eq!(game.win_line.len(), 5);
    assert!(!game.place(8, 8));
    assert_eq!(game.history.len(), 9);
}

#[test]
fn undo_with_empty_history_is_a_noop() {
    let mut game = Game::new(Mode::HumanVsAi);
    let board_before = game.board;

    game.undo();

    assert_eq!(game.board, board_before);
    assert!(game.history.is_empty());
    assert_eq!(game.turn, Cell::Black);
    assert_eq!(game.status, Status::Playing);
    assert_eq!(game.mode, Mode::HumanVsAi);
    assert!(game.win_line.is_empty());
}

#[test]
fn undo_restores_invariants_in_both_modes() {
    let mut pvp = play_black_horizontal_win(Mode::HumanVsHuman);
    pvp.undo();
    assert_eq!(pvp.status, Status::Playing);
    assert_eq!(pvp.turn, Cell::Black);
    assert_eq!(pvp.history.len(), 8);
    assert_eq!(pvp.board[7][7], Cell::Empty);
    assert!(pvp.win_line.is_empty());

    let mut versus_ai = Game::new(Mode::HumanVsAi);
    assert!(versus_ai.place(7, 7));
    assert!(versus_ai.place(7, 8));
    versus_ai.undo();
    assert!(versus_ai.history.is_empty());
    assert_eq!(versus_ai.board[7][7], Cell::Empty);
    assert_eq!(versus_ai.board[8][7], Cell::Empty);
    assert_eq!(versus_ai.turn, Cell::Black);
    assert_eq!(versus_ai.status, Status::Playing);
}

#[test]
fn undo_before_ai_reply_only_removes_the_latest_human_move() {
    let mut game = Game::new(Mode::HumanVsAi);
    assert!(game.place(7, 7));
    assert!(game.place(7, 8));
    assert!(game.place(8, 7));
    game.undo();
    assert_eq!(game.history, vec![(7, 7), (7, 8)]);
    assert_eq!(game.board[7][7], Cell::Black);
    assert_eq!(game.board[8][7], Cell::White);
    assert_eq!(game.board[7][8], Cell::Empty);
    assert_eq!(game.turn, Cell::Black);
    assert_eq!(game.status, Status::Playing);
    assert!(game.win_line.is_empty());
}

#[test]
fn undo_after_a_human_win_only_removes_the_winning_move() {
    let mut game = play_black_horizontal_win(Mode::HumanVsAi);
    game.undo();
    assert_eq!(game.history.len(), 8);
    assert_eq!(game.board[7][7], Cell::Empty);
    assert_eq!(game.turn, Cell::Black);
    assert_eq!(game.status, Status::Playing);
    assert!(game.win_line.is_empty());
}
