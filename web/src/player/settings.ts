// The synced settings the player applies (spec 2.4). Mirrors Android's
// `PlayerSettings.kt` and pod-desktop's `player/settings.rs`.

export type PlayerSettings = {
  speed: number;
  skipBackMs: number;
  skipForwardMs: number;
  playedThresholdMs: number;
};

export const DEFAULT_PLAYER_SETTINGS: PlayerSettings = {
  speed: 1,
  skipBackMs: 15_000,
  skipForwardMs: 30_000,
  playedThresholdMs: 30_000,
};

/** The speeds the player accepts from the setting. Anything outside a range a person would listen at is more likely a bad value than a wish. */
const SPEED_RANGE = [0.25, 4] as const;

/** The speeds the speed control steps through. */
export const SPEEDS = [0.8, 1, 1.1, 1.2, 1.3, 1.5, 1.75, 2];

/** The skip intervals the skip menus offer. */
export const SKIP_STEPS_MS = [5_000, 10_000, 15_000, 30_000, 45_000, 60_000];

/**
 * Reads the settings by key. A missing value, one of the wrong type or one out
 * of range reads as the default, as spec 2.4 asks of clients.
 */
export function playerSettings(values: Record<string, unknown>): PlayerSettings {
  const millis = (key: string, fallback: number) => {
    const value = values[key];
    return typeof value === "number" && Number.isInteger(value) && value >= 0 ? value : fallback;
  };
  const speed = values.playback_speed;
  return {
    speed: typeof speed === "number" && speed >= SPEED_RANGE[0] && speed <= SPEED_RANGE[1] ? speed : DEFAULT_PLAYER_SETTINGS.speed,
    skipBackMs: millis("skip_back_ms", DEFAULT_PLAYER_SETTINGS.skipBackMs),
    skipForwardMs: millis("skip_forward_ms", DEFAULT_PLAYER_SETTINGS.skipForwardMs),
    playedThresholdMs: millis("played_threshold_ms", DEFAULT_PLAYER_SETTINGS.playedThresholdMs),
  };
}

/** The step after `current` in `steps`, wrapping around. A value between steps moves to the next one up. */
export function nextStep(steps: number[], current: number): number {
  return steps.find((step) => step > current) ?? steps[0]!;
}
