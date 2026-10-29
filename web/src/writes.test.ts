import { afterEach, beforeEach, expect, test, vi } from "vitest";
import { ApiError } from "./api/http";
import type { Mutation, MutationResult } from "./api/types";
import { coalesceKey, Outbox } from "./writes";

const EPISODE = "4b1e0000-0000-0000-0000-000000000001";
const OTHER = "4b1e0000-0000-0000-0000-000000000002";

/** An outbox whose requests wait until the test answers them. */
function outbox(now = () => Date.parse("2026-10-29T12:00:00.000Z")) {
  const sent: { mutations: Mutation[]; answer: (results: MutationResult[] | Error) => void }[] = [];
  const box = new Outbox({
    send: (mutations) =>
      new Promise((resolve, reject) =>
        sent.push({ mutations, answer: (r) => (r instanceof Error ? reject(r) : resolve(r)) }),
      ),
    deviceId: () => "dev-1",
    storageKey: "test.outbox",
    now,
  });
  const applied = (n = 0) => sent[n]!.answer(sent[n]!.mutations.map((m) => ({ id: m.id, status: "applied", at: m.at })));
  return { box, sent, applied };
}

beforeEach(() => {
  localStorage.clear();
  vi.useFakeTimers();
});
afterEach(() => vi.useRealTimers());

test("coalesce keys follow spec 5.2", () => {
  expect(coalesceKey({ type: "episode.position", episode_id: EPISODE, position_ms: 1 })).toBe(`position:${EPISODE}`);
  expect(coalesceKey({ type: "episode.played", episode_id: EPISODE, played: true })).toBe(`played:${EPISODE}`);
  expect(coalesceKey({ type: "episode.archived", episode_id: EPISODE, archived: true })).toBe(`archived:${EPISODE}`);
  expect(coalesceKey({ type: "setting.set", key: "playback_speed", value: 1.5 })).toBe("setting:playback_speed");
  expect(coalesceKey({ type: "subscription.set", podcast_id: "p", feed_url: "https://x", subscribed: true })).toBe("sub:p");
});

test("a newer write replaces a queued one with the same key and goes to the back", async () => {
  const { box, sent } = outbox();
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 1000 });
  box.write({ type: "episode.played", episode_id: OTHER, played: true });
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 2000 });
  void box.flush();
  expect(sent[0]!.mutations.map((m) => [m.type, "position_ms" in m ? m.position_ms : null])).toEqual([
    ["episode.played", null],
    ["episode.position", 2000],
  ]);
});

test("mutations carry an id, the device id and a millisecond stamp that never repeats for a key", async () => {
  const { box, sent, applied } = outbox();
  box.write({ type: "episode.played", episode_id: EPISODE, played: true });
  const answered = box.flush();
  applied(0);
  await answered;
  box.write({ type: "episode.played", episode_id: EPISODE, played: false });
  void box.flush();
  const [first, second] = [sent[0]!.mutations[0]!, sent[1]!.mutations[0]!];
  expect(first.dev).toBe("dev-1");
  expect(first.id).toMatch(/^[0-9a-f-]{36}$/);
  expect(first.at).toBe("2026-10-29T12:00:00.000Z");
  // Same wall-clock millisecond: the second action must still win (5.2, step 1).
  expect(second.at).toBe("2026-10-29T12:00:00.001Z");
});

test("a write during a request queues behind it and survives the response", async () => {
  const { box, sent, applied } = outbox();
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 1000 });
  const flushing = box.flush();
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 2000 });
  applied(0);
  await flushing;
  expect(box.pending()).toBe(1);
  void box.flush();
  expect(sent[1]!.mutations).toMatchObject([{ position_ms: 2000 }]);
});

test("a retryable failure keeps the batch and tries again later", async () => {
  const { box, sent, applied } = outbox();
  box.write({ type: "episode.archived", episode_id: EPISODE, archived: true });
  const flushing = box.flush();
  sent[0]!.answer(new ApiError(0, "network", "offline"));
  await flushing;
  expect(box.pending()).toBe(1);
  await vi.advanceTimersByTimeAsync(5_000);
  expect(sent).toHaveLength(2);
  expect(sent[1]!.mutations[0]!.id).toBe(sent[0]!.mutations[0]!.id);
  applied(1);
  await vi.advanceTimersByTimeAsync(0);
  expect(box.pending()).toBe(0);
});

test("a retried batch yields to a newer write for the same key", async () => {
  const { box, sent } = outbox();
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 1000 });
  const flushing = box.flush();
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 2000 });
  sent[0]!.answer(new ApiError(503, "unknown", "busy"));
  await flushing;
  void box.flush();
  expect(sent[1]!.mutations).toMatchObject([{ position_ms: 2000 }]);
});

test("a failure that can't succeed later drops the batch", async () => {
  const { box, sent } = outbox();
  box.write({ type: "episode.archived", episode_id: EPISODE, archived: true });
  const flushing = box.flush();
  sent[0]!.answer(new ApiError(400, "bad_request", "broken"));
  await flushing;
  expect(box.pending()).toBe(0);
});

test("rejected results reach the listeners", async () => {
  const { box, sent } = outbox();
  const rejected = vi.fn();
  box.onRejected(rejected);
  box.write({ type: "subscription.set", podcast_id: "p", feed_url: "http://10.0.0.1/feed", subscribed: true });
  const flushing = box.flush();
  const mutation = sent[0]!.mutations[0]!;
  sent[0]!.answer([{ id: mutation.id, status: "rejected", code: "invalid_feed_url" }]);
  await flushing;
  expect(rejected).toHaveBeenCalledWith(mutation, "invalid_feed_url");
});

test("a request carries at most 200 mutations", async () => {
  const { box, sent } = outbox();
  for (let n = 0; n < 250; n++) box.write({ type: "setting.set", key: `k${n}`, value: n });
  void box.flush();
  expect(sent[0]!.mutations).toHaveLength(200);
});

test("the queue survives a reload", () => {
  const { box } = outbox();
  box.write({ type: "episode.played", episode_id: EPISODE, played: true });
  const { box: reloaded } = outbox();
  expect(reloaded.pending()).toBe(1);
});

test("position writes wait up to 30 s, other writes 2 s", async () => {
  const { box, sent } = outbox();
  box.write({ type: "episode.position", episode_id: EPISODE, position_ms: 1000 }, { lazy: true });
  await vi.advanceTimersByTimeAsync(29_000);
  expect(sent).toHaveLength(0);
  box.write({ type: "episode.played", episode_id: OTHER, played: true });
  await vi.advanceTimersByTimeAsync(2_000);
  expect(sent).toHaveLength(1);
  expect(sent[0]!.mutations).toHaveLength(2);
});
