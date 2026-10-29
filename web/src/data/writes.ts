import { createRoot, createEffect, createSignal, onCleanup } from "solid-js";
import type { Mutation, MutationResult } from "../api/types";
import { api, session } from "../session";
import { Outbox, type Change } from "../writes";

// The outbox of the signed-in user (writes.ts). A new one loads whenever the
// user changes, so a queue saved under one account is only ever sent by it.

const [outbox, setOutbox] = createSignal<Outbox | null>(null);
const [pendingWrites, setPendingWrites] = createSignal(0);

createRoot(() => {
  createEffect(() => {
    const userId = session()?.userId;
    if (!userId) {
      setOutbox(null);
      return;
    }
    const box = new Outbox({
      send: async (mutations: Mutation[], options) =>
        (await api<{ results: MutationResult[] }>("POST", "/v1/sync", { body: { mutations }, ...options })).results,
      deviceId: () => session()?.deviceId ?? "",
      storageKey: `pod.outbox.${userId}`,
      onChange: setPendingWrites,
    });
    setPendingWrites(box.pending());
    setOutbox(box);
    void box.flush();

    const online = () => void box.flush();
    // `pagehide` is the last event a closing tab reliably gets.
    const leaving = () => box.flushOnExit();
    window.addEventListener("online", online);
    window.addEventListener("pagehide", leaving);
    onCleanup(() => {
      window.removeEventListener("online", online);
      window.removeEventListener("pagehide", leaving);
    });
  });
});

/** Queues a write for the signed-in user. Without a session it is dropped; there is no one to send it as. */
export function write(change: Change, options?: { lazy?: boolean }): void {
  outbox()?.write(change, options);
}

export function flushWrites(): Promise<void> {
  return outbox()?.flush() ?? Promise.resolve();
}

export { outbox };

/** How many writes haven't reached the server yet. */
export { pendingWrites };

/**
 * Forgets what this browser keeps for `userId`: unsent writes and the restored
 * player. Signing out does this (spec 5.7); a session that merely expired keeps
 * them for when the same user signs in again.
 */
export function discardLocalData(userId: string) {
  try {
    localStorage.removeItem(`pod.outbox.${userId}`);
    localStorage.removeItem(`pod.player.${userId}`);
  } catch {
    // Storage is off; there is nothing to forget.
  }
}
