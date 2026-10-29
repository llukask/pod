import { expect, test } from "vitest";
import { FIELDS, inboxWindowDays } from "./fields";

// Ported from Android's FieldsTest (settings/Fields.kt), minus the
// device-only download fields the web client doesn't have.

const field = (key: string) => FIELDS.find((f) => f.key === key)!;

test("every step reads back as itself", () => {
  for (const f of FIELDS) {
    for (const step of f.steps) expect(f.current({ [f.key]: step }), `${f.key} = ${step}`).toBe(step);
  }
});

test("the defaults are steps", () => {
  for (const f of FIELDS) expect(f.steps, f.key).toContain(f.current({}));
});

test("values are worded like the wireframe", () => {
  expect(FIELDS.map((f) => f.label(f.current({})))).toEqual(["1×", "15 s", "30 s", "30 s", "30 days"]);
  expect(field("played_threshold_ms").label(0)).toBe("Off");
  expect(field("played_threshold_ms").label(120_000)).toBe("2 min");
  expect(field("inbox_window_days").label(365)).toBe("1 year");
  expect(field("inbox_window_days").label(7)).toBe("1 week");
});

test("the inbox window reads like the server's (spec 6)", () => {
  expect(inboxWindowDays(undefined)).toBe(30);
  expect(inboxWindowDays(14)).toBe(14);
  expect(inboxWindowDays(0)).toBe(0);
  expect(inboxWindowDays(-1)).toBe(30);
  expect(inboxWindowDays(2.5)).toBe(30);
  expect(inboxWindowDays("14")).toBe(30);
  expect(inboxWindowDays(1_000_000)).toBe(36_500);
});
