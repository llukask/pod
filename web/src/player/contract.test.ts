import { expect, test } from "vitest";
import { Contract, startPosition, type Write } from "./contract";

// Ported from Android's ContractTest, which ports pod-desktop's contract tests.

const thresholdMs = 30_000;
/** 50 minutes, so 2 % (60 s) is above the 30 s threshold. */
const longMs = 50 * 60 * 1000;
const now = 1_000_000;
const secs = (seconds: number) => now + seconds * 1000;
const unplayedAt = (positionMs: number) => ({ positionMs, played: false });
const position = (positionMs: number, whilePlaying: boolean): Write => ({ kind: "position", positionMs, whilePlaying });
const played = (value: boolean): Write => ({ kind: "played", played: value });

test("playback starts at the stored position without writing", () => {
  const [contract, writes] = Contract.start(unplayedAt(754_000), longMs, thresholdMs, now);
  expect(contract.positionMs).toBe(754_000);
  expect(contract.isPlaying).toBe(true);
  expect(writes).toEqual([]);
});

test("replaying a played episode starts over and unmarks it once", () => {
  const stored = { positionMs: longMs, played: true };
  expect(startPosition(stored)).toBe(0);
  const [contract, writes] = Contract.start(stored, longMs, thresholdMs, now);
  expect(contract.positionMs).toBe(0);
  expect(writes).toEqual([played(false)]);
  expect(contract.progress(1_000, secs(1)), "only the start writes played = false").toEqual([]);
});

test("a playing episode saves its position every five seconds", () => {
  const [contract] = Contract.start(unplayedAt(0), longMs, thresholdMs, now);
  const saves = Array.from({ length: 24 }, (_, i) => i + 1).flatMap((tick) => contract.progress(tick * 500, secs(tick * 0.5)));
  expect(saves, "12 seconds of progress save at 5 s and 10 s").toEqual([position(5_000, true), position(10_000, true)]);
});

test("progress while paused saves nothing", () => {
  const [contract] = Contract.start(unplayedAt(0), longMs, thresholdMs, now);
  expect(contract.pause(secs(1))).toEqual([position(0, false)]);
  expect(contract.pause(secs(2)), "already paused").toEqual([]);
  expect(contract.progress(0, secs(30))).toEqual([]);
  expect(contract.isPlaying).toBe(false);
});

test("resuming restarts the five second timer", () => {
  const [contract] = Contract.start(unplayedAt(0), longMs, thresholdMs, now);
  contract.progress(4_000, secs(4));
  contract.pause(secs(4));
  contract.resume(secs(60));
  expect(contract.progress(8_000, secs(64))).toEqual([]);
  expect(contract.progress(9_000, secs(65))).toEqual([position(9_000, true)]);
});

test("pause, seek and stop save at once and reset the timer", () => {
  const [contract] = Contract.start(unplayedAt(60_000), longMs, thresholdMs, now);
  contract.progress(63_000, secs(3));
  expect(contract.seek(93_000, secs(3))).toEqual([position(93_000, false)]);
  expect(contract.progress(97_000, secs(7)), "the seek saved, so the next periodic save is 5 s after it").toEqual([]);
  expect(contract.seek(-5_000, secs(7.5)), "a skip back past the start lands on 0").toEqual([position(0, false)]);
  expect(contract.stop(secs(8))).toEqual([position(0, false)]);
});

test("crossing the threshold marks played once", () => {
  const [contract] = Contract.start(unplayedAt(longMs - 31_000), longMs, thresholdMs, now);
  expect(contract.progress(longMs - 30_000, secs(1))).toEqual([]);
  expect(contract.progress(longMs - 29_999, secs(2))).toEqual([played(true)]);
  expect(contract.progress(longMs - 29_000, secs(3))).toEqual([]);
  expect(contract.ended(secs(4)), "the end doesn't mark it again").toEqual([position(longMs, false)]);
});

test("a short episode uses two percent of its duration", () => {
  // 10 minutes: 2 % is 12 s, below the 30 s threshold.
  const duration = 600_000;
  const [contract] = Contract.start(unplayedAt(0), duration, thresholdMs, now);
  expect(contract.seek(duration - 20_000, now)).toEqual([position(580_000, false)]);
  expect(contract.seek(duration - 11_000, now), "a seek past the threshold marks it too").toEqual([played(true), position(589_000, false)]);
});

test("the player's duration replaces the feed's", () => {
  const [contract] = Contract.start(unplayedAt(0), null, thresholdMs, now);
  expect(contract.progress(longMs, secs(1)), "without a duration there's no threshold").toEqual([]);
  contract.setDuration(longMs + 10_000);
  expect(contract.durationMs).toBe(longMs + 10_000);
  expect(contract.progress(longMs, secs(2))).toEqual([played(true)]);
});

test("reaching the end marks played and saves the end", () => {
  const [unknown] = Contract.start(unplayedAt(0), null, thresholdMs, now);
  expect(unknown.ended(secs(1))).toEqual([played(true), position(0, false)]);
  const [known] = Contract.start(unplayedAt(0), longMs, thresholdMs, now);
  known.progress(longMs - 40_000, secs(1));
  expect(known.ended(secs(2))).toEqual([played(true), position(longMs, false)]);
});
