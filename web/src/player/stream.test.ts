import { afterEach, beforeEach, expect, test, vi } from "vitest";
import type { ConnectEvent } from "./connect";
import { STALL_MS, holdStream } from "./stream";

// The stream against a stubbed `fetch`: each call gets the next scripted
// answer, and a body stays open until the test closes it.

type Body = { send: (text: string) => void; close: () => void };

let calls: RequestInit[] = [];
let answers: (() => Response | Promise<Response>)[] = [];

function openBody(): [Response, Body] {
  const encoder = new TextEncoder();
  let controller!: ReadableStreamDefaultController<Uint8Array>;
  const stream = new ReadableStream<Uint8Array>({ start: (c) => void (controller = c) });
  const body: Body = { send: (text) => controller.enqueue(encoder.encode(text)), close: () => controller.close() };
  return [new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } }), body];
}

/** Scripts the next connection to open, and returns its body. */
function nextOpens(): Body {
  const [response, body] = openBody();
  answers.push(() => response);
  return body;
}

function handlers() {
  return { onOpen: vi.fn(), onEvent: vi.fn<(event: ConnectEvent) => void>(), onClose: vi.fn(), onUnauthorized: vi.fn() };
}

beforeEach(() => {
  vi.useFakeTimers();
  calls = [];
  answers = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((_path: string, init: RequestInit) => {
      calls.push(init);
      const next = answers.shift();
      if (!next) return Promise.reject(new TypeError("offline"));
      // An abort ends the request like a real one would.
      return new Promise<Response>((resolve, reject) => {
        init.signal?.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")));
        void Promise.resolve(next()).then(resolve);
      });
    }),
  );
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

const STATE = 'data: {"type":"state","devices":[],"playback":null}\n\n';

test("sends the token and hands over each event", async () => {
  const body = nextOpens();
  const on = handlers();
  const stop = holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(0);
  expect((calls[0]!.headers as Record<string, string>).Authorization).toBe("Bearer secret");
  expect(on.onOpen).toHaveBeenCalledTimes(1);

  body.send(STATE + ": ping\n\ndata: {\"type\":\"release\",");
  body.send('"transfer_id":null}\n\n');
  await vi.advanceTimersByTimeAsync(0);
  expect(on.onEvent.mock.calls.map(([event]) => event.type)).toEqual(["state", "release"]);
  stop();
});

test("reconnects after 1 s, doubling, and starts over once a state arrives", async () => {
  const on = handlers();
  const stop = holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(0);
  expect(calls).toHaveLength(1);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(calls).toHaveLength(2);
  await vi.advanceTimersByTimeAsync(1_999);
  expect(calls, "the second wait is 2 s").toHaveLength(2);
  await vi.advanceTimersByTimeAsync(1);
  expect(calls).toHaveLength(3);

  // This one opens and delivers a state, then closes.
  const body = nextOpens();
  await vi.advanceTimersByTimeAsync(4_000);
  expect(calls).toHaveLength(4);
  body.send(STATE);
  body.close();
  await vi.advanceTimersByTimeAsync(0);
  expect(on.onClose).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(calls, "back to 1 s after the state").toHaveLength(5);
  stop();
});

test("a stream that closes before any state keeps doubling the wait", async () => {
  nextOpens().close();
  const on = handlers();
  const stop = holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(0);
  expect(on.onOpen).toHaveBeenCalledTimes(1);
  expect(on.onClose).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(calls).toHaveLength(2);
  nextOpens().close();
  await vi.advanceTimersByTimeAsync(1_999);
  expect(calls).toHaveLength(2);
  await vi.advanceTimersByTimeAsync(1);
  expect(calls).toHaveLength(3);
  stop();
});

test("a 401 signs out and doesn't reconnect", async () => {
  answers.push(() => new Response('{"error":{"code":"unauthenticated","message":"x"}}', { status: 401 }));
  const on = handlers();
  holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(60_000);
  expect(on.onUnauthorized).toHaveBeenCalledTimes(1);
  expect(calls).toHaveLength(1);
});

test("a replaced stream stops for good", async () => {
  const body = nextOpens();
  const on = handlers();
  holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(0);
  body.send(STATE + 'data: {"type":"replaced"}\n\n');
  await vi.advanceTimersByTimeAsync(120_000);
  expect(on.onEvent.mock.calls.map(([event]) => event.type)).toEqual(["state", "replaced"]);
  expect(on.onClose).toHaveBeenCalledTimes(1);
  expect(calls).toHaveLength(1);
});

test("a stream silent for longer than three pings counts as dead", async () => {
  const body = nextOpens();
  const on = handlers();
  const stop = holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(0);
  body.send(STATE);
  await vi.advanceTimersByTimeAsync(STALL_MS - 1);
  expect(on.onClose).not.toHaveBeenCalled();
  await vi.advanceTimersByTimeAsync(1);
  expect(on.onClose).toHaveBeenCalledTimes(1);
  await vi.advanceTimersByTimeAsync(1_000);
  expect(calls).toHaveLength(2);
  stop();
});

test("stopping ends the stream and every retry", async () => {
  nextOpens();
  const on = handlers();
  const stop = holdStream("secret", on);
  await vi.advanceTimersByTimeAsync(0);
  stop();
  await vi.advanceTimersByTimeAsync(120_000);
  expect(calls).toHaveLength(1);
  expect(on.onClose).not.toHaveBeenCalled();
});
