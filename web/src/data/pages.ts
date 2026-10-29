import { createSignal } from "solid-js";
import { ApiError } from "../api/http";
import type { Page } from "../api/types";

/**
 * A keyset-paginated list (spec 4.5): the first page on `reload`, the next on
 * `more`. A cursor the server no longer accepts (`invalid_cursor`) restarts
 * from the first page, as the spec asks.
 */
export function createPages<T>(fetchPage: (cursor: string | null) => Promise<Page<T>>) {
  const [items, setItems] = createSignal<T[]>([]);
  const [cursor, setCursor] = createSignal<string | null>(null);
  const [done, setDone] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [error, setError] = createSignal<ApiError | null>(null);
  let generation = 0;

  async function load(from: string | null) {
    const mine = ++generation;
    setLoading(true);
    setError(null);
    try {
      const page = await fetchPage(from);
      if (mine !== generation) return;
      setItems((previous) => (from === null ? page.items : [...previous, ...page.items]));
      setCursor(page.next_cursor ?? null);
      setDone(!page.next_cursor);
    } catch (e) {
      if (mine !== generation) return;
      if (e instanceof ApiError && e.code === "invalid_cursor" && from !== null) return load(null);
      setError(e instanceof ApiError ? e : new ApiError(0, "unknown", String(e)));
    } finally {
      if (mine === generation) setLoading(false);
    }
  }

  return {
    items,
    loading,
    error,
    done,
    reload: () => load(null),
    more: () => {
      if (!loading() && !done()) void load(cursor());
    },
  };
}
