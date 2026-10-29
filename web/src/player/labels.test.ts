import { expect, test } from "vitest";
import { speedLabel } from "./labels";

test("speeds drop trailing zeros", () => {
  expect([1, 1.1, 1.75, 2, 0.8].map(speedLabel)).toEqual(["1×", "1.1×", "1.75×", "2×", "0.8×"]);
});
