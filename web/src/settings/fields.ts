import { SKIP_STEPS_MS, SPEEDS, playerSettings } from "../player/settings";
import { skipLabel, speedLabel } from "../player/labels";

// ==============================================================================
// The synced settings the user picks from steps (wireframe D5, Android M10)
// ==============================================================================
//
// One table drives the settings page. Each field names its key, the steps
// offered and how a value reads, and takes its current value from the same
// readers the player and the server's inbox use, so the page shows what they
// apply: a stored value they reject shows as the default they fall back to.
// The same table as Android's `settings/Fields.kt`, without the device-only
// download settings, which the web client has no use for.

export type Field = {
  key: string;
  title: string;
  /** Explains a setting whose title can't. */
  note?: string;
  steps: number[];
  label: (value: number) => string;
  current: (values: Record<string, unknown>) => number;
};

export const DEFAULT_INBOX_WINDOW_DAYS = 30;

/** A century: `now - window` stays a valid timestamp and covers any back catalogue. */
const MAX_INBOX_WINDOW_DAYS = 36_500;

/** `inbox_window_days` as the inbox reads it (spec 6): a non-negative integer, capped, else the default. */
export function inboxWindowDays(value: unknown): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 0) return DEFAULT_INBOX_WINDOW_DAYS;
  return Math.min(value, MAX_INBOX_WINDOW_DAYS);
}

export const FIELDS: Field[] = [
  {
    key: "playback_speed",
    title: "Speed",
    steps: SPEEDS,
    label: speedLabel,
    current: (values) => playerSettings(values).speed,
  },
  {
    key: "skip_back_ms",
    title: "Skip back",
    steps: SKIP_STEPS_MS,
    label: skipLabel,
    current: (values) => playerSettings(values).skipBackMs,
  },
  {
    key: "skip_forward_ms",
    title: "Skip forward",
    steps: SKIP_STEPS_MS,
    label: skipLabel,
    current: (values) => playerSettings(values).skipForwardMs,
  },
  {
    key: "played_threshold_ms",
    title: "Mark played near the end",
    note: "Within this of the end, or the last 2 % if that's shorter",
    steps: [0, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000],
    label: (ms) => (ms === 0 ? "Off" : skipLabel(ms)),
    current: (values) => playerSettings(values).playedThresholdMs,
  },
  {
    key: "inbox_window_days",
    title: "Inbox shows the last",
    steps: [1, 3, 7, 14, 30, 60, 90, 180, 365],
    label: daysLabel,
    current: (values) => inboxWindowDays(values.inbox_window_days),
  },
];

function daysLabel(days: number): string {
  switch (days) {
    case 1:
      return "1 day";
    case 7:
      return "1 week";
    case 14:
      return "2 weeks";
    case 365:
      return "1 year";
    default:
      return `${days} days`;
  }
}
