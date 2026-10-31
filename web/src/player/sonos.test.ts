import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { POLL_HIDDEN_MS, POLL_VISIBLE_MS, type SonosPlayer, handOffStart, observe, poll } from "./sonos";

const EPISODE = "4b1e0000-0000-0000-0000-000000000001";
const OTHER_EPISODE = "4b1e0000-0000-0000-0000-000000000002";
const COUCH = "RINCON_804AF246458401400";
const KITCHEN = "RINCON_38420B1A2B3C01400";

const player = (id: string, state: SonosPlayer["state"], episodeId: string | null = null): SonosPlayer => ({
  id,
  name: id === COUCH ? "Couch" : "Küche",
  volume: 12,
  state,
  session: episodeId ? { episode_id: episodeId, playing: true, position_ms: 683_000 } : null,
});

test("a hand-off starts where a local play would", () => {
  expect(handOffStart({ positionMs: 754_000, played: false })).toEqual({ positionMs: 754_000, unplay: false });
});

test("handing off a played episode starts it over and unmarks it first", () => {
  expect(handOffStart({ positionMs: 3_000_000, played: true })).toEqual({ positionMs: 0, unplay: true });
});

test("the session goes on while the chosen player still plays the episode for this user", () => {
  const players = [player(KITCHEN, "idle"), player(COUCH, "pod", EPISODE)];
  expect(observe(players, EPISODE, COUCH)).toEqual({ kind: "session", player: players[1], session: players[1]!.session });
});

test("the session is over once the player is idle or plays something else", () => {
  expect(observe([player(COUCH, "idle")], EPISODE, COUCH)).toEqual({ kind: "ended" });
  expect(observe([player(COUCH, "other")], EPISODE, COUCH)).toEqual({ kind: "ended" });
});

test("a session of another episode counts as over, since the player has nothing to show for it", () => {
  expect(observe([player(COUCH, "pod", OTHER_EPISODE)], EPISODE, COUCH)).toEqual({ kind: "ended" });
});

test("a session on another player doesn't count for the chosen one", () => {
  expect(observe([player(COUCH, "idle"), player(KITCHEN, "pod", EPISODE)], EPISODE, COUCH)).toEqual({ kind: "ended" });
});

test("a player missing from the list is missing, not over", () => {
  expect(observe([player(KITCHEN, "idle")], EPISODE, COUCH)).toEqual({ kind: "missing" });
  expect(observe([], EPISODE, COUCH)).toEqual({ kind: "missing" });
});

test("restoring finds the episode's session on any player", () => {
  const players = [player(COUCH, "other"), player(KITCHEN, "pod", EPISODE)];
  expect(observe(players, EPISODE, null)).toMatchObject({ kind: "session", player: { id: KITCHEN } });
  expect(observe([player(COUCH, "pod", OTHER_EPISODE)], EPISODE, null)).toEqual({ kind: "ended" });
  expect(observe([], EPISODE, null)).toEqual({ kind: "ended" });
});

// ==============================================================================
// Polling cadence
// ==============================================================================

let visibility: DocumentVisibilityState = "visible";

function setVisibility(state: DocumentVisibilityState) {
  visibility = state;
  document.dispatchEvent(new Event("visibilitychange"));
}

beforeEach(() => {
  vi.useFakeTimers();
  visibility = "visible";
  vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibility);
});
afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

test("polls every 2 s while visible and every 15 s while hidden", async () => {
  const tick = vi.fn(async () => {});
  const stop = poll(tick);

  await vi.advanceTimersByTimeAsync(POLL_VISIBLE_MS * 3);
  expect(tick).toHaveBeenCalledTimes(3);

  setVisibility("hidden");
  await vi.advanceTimersByTimeAsync(POLL_HIDDEN_MS - 1);
  expect(tick, "the hidden interval replaces the pending visible one").toHaveBeenCalledTimes(3);
  await vi.advanceTimersByTimeAsync(1);
  expect(tick).toHaveBeenCalledTimes(4);

  stop();
  await vi.advanceTimersByTimeAsync(POLL_HIDDEN_MS * 2);
  expect(tick).toHaveBeenCalledTimes(4);
});

test("polls at once when the tab comes back into view", async () => {
  const tick = vi.fn(async () => {});
  const stop = poll(tick);
  setVisibility("hidden");
  await vi.advanceTimersByTimeAsync(5_000);
  expect(tick).not.toHaveBeenCalled();

  setVisibility("visible");
  await vi.advanceTimersByTimeAsync(0);
  expect(tick).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(POLL_VISIBLE_MS);
  expect(tick).toHaveBeenCalledTimes(2);
  stop();
});

test("a failed poll doesn't stop the polling", async () => {
  const tick = vi.fn(async () => {
    throw new Error("502");
  });
  const stop = poll(tick);
  await vi.advanceTimersByTimeAsync(POLL_VISIBLE_MS * 2);
  expect(tick).toHaveBeenCalledTimes(2);
  stop();
});

test("one poll runs at a time, however often the tab is shown", async () => {
  let answer!: () => void;
  const tick = vi.fn(() => new Promise<void>((resolve) => (answer = resolve)));
  const stop = poll(tick);
  await vi.advanceTimersByTimeAsync(POLL_VISIBLE_MS * 5);
  setVisibility("visible");
  expect(tick).toHaveBeenCalledTimes(1);
  answer();
  await vi.advanceTimersByTimeAsync(POLL_VISIBLE_MS);
  expect(tick).toHaveBeenCalledTimes(2);
  stop();
});
