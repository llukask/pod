import { createEffect, createRoot, on } from "solid-js";
import type { DirectoryResult, Podcast } from "../api/types";
import { normalizeFeedUrl } from "../feedUrl";
import { notify } from "../ui/Toasts";
import { podcasts, refetchPodcasts, setPodcasts } from "./podcasts";
import { flushWrites, outbox } from "./writes";

// Subscribing (spec 5.5, as the web client does it). The show joins the
// sidebar at once as a pending placeholder, the write goes out without the
// usual debounce, and the server's list replaces the placeholder once it has
// the subscription. A subscribe the server rejects (`invalid_feed_url`) takes
// the placeholder away again and says why.

export function isSubscribed(podcastId: string): boolean {
  return podcasts()?.some((p) => p.id === podcastId) ?? false;
}

/**
 * Whether a directory result is a show the user follows: by the server's
 * canonical id, which also covers an old address of a followed show (spec
 * 4.6), or by the id of the result's own URL, a placeholder from subscribing to
 * it that the server hasn't redirected yet.
 */
export function resultIsSubscribed(result: DirectoryResult): boolean {
  if (isSubscribed(result.podcast_id)) return true;
  const url = normalizeFeedUrl(result.feed_url);
  return url.ok && isSubscribed(url.podcastId);
}

export async function subscribe(show: { podcastId: string; feedUrl: string; title?: string | null; artworkUrl?: string | null }): Promise<boolean> {
  if (!isSubscribed(show.podcastId)) {
    const placeholder: Podcast = {
      id: show.podcastId,
      feed_url: show.feedUrl,
      status: "pending",
      title: show.title ?? null,
      artwork_url: show.artworkUrl ?? null,
      version: 0,
      latest_published_at: null,
    };
    // A fresh subscription sorts first, as the server's list does (spec 4.5).
    setPodcasts((list) => [placeholder, ...(list ?? [])]);
  }
  outbox()?.write({ type: "subscription.set", podcast_id: show.podcastId, feed_url: show.feedUrl, subscribed: true });
  await flushWrites();
  await refetchPodcasts();
  // False when the server rejected it; the rejection listener has said why.
  return isSubscribed(show.podcastId);
}

createRoot(() => {
  createEffect(
    on(outbox, (box) =>
      box?.onRejected((mutation, code) => {
        if (mutation.type !== "subscription.set" || !mutation.subscribed) return;
        setPodcasts((list) => list?.filter((p) => p.id !== mutation.podcast_id));
        notify(`Couldn't add ${host(mutation.feed_url)}: ${code === "invalid_feed_url" ? "address not allowed" : "the server refused it"}`);
      }),
    ),
  );
});

/** The feed's host for messages; a feed URL may hold credentials, which never show (spec 3.2). */
function host(feedUrl: string): string {
  try {
    return new URL(feedUrl).host;
  } catch {
    return "the feed";
  }
}
