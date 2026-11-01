import { expect, test } from "vitest";
import { deviceKindLabel, speedLabel } from "./labels";

test("speeds drop trailing zeros", () => {
  expect([1, 1.1, 1.75, 2, 0.8].map(speedLabel)).toEqual(["1×", "1.1×", "1.75×", "2×", "0.8×"]);
});

test("each kind of Connect device has a label", () => {
  expect((["web", "desktop", "android"] as const).map(deviceKindLabel)).toEqual(["Browser", "Desktop app", "Android"]);
});
