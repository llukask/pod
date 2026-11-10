import { expect, test } from "vitest";
import { chapterAt } from "./chapters";

const chapters = [{ start_ms: 5_000 }, { start_ms: 60_000 }, { start_ms: 120_000 }];

test.each([
  [0, -1],
  [4_999, -1],
  [5_000, 0],
  [59_999, 0],
  [60_000, 1],
  [3_600_000, 2],
])("position %d is in chapter %d", (positionMs, index) => {
  expect(chapterAt(chapters, positionMs)).toBe(index);
});

test("no chapters hold nothing", () => {
  expect(chapterAt([], 1_000)).toBe(-1);
});
