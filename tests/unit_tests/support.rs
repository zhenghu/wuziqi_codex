use super::super::game::{BOARD, Cell, Game, Mode};

pub(super) type Board = [[Cell; BOARD]; BOARD];

pub(super) fn empty_board() -> Board {
    [[Cell::Empty; BOARD]; BOARD]
}

pub(super) fn put(board: &mut Board, stones: &[(usize, usize)], cell: Cell) {
    for &(x, y) in stones {
        board[y][x] = cell;
    }
}

pub(super) fn play_black_horizontal_win(mode: Mode) -> Game {
    let mut game = Game::new(mode);
    for &(x, y) in &[
        (3, 7),
        (0, 0),
        (4, 7),
        (0, 1),
        (5, 7),
        (0, 2),
        (6, 7),
        (0, 3),
        (7, 7),
    ] {
        assert!(game.place(x, y));
    }
    game
}
