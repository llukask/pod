// ==============================================================================
// Writes: the outbox in front of POST /v1/sync
// ==============================================================================
//
// The web client keeps no local copy of the user's data, but its writes follow
// spec 5.2 all the same. Each action becomes a mutation stamped with this
// session's device id and a millisecond that never repeats for its field, and
// waits in an outbox keyed by field (the coalesce key), where a newer action
// replaces an older one that hasn't been sent yet. A request carries what is
// queued; its answer removes exactly the mutations it carried, by id, so a
// write made while it was in flight survives.
//
// The outbox is saved to localStorage, so a reload or a crash resends what was
// still waiting. Resending is safe: mutation ids are UUIDv7 (spec 1), the
// server answers a replayed id with its stored result (spec 4.3), and every
// field is last-writer-wins.

import { ApiError } from "./api/http";
import type { Mutation, MutationResult } from "./api/types";
import { uuidv7 } from "./ids";

/** A user action: a mutation before it gets its id, stamp and device. */
export type Change = Mutation extends infer M ? (M extends Mutation ? Omit<M, "id" | "at" | "dev"> : never) : never;

export function coalesceKey(change: Change): string {
  switch (change.type) {
    case "episode.position":
      return `position:${change.episode_id}`;
    case "episode.played":
      return `played:${change.episode_id}`;
    case "episode.archived":
      return `archived:${change.episode_id}`;
    case "setting.set":
      return `setting:${change.key}`;
    case "subscription.set":
      return `sub:${change.podcast_id}`;
  }
}

/** Spec 4.2: more than this in one request is `413 batch_too_large`. */
const MAX_BATCH = 200;

/** Playback positions wait this long, so playing doesn't send a request every 5 s (spec 5.2, step 3). */
const LAZY_DELAY_MS = 30_000;

/** Every other write goes out after this debounce. */
const PROMPT_DELAY_MS = 2_000;

/** Retries after a failure that can succeed later back off from here, doubling up to the cap. */
const RETRY_BASE_MS = 5_000;
const RETRY_CAP_MS = 5 * 60_000;

type Entry = { key: string; mutation: Mutation };

export type OutboxOptions = {
  send: (mutations: Mutation[], options?: { keepalive?: boolean }) => Promise<MutationResult[]>;
  deviceId: () => string;
  /** localStorage key; one per user, so another account never sends this one's writes. */
  storageKey: string;
  now?: () => number;
};

export class Outbox {
  private queue: Entry[];
  private inFlight: Entry[] | null = null;
  private flushing: Promise<void> | null = null;
  /** The last stamp per coalesce key, so two actions in one millisecond still order (5.2, step 1). */
  private lastAt = new Map<string, number>();
  private timer: ReturnType<typeof setTimeout> | undefined;
  private timerDue = Infinity;
  private failures = 0;
  private rejectedListeners = new Set<(mutation: Mutation, code: string) => void>();

  constructor(private readonly options: OutboxOptions) {
    this.queue = this.load();
  }

  /**
   * Queues `change` and returns the mutation it became. `lazy` is for playback
   * positions, which may wait for a later request.
   */
  write(change: Change, { lazy = false } = {}): Mutation {
    const key = coalesceKey(change);
    const now = (this.options.now ?? Date.now)();
    const at = Math.max(now, (this.lastAt.get(key) ?? -Infinity) + 1);
    this.lastAt.set(key, at);
    const mutation = {
      ...change,
      id: uuidv7(now),
      at: new Date(at).toISOString(),
      dev: this.options.deviceId(),
    } as Mutation;
    this.queue = this.queue.filter((entry) => entry.key !== key);
    this.queue.push({ key, mutation });
    this.save();
    this.schedule(lazy ? LAZY_DELAY_MS : PROMPT_DELAY_MS);
    return mutation;
  }

  /** Mutations not yet answered, in flight included. */
  pending(): number {
    return this.queue.length + (this.inFlight?.length ?? 0);
  }

  onRejected(listener: (mutation: Mutation, code: string) => void): () => void {
    this.rejectedListeners.add(listener);
    return () => this.rejectedListeners.delete(listener);
  }

  /** Sends what is queued now. Resolves when that request is answered, or at once if one is already out. */
  flush(): Promise<void> {
    if (this.flushing) return this.flushing;
    if (this.queue.length === 0) return Promise.resolve();
    this.clearTimer();
    this.flushing = this.send().finally(() => (this.flushing = null));
    return this.flushing;
  }

  /**
   * The page is going away: hand everything still waiting to a request that
   * may outlive it. The entries stay saved, so if it doesn't arrive the next
   * load resends them under the same ids.
   */
  flushOnExit(): void {
    const all = [...(this.inFlight ?? []), ...this.queue];
    if (all.length === 0) return;
    this.options.send(all.slice(0, MAX_BATCH).map((entry) => entry.mutation), { keepalive: true }).catch(() => {});
  }

  private async send(): Promise<void> {
    const batch = this.queue.splice(0, MAX_BATCH);
    this.inFlight = batch;
    try {
      const results = await this.options.send(batch.map((entry) => entry.mutation));
      this.failures = 0;
      for (const result of results) {
        if (result.status !== "rejected") continue;
        const entry = batch.find((e) => e.mutation.id === result.id);
        const code = result.code ?? "unknown";
        console.warn(`pod: the server rejected a ${entry?.mutation.type} (${code})`);
        if (entry) for (const listener of this.rejectedListeners) listener(entry.mutation, code);
      }
    } catch (error) {
      const keep = error instanceof ApiError && (error.retryable || error.status === 401);
      if (keep) {
        // Back to the front, unless a newer write for the same field arrived
        // meanwhile; that one replaces it as it would have in the queue.
        const queuedKeys = new Set(this.queue.map((entry) => entry.key));
        this.queue.unshift(...batch.filter((entry) => !queuedKeys.has(entry.key)));
        // A 401 has ended the session; the batch waits for the same user to
        // sign in again, when a new outbox loads it from storage.
        if (error.status !== 401) {
          this.failures++;
          this.schedule(Math.min(RETRY_BASE_MS * 2 ** (this.failures - 1), RETRY_CAP_MS));
        }
      } else {
        console.error("pod: dropping writes the server refused", error);
      }
    } finally {
      this.inFlight = null;
      this.save();
    }
    // More than one batch was waiting.
    if (this.queue.length > 0 && this.failures === 0) this.schedule(0);
  }

  private schedule(delay: number) {
    const due = Date.now() + delay;
    if (this.timer !== undefined && this.timerDue <= due) return;
    this.clearTimer();
    this.timerDue = due;
    this.timer = setTimeout(() => {
      this.timer = undefined;
      this.timerDue = Infinity;
      void this.flush();
    }, delay);
  }

  private clearTimer() {
    clearTimeout(this.timer);
    this.timer = undefined;
    this.timerDue = Infinity;
  }

  private load(): Entry[] {
    try {
      const stored = JSON.parse(localStorage.getItem(this.options.storageKey) ?? "[]");
      return Array.isArray(stored) ? stored : [];
    } catch {
      return [];
    }
  }

  private save() {
    try {
      const all = [...(this.inFlight ?? []), ...this.queue];
      if (all.length === 0) localStorage.removeItem(this.options.storageKey);
      else localStorage.setItem(this.options.storageKey, JSON.stringify(all));
    } catch {
      // Storage is off; the outbox lives as long as the tab.
    }
  }
}
