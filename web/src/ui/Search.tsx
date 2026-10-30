import { useNavigate, useSearchParams } from "@solidjs/router";
import { For, Match, Show, Switch, createMemo, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { DirectoryResult } from "../api/types";
import { resultIsSubscribed, isSubscribed, subscribe } from "../data/subscriptions";
import { type FeedUrlError, classify } from "../feedUrl";
import { api } from "../session";
import { Artwork } from "./Artwork";
import { Icon } from "./Icon";

// Wireframe D3: directory results for typed terms (`GET /v1/directory/search`,
// spec 4.6), or one "Subscribe to this feed" row for a pasted feed URL, which
// is checked here before anything is sent (Android M4).

const FEED_URL_ERRORS: Record<FeedUrlError, string> = {
  invalid_url: "That isn't a web address.",
  unsupported_scheme: "Feed addresses start with http:// or https://.",
};

export function Search() {
  const [params] = useSearchParams<{ q?: string }>();
  const query = createMemo(() => classify(params.q ?? ""));
  const terms = () => {
    const q = query();
    return q.kind === "terms" ? q.terms : undefined;
  };
  const feed = () => {
    const q = query();
    return q.kind === "feed" && q.url.ok ? q.url : undefined;
  };
  const feedError = () => {
    const q = query();
    return q.kind === "feed" && !q.url.ok ? q.url.error : undefined;
  };
  const [results] = createResource(terms, async (q) =>
    (await api<{ results: DirectoryResult[] }>("GET", `/v1/directory/search?q=${encodeURIComponent(q)}&limit=25`)).results,
  );

  return (
    <div class="page search">
      {/* Phones have no sidebar, so the page brings its own field. */}
      <SearchField class="page-search" autofocus={query().kind === "empty"} />
      <Switch>
        <Match when={query().kind === "empty"}>
          <header class="page-header">
            <h1>Add a podcast</h1>
          </header>
          <p class="muted lead">Search the directory by name, or paste a feed address.</p>
        </Match>
        <Match when={feedError()}>
          {(error) => (
            <>
              <header class="page-header">
                <h1>Add by feed address</h1>
              </header>
              <p class="error">{FEED_URL_ERRORS[error()]}</p>
            </>
          )}
        </Match>
        <Match when={feed()}>
          {(url) => (
            <>
              <header class="page-header">
                <h1>Add by feed address</h1>
              </header>
              <ul class="result-list">
                <ResultRow
                  title="Subscribe to this feed"
                  subtitle={url().feedUrl}
                  podcastId={url().podcastId}
                  subscribed={isSubscribed(url().podcastId)}
                  onSubscribe={() => subscribe({ podcastId: url().podcastId, feedUrl: url().feedUrl })}
                />
              </ul>
            </>
          )}
        </Match>
        <Match when={terms()}>
          {(q) => (
            <>
              <header class="page-header">
                <h1>Results for “{q()}”</h1>
                <p class="muted">From the Apple Podcasts directory</p>
              </header>
              <Switch>
                <Match when={results.error}>
                  <p class="error">Search isn't available right now. You can still add a show by pasting its feed address.</p>
                </Match>
                <Match when={results.loading}>
                  <p class="muted">Searching…</p>
                </Match>
                <Match when={results()?.length === 0}>
                  <p class="muted">Nothing found. Try other words, or paste the show's feed address.</p>
                </Match>
              </Switch>
              <ul class="result-list">
                <For each={results.state === "ready" ? results() : []}>
                  {(result) => (
                    <ResultRow
                      title={result.title}
                      subtitle={result.author ?? ""}
                      artworkUrl={result.artwork_url}
                      podcastId={result.podcast_id}
                      subscribed={resultIsSubscribed(result)}
                      onSubscribe={() =>
                        subscribe({ podcastId: result.podcast_id, feedUrl: result.feed_url, title: result.title, artworkUrl: result.artwork_url })
                      }
                    />
                  )}
                </For>
              </ul>
            </>
          )}
        </Match>
      </Switch>
    </div>
  );
}

function ResultRow(props: {
  title: string;
  subtitle: string;
  artworkUrl?: string | null;
  podcastId: string;
  subscribed: boolean;
  /** Resolves to whether the show is subscribed afterwards. */
  onSubscribe: () => Promise<boolean>;
}) {
  const navigate = useNavigate();
  const [busy, setBusy] = createSignal(false);
  return (
    <li class="result">
      <Artwork url={props.artworkUrl} size={64} title={props.title} />
      <span class="episode-text">
        <span class="title">{props.title}</span>
        <span class="meta">{props.subtitle}</span>
      </span>
      <Show
        when={!props.subscribed}
        fallback={
          <button class="button" onClick={() => navigate(`/podcasts/${props.podcastId}`)}>
            <Icon name="check" size={18} />
            Subscribed
          </button>
        }
      >
        <button
          class="button primary"
          disabled={busy()}
          onClick={async () => {
            setBusy(true);
            // The page opens once the server has the subscription, so its first fetch finds the show.
            const subscribed = await props.onSubscribe().finally(() => setBusy(false));
            if (subscribed) navigate(`/podcasts/${props.podcastId}`);
          }}
        >
          <Icon name="plus" size={18} />
          {busy() ? "Subscribing…" : "Subscribe"}
        </button>
      </Show>
    </li>
  );
}

/**
 * The search field (D3): Enter searches. The sidebar's copy also takes Ctrl+K
 * (⌘K) from anywhere; the search page's copy is for phones, which hide the
 * sidebar.
 */
export function SearchField(props: { class?: string; autofocus?: boolean }) {
  const navigate = useNavigate();
  const [params] = useSearchParams<{ q?: string }>();
  let input!: HTMLInputElement;
  const onKey = (event: KeyboardEvent) => {
    if (props.class) return;
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      input.focus();
      input.select();
    }
  };
  onMount(() => {
    window.addEventListener("keydown", onKey);
    // Only where the field shows: focusing a hidden one would do nothing useful.
    if (props.autofocus && input.offsetParent) input.focus();
  });
  onCleanup(() => window.removeEventListener("keydown", onKey));
  return (
    <form
      class={`search-field ${props.class ?? ""}`}
      role="search"
      onSubmit={(event) => {
        event.preventDefault();
        navigate(`/search?q=${encodeURIComponent(input.value.trim())}`);
      }}
    >
      <Icon name="search" size={18} />
      <input
        ref={input}
        type="search"
        enterkeyhint="search"
        placeholder="Search or paste URL"
        aria-label="Search or paste a feed URL"
        value={params.q ?? ""}
      />
      <Show when={!props.class}>
        <kbd class="shortcut">Ctrl K</kbd>
      </Show>
    </form>
  );
}
