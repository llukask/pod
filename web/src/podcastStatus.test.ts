import { expect, test } from "vitest";
import { feedErrorMessage, statusNotice } from "./podcastStatus";

const now = new Date("2026-10-24T09:00:00Z");

test("ok and pending feeds have no notice", () => {
  expect(statusNotice({ status: "ok" }, now)).toBeNull();
  expect(statusNotice({ status: "pending" }, now)).toBeNull();
});

test("a failing feed names the reason and how long it has failed", () => {
  expect(statusNotice({ status: "failing", failing_since: "2026-10-24T07:30:00Z", error: { code: "not_a_feed" } }, now)).toBe(
    "The URL doesn't point to a podcast feed. Pod has kept trying for 1 hour and will keep checking for new episodes.",
  );
  expect(statusNotice({ status: "failing", failing_since: "2026-10-24T08:59:00Z", error: { code: "timeout" } }, now)).toBe(
    "The feed timed out. Pod has kept trying for a few minutes and will keep checking for new episodes.",
  );
  expect(statusNotice({ status: "failing", failing_since: "2026-10-21T08:00:00Z", error: { code: "http", status: 404 } }, now)).toBe(
    "The feed returned HTTP 404. Pod has kept trying for 3 days and will keep checking for new episodes.",
  );
});

test("a gone feed says the server stopped checking", () => {
  expect(statusNotice({ status: "gone" }, now)).toBe("The publisher removed this feed, and Pod has stopped checking for new episodes.");
});

test("an error code this client doesn't know still reads as a sentence", () => {
  expect(feedErrorMessage({ code: "quantum_flux" } as never)).toBe("The feed can't be fetched");
});
