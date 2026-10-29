import { expect, test } from "vitest";
import { nextStep, playerSettings, SKIP_STEPS_MS, SPEEDS } from "./settings";

test("missing settings read as the spec defaults", () => {
  expect(playerSettings({})).toEqual({ speed: 1, skipBackMs: 15_000, skipForwardMs: 30_000, playedThresholdMs: 30_000 });
});

test("stored settings apply and bad values fall back", () => {
  expect(playerSettings({ playback_speed: 1.3, skip_back_ms: 10_000, skip_forward_ms: "far", played_threshold_ms: -1 })).toEqual({
    speed: 1.3,
    skipBackMs: 10_000,
    skipForwardMs: 30_000,
    playedThresholdMs: 30_000,
  });
  expect(playerSettings({ playback_speed: "fast" }).speed).toBe(1);
  expect(playerSettings({ playback_speed: 50 }).speed).toBe(1);
  expect(playerSettings({ skip_back_ms: 1.5 }).skipBackMs, "the spec types it as an int").toBe(15_000);
});

test("steps cycle upwards and wrap", () => {
  expect(nextStep(SPEEDS, 1)).toBe(1.1);
  expect(nextStep(SPEEDS, 1.15), "between steps goes up").toBe(1.2);
  expect(nextStep(SPEEDS, 2)).toBe(0.8);
  expect(nextStep(SKIP_STEPS_MS, 60_000)).toBe(5_000);
});
