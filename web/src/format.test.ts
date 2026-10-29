import { expect, test } from "vitest";
import { clockLabel, dateLabel, durationLabel, leftLabel, remainingLabel } from "./format";

// Local time throughout; vite.config.ts runs the tests in UTC.
const now = new Date("2026-10-29T15:00:00Z");

test.each([
  ["2026-10-29T05:00:00Z", "Today"],
  ["2026-10-28T23:59:00Z", "Yesterday"],
  ["2026-10-26T08:00:00Z", "Mon"],
  ["2026-10-23T08:00:00Z", "Fri"],
  ["2026-10-22T08:00:00Z", "Oct 22"],
  ["2026-01-14T08:00:00Z", "Jan 14"],
  ["2025-12-30T08:00:00Z", "Dec 30, 2025"],
  // A clock ahead of ours can publish "tomorrow"; it still reads as today.
  ["2026-10-30T01:00:00Z", "Today"],
])("%s is %s", (published, label) => {
  expect(dateLabel(published, now)).toBe(label);
});

test.each([
  [0, "0 min"],
  [29_000, "1 min"],
  [28 * 60_000, "28 min"],
  [60 * 60_000, "1 h"],
  [64 * 60_000 + 10_000, "1 h 4 min"],
  [192 * 60_000, "3 h 12 min"],
])("%i ms lasts %s", (ms, label) => {
  expect(durationLabel(ms)).toBe(label);
});

test("time left rounds up, so an unfinished episode never says 0", () => {
  expect(leftLabel(52 * 60_000, 31 * 60_000)).toBe("21 min left");
  expect(leftLabel(52 * 60_000, 52 * 60_000 - 5_000)).toBe("1 min left");
  expect(leftLabel(77 * 60_000, 0)).toBe("1 h 17 min left");
});

test("the player's clock truncates elapsed time and rounds the remainder up", () => {
  expect(clockLabel(31 * 60_000 + 12_900)).toBe("31:12");
  expect(clockLabel(3_723_000)).toBe("1:02:03");
  expect(remainingLabel(31 * 60_000 + 12_000, 52 * 60_000)).toBe("−20:48");
  expect(remainingLabel(52 * 60_000 - 1, 52 * 60_000)).toBe("−0:01");
  expect(remainingLabel(52 * 60_000, 52 * 60_000)).toBe("−0:00");
});
