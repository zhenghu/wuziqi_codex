const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const BOARD = 15;
const CENTER = Math.floor(BOARD / 2);
const EMPTY = 0;
const BLACK = 1;
const WHITE = 2;
const DIRECTIONS = [
  [1, 0],
  [0, 1],
  [1, 1],
  [1, -1],
];

const context = vm.createContext({
  BOARD,
  CENTER,
  EMPTY,
  BLACK,
  WHITE,
  DIRECTIONS,
  opponent: (cell) => (cell === BLACK ? WHITE : cell === WHITE ? BLACK : EMPTY),
  inBoard: (x, y) =>
    Number.isInteger(x) && Number.isInteger(y) && x >= 0 && y >= 0 && x < BOARD && y < BOARD,
});
const aiSource = fs.readFileSync(path.join(__dirname, "..", "ai.js"), "utf8");
vm.runInContext(`${aiSource}\nglobalThis.__webAi = { aiMove };`, context);
const { aiMove } = context.__webAi;

function emptyBoard() {
  return Array.from({ length: BOARD }, () => new Array(BOARD).fill(EMPTY));
}

function put(board, stones, cell) {
  for (const [x, y] of stones) {
    board[y][x] = cell;
  }
}

function moveArray(move) {
  return [move[0], move[1]];
}

test("web AI opens an empty board at the center", () => {
  const board = emptyBoard();

  assert.deepEqual(moveArray(aiMove(board, WHITE, 0)), [CENTER, CENTER]);
});

test("web AI takes a unique immediate win", () => {
  const board = emptyBoard();
  put(board, [[0, 7], [1, 7], [2, 7], [3, 7]], WHITE);

  assert.deepEqual(moveArray(aiMove(board, WHITE, 4)), [4, 7]);
});

test("web AI blocks a unique immediate loss", () => {
  const board = emptyBoard();
  put(board, [[0, 7], [1, 7], [2, 7], [3, 7]], BLACK);

  assert.deepEqual(moveArray(aiMove(board, WHITE, 4)), [4, 7]);
});

test("web AI returns a legal empty position without mutating its input", () => {
  const board = emptyBoard();
  put(board, [[7, 7], [7, 8], [8, 7], [6, 8], [8, 8]], BLACK);
  put(board, [[6, 7], [8, 6], [6, 6], [9, 8]], WHITE);
  const before = board.map((row) => row.slice());

  const [x, y] = moveArray(aiMove(board, WHITE, 9));

  assert.ok(Number.isInteger(x) && Number.isInteger(y));
  assert.ok(x >= 0 && x < BOARD && y >= 0 && y < BOARD);
  assert.equal(before[y][x], EMPTY);
  assert.deepEqual(board, before);
});
