import type { Chapter, ChapterList } from "../api/types";
import { api } from "../session";

// ==============================================================================
// Chapters (spec 17.4)
// ==============================================================================
//
// The server resolves an episode's chapters on demand, so the detail pane asks
// when it opens. The answer is kept for the session: chapters only change when
// the publisher rewrites the episode, and reopening an episode shouldn't
// refetch. A failed request isn't kept, so the next open asks again.

const cache = new Map<string, Promise<Chapter[]>>();

/** The episode's chapters, empty when it has none or the request failed. */
export function chaptersOf(episodeId: string): Promise<Chapter[]> {
  const cached = cache.get(episodeId);
  if (cached) return cached;
  const request = api<ChapterList>("GET", `/v1/episodes/${episodeId}/chapters`)
    .then((list) => list.chapters)
    .catch(() => {
      cache.delete(episodeId);
      return [];
    });
  cache.set(episodeId, request);
  return request;
}

/**
 * The index of the chapter that holds `positionMs`: the last one starting at
 * or before it, so a chapter ends where the next begins (spec 17.1). -1
 * before the first chapter starts.
 */
export function chapterAt(chapters: readonly Pick<Chapter, "start_ms">[], positionMs: number): number {
  let index = -1;
  for (const [i, chapter] of chapters.entries()) {
    if (chapter.start_ms > positionMs) break;
    index = i;
  }
  return index;
}
