//! 与界面和 AI 实现无关的五子棋规则与棋局状态。

pub(crate) const BOARD: usize = 15;
pub(crate) const CENTER: usize = BOARD / 2;
pub(crate) const DIRECTIONS: [(i32, i32); 4] = [(1, 0), (0, 1), (1, 1), (1, -1)];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cell {
    Empty,
    Black,
    White,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    HumanVsAi,
    HumanVsHuman,
    /// 黑白双方都由大模型驱动；落子合法性仍由 `Game` 统一裁决。
    LlmVsLlm,
}

impl Mode {
    /// 按应用界面的固定顺序切换游戏模式。
    pub(crate) fn next(self) -> Self {
        match self {
            Self::HumanVsAi => Self::HumanVsHuman,
            Self::HumanVsHuman => Self::LlmVsLlm,
            Self::LlmVsLlm => Self::HumanVsAi,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Playing,
    Won(Cell),
    Draw,
}

pub(crate) struct Game {
    pub(crate) board: [[Cell; BOARD]; BOARD],
    pub(crate) turn: Cell,
    pub(crate) status: Status,
    pub(crate) mode: Mode,
    pub(crate) history: Vec<(usize, usize)>,
    pub(crate) win_line: Vec<(usize, usize)>,
}

impl Game {
    pub(crate) fn new(mode: Mode) -> Self {
        Self {
            board: [[Cell::Empty; BOARD]; BOARD],
            turn: Cell::Black,
            status: Status::Playing,
            mode,
            history: Vec::new(),
            win_line: Vec::new(),
        }
    }

    pub(crate) fn place(&mut self, x: usize, y: usize) -> bool {
        if self.status != Status::Playing
            || x >= BOARD
            || y >= BOARD
            || self.board[y][x] != Cell::Empty
        {
            return false;
        }
        self.board[y][x] = self.turn;
        self.history.push((x, y));
        if let Some(line) = winning_line(&self.board, x, y) {
            self.status = Status::Won(self.turn);
            self.win_line = line;
        } else if self.history.len() == BOARD * BOARD {
            self.status = Status::Draw;
        } else {
            self.turn = opponent(self.turn);
        }
        true
    }

    pub(crate) fn undo(&mut self) {
        let steps = match self.mode {
            Mode::HumanVsAi if self.history.len().is_multiple_of(2) => 2,
            _ => 1,
        };
        for _ in 0..steps {
            if let Some((x, y)) = self.history.pop() {
                self.board[y][x] = Cell::Empty;
            }
        }
        self.status = Status::Playing;
        self.win_line.clear();
        self.turn = if self.history.len().is_multiple_of(2) {
            Cell::Black
        } else {
            Cell::White
        };
    }
}

pub(crate) fn opponent(cell: Cell) -> Cell {
    match cell {
        Cell::Black => Cell::White,
        Cell::White => Cell::Black,
        Cell::Empty => Cell::Empty,
    }
}

pub(crate) fn in_board(x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && (x as usize) < BOARD && (y as usize) < BOARD
}

pub(crate) fn winning_line(
    board: &[[Cell; BOARD]; BOARD],
    x: usize,
    y: usize,
) -> Option<Vec<(usize, usize)>> {
    if x >= BOARD || y >= BOARD {
        return None;
    }
    let player = board[y][x];
    if player == Cell::Empty {
        return None;
    }
    for (dx, dy) in DIRECTIONS {
        let mut line = vec![(x, y)];
        for direction in [1i32, -1i32] {
            let (mut cx, mut cy) = (x as i32 + dx * direction, y as i32 + dy * direction);
            while in_board(cx, cy) && board[cy as usize][cx as usize] == player {
                line.push((cx as usize, cy as usize));
                cx += dx * direction;
                cy += dy * direction;
            }
        }
        if line.len() >= 5 {
            return Some(line);
        }
    }
    None
}

#[cfg(test)]
#[path = "game/tests.rs"]
mod tests;
