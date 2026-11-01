// ==============================================================================
// The Connect event stream (spec 14.2)
// ==============================================================================
//
// `GET /v1/connect/events?kind=web` stays open while the tab is signed in, and
// holding it is what makes this tab a device other devices can see and steer.
// It is read with `fetch`, since `EventSource` can't send the bearer token, and
// parsed by sse.ts. A stream that closes or never opens is tried again after
// 1 s, doubling to 30 s, and the delay starts over once a `state` arrives. A
// 401 ends the session; a `replaced` means another tab of this browser holds
// the device now, and this one stops trying.

import { type ConnectEvent, Backoff } from "./connect";
import { SseParser } from "./sse";

/**
 * How long the stream may stay silent before it counts as dead. The server
 * pings every 15 s, so this is three missed pings: a laptop that slept or a
 * proxy that dropped the connection without closing it.
 */
export const STALL_MS = 45_000;

export type StreamHandlers = {
  /** The server accepted the stream: reports go through from now on (14.3). */
  onOpen: () => void;
  onEvent: (event: ConnectEvent) => void;
  /** The stream closed, before any reconnect. */
  onClose: () => void;
  /** The token was refused. Nothing reconnects. */
  onUnauthorized: () => void;
};

/** Holds the stream for `token` until the returned function is called. */
export function holdStream(token: string, handlers: StreamHandlers): () => void {
  const backoff = new Backoff();
  let stopped = false;
  let connection: AbortController | null = null;
  /** Ends the wait before the next attempt early: when the browser comes back online, or on stop. */
  let wake: (() => void) | null = null;

  const pause = (ms: number) =>
    new Promise<void>((resolve) => {
      const timer = setTimeout(done, ms);
      function done() {
        clearTimeout(timer);
        wake = null;
        resolve();
      }
      wake = done;
    });
  const online = () => wake?.();
  window.addEventListener("online", online);

  /** Reads one connection to its end. Returns whether to try again. */
  async function connect(): Promise<boolean> {
    const controller = new AbortController();
    connection = controller;
    let response: Response;
    try {
      response = await fetch("/v1/connect/events?kind=web", {
        headers: { Authorization: `Bearer ${token}`, Accept: "text/event-stream" },
        cache: "no-store",
        signal: controller.signal,
      });
    } catch {
      return true;
    }
    if (stopped) return false;
    if (response.status === 401) {
      handlers.onUnauthorized();
      return false;
    }
    if (!response.ok || !response.body) return true;

    let stall: ReturnType<typeof setTimeout> | undefined;
    const alive = () => {
      clearTimeout(stall);
      stall = setTimeout(() => controller.abort(), STALL_MS);
    };
    const parser = new SseParser();
    const decoder = new TextDecoder();
    const reader = response.body.getReader();
    // A stall or a stop aborts the request; cancelling the reader as well ends
    // a read that waits on a body the abort doesn't reach.
    controller.signal.addEventListener("abort", () => void reader.cancel().catch(() => {}));
    let replaced = false;
    handlers.onOpen();
    alive();
    try {
      while (!replaced) {
        const { value, done } = await reader.read();
        if (done || stopped) break;
        alive();
        for (const data of parser.push(decoder.decode(value, { stream: true }))) {
          let event: ConnectEvent;
          try {
            event = JSON.parse(data);
          } catch {
            continue;
          }
          if (event.type === "state") backoff.reset();
          if (event.type === "replaced") replaced = true;
          handlers.onEvent(event);
        }
      }
    } catch {
      // Aborted, or the connection broke; either way it's over.
    } finally {
      clearTimeout(stall);
      controller.abort();
    }
    if (!stopped) handlers.onClose();
    return !replaced;
  }

  void (async () => {
    while (!stopped && (await connect()) && !stopped) {
      await pause(backoff.next());
    }
  })();

  return () => {
    stopped = true;
    connection?.abort();
    wake?.();
    window.removeEventListener("online", online);
  };
}
