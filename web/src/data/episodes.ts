import { createStore } from "solid-js/store";
import type { Episode, InboxItem, Page, Podcast } from "../api/types";
import { api } from "../session";
import { write } from "./writes";

// ==============================================================================
// Episodes as the screens see them
// ==============================================================================
//
// The inbox and a podcast's episode list arrive in different shapes (spec 4.5).
// Both become an `EpisodeView`, so rows, the detail pane and the player take one
// type. The user's state in it is the server's, overlaid with what this tab has
// written since: a write shows everywhere at once, without waiting for a
// refetch.

export type EpisodeView = {
  id: string;
  podcastId: string;
  podcastTitle: string;
  title: string;
  publishedAt: string;
  durationMs: number | null;
  audioUrl: string;
  /** The episode's own artwork, else the show's. */
  artworkUrl: string | null;
  /** Sanitized HTML (spec 2.2), the fuller of the two the feed gave. */
  notesHtml: string;
  link: string | null;
  positionMs: number;
  played: boolean;
  archived: boolean;
};

type Shared = Pick<InboxItem, "id" | "podcast_id" | "title" | "published_at" | "duration_ms" | "audio_url" | "artwork_url" | "content_html" | "summary_html" | "link">;

function base(episode: Shared, podcastTitle: string, podcastArtwork: string | null | undefined) {
  return {
    id: episode.id,
    podcastId: episode.podcast_id,
    podcastTitle,
    title: episode.title ?? "Untitled episode",
    publishedAt: episode.published_at,
    durationMs: episode.duration_ms ?? null,
    audioUrl: episode.audio_url,
    artworkUrl: episode.artwork_url ?? podcastArtwork ?? null,
    notesHtml: episode.content_html || episode.summary_html || "",
    link: episode.link ?? null,
  };
}

/** An inbox item is unplayed and not archived by definition (spec 6). */
export function fromInbox(item: InboxItem): EpisodeView {
  return { ...base(item, item.podcast_title ?? "", item.podcast_artwork_url), positionMs: item.position_ms, played: false, archived: false };
}

export function fromPodcast(episode: Episode, podcast: Pick<Podcast, "title" | "artwork_url" | "feed_url">): EpisodeView {
  const state = episode.episode_state;
  return {
    ...base(episode, podcast.title ?? podcast.feed_url, podcast.artwork_url),
    positionMs: state?.position_ms ?? 0,
    played: state?.played ?? false,
    archived: state?.archived ?? false,
  };
}

type Local = Partial<Pick<EpisodeView, "positionMs" | "played" | "archived">>;

const [local, setLocal] = createStore<Record<string, Local>>({});

/** `episode` with this tab's writes on top. Reactive: reading it in a component tracks the overlay. */
export function current(episode: EpisodeView): EpisodeView {
  const overlay = local[episode.id];
  return overlay ? { ...episode, ...overlay } : episode;
}

export function setPlayed(episode: EpisodeView, played: boolean) {
  setLocal(episode.id, { ...local[episode.id], played });
  write({ type: "episode.played", episode_id: episode.id, played });
}

export function setArchived(episode: EpisodeView, archived: boolean) {
  setLocal(episode.id, { ...local[episode.id], archived });
  write({ type: "episode.archived", episode_id: episode.id, archived });
}

/** `lazy` while playing: positions then wait for a later request (spec 5.2, step 3). */
export function setPosition(episode: Pick<EpisodeView, "id">, positionMs: number, options?: { lazy?: boolean }) {
  setLocal(episode.id, { ...local[episode.id], positionMs });
  write({ type: "episode.position", episode_id: episode.id, position_ms: Math.round(positionMs) }, options);
}

/**
 * Shows state the server already holds, without writing it: a Sonos session's
 * position, which pod-server writes itself (spec 13.3).
 */
export function adoptServerState(episodeId: string, state: Local) {
  setLocal(episodeId, { ...local[episodeId], ...state });
}

/** How many pages of the show's episodes `refetchState` reads before giving up. */
const REFETCH_PAGES = 4;

/**
 * Re-reads one episode's state from the server and shows it, for when
 * pod-server wrote it last (a Sonos session that ended). There is no endpoint
 * for one episode, so this walks the show's episode list, newest first, where
 * an episode just played usually sits on the first page. Returns the state,
 * or null when the episode wasn't found.
 */
export async function refetchState(episode: Pick<EpisodeView, "id" | "podcastId">): Promise<Required<Pick<Local, "positionMs" | "played">> | null> {
  let cursor: string | null = null;
  for (let page = 0; page < REFETCH_PAGES; page++) {
    const query: string = cursor ? `&cursor=${encodeURIComponent(cursor)}` : "";
    const result: Page<Episode> = await api<Page<Episode>>("GET", `/v1/podcasts/${episode.podcastId}/episodes?limit=50${query}`);
    const found = result.items.find((item) => item.id === episode.id);
    if (found) {
      const state = { positionMs: found.episode_state?.position_ms ?? 0, played: found.episode_state?.played ?? false };
      adoptServerState(episode.id, state);
      return state;
    }
    cursor = result.next_cursor ?? null;
    if (!cursor) break;
  }
  return null;
}
