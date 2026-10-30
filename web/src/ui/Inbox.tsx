import { A, useNavigate, useParams } from "@solidjs/router";
import { For, Index, Show, createMemo, onMount } from "solid-js";
import type { InboxItem, Page } from "../api/types";
import { type EpisodeView, current, fromInbox } from "../data/episodes";
import { createPages } from "../data/pages";
import { dayGroup } from "../format";
import { api } from "../session";
import { EpisodeDetail } from "./EpisodeDetail";
import { EpisodeRow } from "./EpisodeRow";
import { Icon } from "./Icon";

// Wireframe D1: the inbox (spec 6, served by `GET /v1/inbox`) beside the
// selected episode. An episode marked played or archived here leaves the list
// at once, except the one open in the detail pane, so the buttons there don't
// pull the page out from under the reader.
//
// TODO: an undo for Mark played and Archive, and loading the next page on
// scroll instead of the Show more button.
export function Inbox() {
  const params = useParams<{ episodeId?: string }>();
  const navigate = useNavigate();
  const pages = createPages((cursor) =>
    api<Page<InboxItem>>("GET", `/v1/inbox?limit=50${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`),
  );
  onMount(() => void pages.reload());

  const episodes = createMemo(() => pages.items().map(fromInbox));
  const visible = () =>
    episodes().filter((episode) => {
      const now = current(episode);
      return episode.id === params.episodeId || (!now.played && !now.archived);
    });
  const selected = (): EpisodeView | undefined => episodes().find((episode) => episode.id === params.episodeId);
  // The list is newest first, so each day group is one run of rows.
  const groups = createMemo(() => {
    const runs: { label: string; episodes: EpisodeView[] }[] = [];
    for (const episode of visible()) {
      const label = dayGroup(episode.publishedAt);
      if (runs.at(-1)?.label === label) runs.at(-1)!.episodes.push(episode);
      else runs.push({ label, episodes: [episode] });
    }
    return runs;
  });

  return (
    <div class="split" classList={{ open: !!selected() }}>
      <section class="list-pane">
        <header class="pane-header">
          <h1>Inbox</h1>
        </header>
        <Show when={pages.error()}>{(error) => <p class="error notice">{error().message}</p>}</Show>
        <Show when={!pages.loading() && !pages.error() && visible().length === 0}>
          <div class="empty">
            <Icon name="inbox" size={40} />
            <p class="empty-title">You're all caught up</p>
            <p class="muted">New episodes from your podcasts land here.</p>
            <A class="button" href="/search">
              <Icon name="plus" size={18} />
              Add a podcast
            </A>
          </div>
        </Show>
        {/* Index, not For: the groups are rebuilt on every change, and keying them by position keeps the rows inside. */}
        <Index each={groups()}>
          {(group) => (
            <section class="day-group">
              <h2 class="group-heading">{group().label}</h2>
              <ul class="episode-list">
                <For each={group().episodes}>
                  {/* Under "Today" and "Yesterday" the heading already says when. */}
                  {(episode) => (
                    <EpisodeRow
                      episode={episode}
                      href={`/inbox/${episode.id}`}
                      showPodcast
                      showDate={group().label !== "Today" && group().label !== "Yesterday"}
                    />
                  )}
                </For>
              </ul>
            </section>
          )}
        </Index>
        <Show when={!pages.done() && pages.items().length > 0}>
          <button class="button more" onClick={() => pages.more()} disabled={pages.loading()}>
            {pages.loading() ? "Loading…" : "Show more"}
          </button>
        </Show>
      </section>
      <Show when={selected()}>
        {(episode) => (
          <section class="detail-pane">
            <EpisodeDetail episode={episode()} onClose={() => navigate("/inbox")} backLabel="Inbox" />
          </section>
        )}
      </Show>
    </div>
  );
}
