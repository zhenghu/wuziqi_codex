use crate::game::{BOARD, Cell, Game, Mode};

pub(crate) type Board = [[Cell; BOARD]; BOARD];

pub(crate) fn empty_board() -> Board {
    [[Cell::Empty; BOARD]; BOARD]
}

pub(crate) fn put(board: &mut Board, stones: &[(usize, usize)], cell: Cell) {
    for &(x, y) in stones {
        board[y][x] = cell;
    }
}

pub(crate) fn play_black_horizontal_win(mode: Mode) -> Game {
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
