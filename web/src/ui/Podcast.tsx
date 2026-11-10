import { useNavigate, useParams } from "@solidjs/router";
import { For, Show, createEffect, createMemo, createResource, createSignal, on, onCleanup, onMount } from "solid-js";
import type { Episode, Page, Podcast as PodcastItem } from "../api/types";
import { type EpisodeView, current, fromPodcast } from "../data/episodes";
import { createPages } from "../data/pages";
import { podcasts, refetchPodcasts, setPodcasts } from "../data/podcasts";
import { write } from "../data/writes";
import { statusNotice } from "../podcastStatus";
import { plainText } from "../showNotes";
import { api } from "../session";
import { Ambient, Artwork } from "./Artwork";
import { Confirm } from "./Confirm";
import { EpisodeDetail } from "./EpisodeDetail";
import { EpisodeRow } from "./EpisodeRow";
import { Icon } from "./Icon";

// Wireframe D2: one show's header and its episodes, all of them or one season
// (spec 15), newest or oldest first (spec 16.2), beside the selected episode.
// The show comes from the sidebar's list; a pending one (subscribed, not
// fetched yet) is asked for again every few seconds until the server has its
// feed.
//
// TODO: Mark all played (D2, M5). It needs every unplayed episode, which here
// means paging through the whole back catalogue first.

const PENDING_POLL_MS = 5_000;

type Filter = "all" | "unplayed";
type Order = "newest" | "oldest";

export function Podcast(props: { id: string }) {
  const params = useParams<{ episodeId?: string }>();
  const navigate = useNavigate();
  const [filter, setFilter] = createSignal<Filter>("all");
  // The chosen season, or null for all of them. It isn't remembered (spec 15.3).
  const [season, setSeason] = createSignal<number | null>(null);
  const [menu, setMenu] = createSignal(false);
  const [confirming, setConfirming] = createSignal(false);
  const [aboutOpen, setAboutOpen] = createSignal(false);
  // The description shows two lines; the link to the rest appears only when there is more.
  const [clamped, setClamped] = createSignal(false);
  const watchClamp = (description: HTMLElement) => {
    const observer = new ResizeObserver(() => setClamped(description.scrollHeight > description.clientHeight + 1));
    observer.observe(description);
    onCleanup(() => observer.disconnect());
  };

  // The sidebar's copy, replaced by a fresh fetch while the feed is pending.
  const [fresh, { refetch: refetchShow }] = createResource(() => api<PodcastItem>("GET", `/v1/podcasts/${props.id}`));
  // Reading a failed resource throws, so the fresh copy counts only once it has loaded.
  const podcast = () => (fresh.state === "ready" ? fresh() : undefined) ?? podcasts()?.find((p) => p.id === props.id);

  // Until the user flips it, a serial show and a chosen season start from
  // their first episode, anything else from the newest (spec 16.2). A flip
  // holds for the rest of the visit, across season changes.
  const [flipped, setFlipped] = createSignal<Order | null>(null);
  // A memo, so a refetched show that is still serial reloads nothing.
  const order = createMemo((): Order => flipped() ?? (podcast()?.show_type === "serial" || season() !== null ? "oldest" : "newest"));

  const pages = createPages((cursor) => {
    const chosen = season();
    const query = `limit=50&order=${order()}${chosen === null ? "" : `&season=${chosen}`}${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`;
    return api<Page<Episode>>("GET", `/v1/podcasts/${props.id}/episodes?${query}`);
  });
  const seasons = () => podcast()?.seasons ?? [];
  onMount(() => void pages.reload());
  // A new season or order lists from the start. So does the show type
  // arriving after the first page asked for the wrong order.
  createEffect(on([season, order], () => void pages.reload(), { defer: true }));

  const poll = setInterval(async () => {
    if (podcast()?.status !== "pending") return;
    await refetchShow();
    if (podcast()?.status !== "pending") {
      void pages.reload();
      void refetchPodcasts();
    }
  }, PENDING_POLL_MS);
  onCleanup(() => clearInterval(poll));

  const episodes = createMemo(() => {
    const show = podcast();
    return show ? pages.items().map((episode) => fromPodcast(episode, show)) : [];
  });
  const visible = () =>
    episodes().filter((episode) => filter() === "all" || episode.id === params.episodeId || !current(episode).played);
  const selected = (): EpisodeView | undefined => episodes().find((episode) => episode.id === params.episodeId);

  function unsubscribe(show: PodcastItem) {
    write({ type: "subscription.set", podcast_id: show.id, feed_url: show.feed_url, subscribed: false });
    setPodcasts((list) => list?.filter((p) => p.id !== show.id));
    navigate("/inbox");
  }

  return (
    <Show
      when={podcast()}
      fallback={
        <Show when={fresh.state === "errored"}>
          <div class="page empty">
            <p class="empty-title">This podcast isn't in your subscriptions</p>
            <p class="muted">It may have been removed on another device.</p>
          </div>
        </Show>
      }
    >
      {(show) => (
        <div class="split" classList={{ open: !!selected() }}>
          <section class="list-pane">
            <Ambient url={show().artwork_url} />
            {/* Wide panes put the header beside the episodes as a sticky column (styles.css). */}
            <div class="podcast-page">
              <header class="podcast-header">
                <Artwork url={show().artwork_url} size={160} title={show().title} />
                <div class="podcast-text">
                  <h1>{show().title ?? show().feed_url}</h1>
                  <Show when={show().author}>{(author) => <p class="author">{author()}</p>}</Show>
                  <div class="podcast-actions">
                    <span class="subscribed">
                      <Icon name="check" size={16} />
                      Subscribed
                    </span>
                    <div class="menu-anchor" onFocusOut={(e) => !e.currentTarget.contains(e.relatedTarget as Node | null) && setMenu(false)}>
                      <button class="icon-button" title="More" aria-haspopup="menu" aria-expanded={menu()} onClick={() => setMenu(!menu())}>
                        <Icon name="more" />
                      </button>
                      <Show when={menu()}>
                        <div class="menu" role="menu">
                          <button role="menuitem" onClick={() => (setMenu(false), setConfirming(true))}>
                            Unsubscribe
                          </button>
                        </div>
                      </Show>
                    </div>
                  </div>
                </div>
                <Show when={show().description}>
                  {(text) => (
                    <p class="description" classList={{ expanded: aboutOpen() }} ref={watchClamp}>
                      {plainText(text())}
                    </p>
                  )}
                </Show>
                <Show when={clamped() && !aboutOpen()}>
                  <button class="link-button more-about" onClick={() => setAboutOpen(true)}>
                    More about this show
                  </button>
                </Show>
              </header>
              <div class="episodes">
                <Show when={statusNotice(show())}>{(notice) => <p class="banner">{notice()}</p>}</Show>
                <div class="list-controls">
                  <div class="filters" role="group" aria-label="Show">
                    <For each={["all", "unplayed"] as Filter[]}>
                      {(value) => (
                        <button class="chip" classList={{ active: filter() === value }} aria-pressed={filter() === value} onClick={() => setFilter(value)}>
                          {value === "all" ? "All" : "Unplayed"}
                        </button>
                      )}
                    </For>
                  </div>
                  {/* One season is nothing to choose from (spec 15.3). */}
                  <Show when={seasons().length >= 2}>
                    <label class="season-picker" classList={{ active: season() !== null }}>
                      <select
                        aria-label="Season"
                        value={season() ?? ""}
                        onChange={(event) => setSeason(event.currentTarget.value === "" ? null : Number(event.currentTarget.value))}
                      >
                        <option value="">All seasons</option>
                        <For each={seasons()}>{(value) => <option value={value}>Season {value}</option>}</For>
                      </select>
                      <Icon name="chevronDown" size={16} />
                    </label>
                  </Show>
                  <button
                    class="order-toggle"
                    title={order() === "newest" ? "Show oldest first" : "Show newest first"}
                    onClick={() => setFlipped(order() === "newest" ? "oldest" : "newest")}
                  >
                    <Icon name="sort" size={16} />
                    {order() === "newest" ? "Newest first" : "Oldest first"}
                  </button>
                </div>
                <Show when={pages.error()}>{(error) => <p class="error notice">{error().message}</p>}</Show>
                <Show when={!pages.loading() && !pages.error() && visible().length === 0}>
                  <div class="empty">
                    <p class="muted">
                      {show().status === "pending"
                        ? "Episodes show up once the server has fetched the feed."
                        : filter() === "unplayed" && pages.items().length > 0
                          ? "You've played every episode shown."
                          : season() !== null
                            ? "This season has no episodes."
                            : "This feed has no episodes."}
                    </p>
                  </div>
                </Show>
                <ul class="episode-list">
                  <For each={visible()}>
                    {(episode) => <EpisodeRow episode={episode} href={`/podcasts/${show().id}/${episode.id}`} showPodcast={false} />}
                  </For>
                </ul>
                <Show when={!pages.done() && pages.items().length > 0}>
                  <button class="button more" onClick={() => pages.more()} disabled={pages.loading()}>
                    {pages.loading() ? "Loading…" : "Show more"}
                  </button>
                </Show>
              </div>
            </div>
          </section>
          <Show when={selected()}>
            {(episode) => (
              <section class="detail-pane">
                <EpisodeDetail episode={episode()} onClose={() => navigate(`/podcasts/${show().id}`)} backLabel={show().title ?? "Podcast"} />
              </section>
            )}
          </Show>
          <Show when={confirming()}>
            <Confirm
              title={`Unsubscribe from ${show().title ?? "this podcast"}?`}
              confirm="Unsubscribe"
              onConfirm={() => unsubscribe(show())}
              onCancel={() => setConfirming(false)}
            >
              Your listening history stays.
            </Confirm>
          </Show>
        </div>
      )}
    </Show>
  );
}

/** The route: a fresh page per show, so lists and polls never mix two shows. */
export function PodcastRoute() {
  const params = useParams<{ podcastId: string }>();
  return <Show when={params.podcastId} keyed>{(id) => <Podcast id={id} />}</Show>;
}
