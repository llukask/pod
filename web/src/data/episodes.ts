import { createStore } from "solid-js/store";
import type { components } from "../api/schema";
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
  /** From the feed's tags (spec 15.1), for the detail's meta line. */
  season: number | null;
  number: number | null;
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

type PlaybackEpisode = components["schemas"]["PlaybackEpisode"];

type Shared = Pick<InboxItem, "id" | "podcast_id" | "title" | "published_at" | "duration_ms" | "audio_url" | "artwork_url" | "content_html" | "summary_html" | "link" | "season" | "number">;

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
    season: episode.season ?? null,
    number: episode.number ?? null,
  };
}

/** An inbox item is unplayed and not archived by definition (spec 6). */
export function fromInbox(item: InboxItem): EpisodeView {
  return seen({ ...base(item, item.podcast_title ?? "", item.podcast_artwork_url), positionMs: item.position_ms, played: false, archived: false });
}

export function fromPodcast(episode: Episode, podcast: Pick<Podcast, "title" | "artwork_url" | "feed_url">): EpisodeView {
  const state = episode.episode_state;
  return seen({
    ...base(episode, podcast.title ?? podcast.feed_url, podcast.artwork_url),
    positionMs: state?.position_ms ?? 0,
    played: state?.played ?? false,
    archived: state?.archived ?? false,
  });
}

/**
 * An episode another device plays, from the metadata Connect sends along
 * (spec 14.1), for when this tab hasn't listed it. It has no audio URL, so it
 * can be shown but not played: `findEpisode` fetches the rest first.
 */
export function fromPlayback(episode: PlaybackEpisode): EpisodeView {
  return {
    id: episode.id,
    podcastId: episode.podcast_id,
    podcastTitle: episode.podcast_title ?? "",
    title: episode.title,
    publishedAt: "",
    durationMs: episode.duration_ms ?? null,
    audioUrl: "",
    artworkUrl: episode.artwork_url ?? null,
    notesHtml: "",
    link: null,
    season: null,
    number: null,
    positionMs: 0,
    played: false,
    archived: false,
  };
}

/** Whether `episode` can go to the <audio>, which one made by `fromPlayback` can't. */
export const playable = (episode: EpisodeView) => episode.audioUrl !== "";

// ==============================================================================
// Episodes by id
// ==============================================================================
//
// There is no endpoint for one episode, and the web client keeps no local copy
// of the library. A command from another device names an episode by id only,
// so the tab remembers every episode its lists showed, and fetches one it
// hasn't seen from its show's list, or from the inbox when the show is unknown.

const known = new Map<string, EpisodeView>();

function seen(episode: EpisodeView): EpisodeView {
  known.set(episode.id, episode);
  return episode;
}

/** The episode as this tab's lists last showed it, if they did. */
export function knownEpisode(id: string): EpisodeView | null {
  return known.get(id) ?? null;
}

/**
 * Finds an episode by id: in the lists this tab showed, else in its show's
 * episode list when the show is known, else on the first page of the inbox.
 * Null when none of them has it.
 */
export async function findEpisode(id: string, show: Pick<PlaybackEpisode, "podcast_id" | "podcast_title" | "artwork_url"> | null): Promise<EpisodeView | null> {
  const listed = knownEpisode(id);
  if (listed) return listed;
  if (show) {
    const found = await findInShow(show.podcast_id, id);
    return found && fromPodcast(found, { title: show.podcast_title, artwork_url: show.artwork_url, feed_url: "" });
  }
  const inbox = await api<Page<InboxItem>>("GET", "/v1/inbox?limit=50");
  const item = inbox.items.find((candidate) => candidate.id === id);
  return item ? fromInbox(item) : null;
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
 * Shows state the server already holds, without writing it: the position of
 * an episode another device plays, which that device writes itself (spec 14).
 */
export function adoptServerState(episodeId: string, state: Local) {
  setLocal(episodeId, { ...local[episodeId], ...state });
}

/** How many pages of the show's episodes `refetchState` reads before giving up. */
const REFETCH_PAGES = 4;

/** The position this tab wrote or adopted for the episode, if any: the local state, which beats a list loaded earlier. */
export function localPosition(episodeId: string): number | null {
  return local[episodeId]?.positionMs ?? null;
}

/**
 * Re-reads one episode's state from the server and shows it, for when another
 * device wrote it last (a remote playback that ended, spec 14.7). Returns the
 * state, or null when the episode wasn't found.
 */
export async function refetchState(episode: Pick<EpisodeView, "id" | "podcastId">): Promise<Required<Pick<Local, "positionMs" | "played">> | null> {
  const found = await findInShow(episode.podcastId, episode.id);
  if (!found) return null;
  const state = { positionMs: found.episode_state?.position_ms ?? 0, played: found.episode_state?.played ?? false };
  adoptServerState(episode.id, state);
  return state;
}

/**
 * Walks the show's episode list, newest first, where an episode just played
 * usually sits on the first page.
 */
async function findInShow(podcastId: string, episodeId: string): Promise<Episode | null> {
  let cursor: string | null = null;
  for (let page = 0; page < REFETCH_PAGES; page++) {
    const query: string = cursor ? `&cursor=${encodeURIComponent(cursor)}` : "";
    const result: Page<Episode> = await api<Page<Episode>>("GET", `/v1/podcasts/${podcastId}/episodes?limit=50${query}`);
    const found = result.items.find((item) => item.id === episodeId);
    if (found) return found;
    cursor = result.next_cursor ?? null;
    if (!cursor) break;
  }
  return null;
}
