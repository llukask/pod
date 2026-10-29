import { useNavigate, useSearchParams } from "@solidjs/router";
import { For, Match, Show, Switch, createMemo, createResource, createSignal, onCleanup, onMount } from "solid-js";
import type { DirectoryResult } from "../api/types";
import { resultIsSubscribed, isSubscribed, subscribe } from "../data/subscriptions";
import { type FeedUrlError, classify } from "../feedUrl";
import { api } from "../session";
import { Artwork } from "./Artwork";

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
      <Switch>
        <Match when={query().kind === "empty"}>
          <h1>Add a podcast</h1>
          <p class="muted">Search the directory by name, or paste a feed address.</p>
        </Match>
        <Match when={feedError()}>
          {(error) => (
            <>
              <h1>Add by feed address</h1>
              <p class="error">{FEED_URL_ERRORS[error()]}</p>
            </>
          )}
        </Match>
        <Match when={feed()}>
          {(url) => (
            <>
              <h1>Add by feed address</h1>
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
              <h1>Results for “{q()}”</h1>
              <p class="muted from">From the Apple Podcasts directory</p>
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
      <Artwork url={props.artworkUrl} size={56} />
      <span class="episode-text">
        <span class="title">{props.title}</span>
        <span class="meta">{props.subtitle}</span>
      </span>
      <Show
        when={!props.subscribed}
        fallback={
          <button class="button" onClick={() => navigate(`/podcasts/${props.podcastId}`)}>
            ✓ Subscribed
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
          Subscribe
        </button>
      </Show>
    </li>
  );
}

/** The sidebar's field (D3): Enter searches, Ctrl+K (⌘K) focuses it from anywhere. */
export function SearchField() {
  const navigate = useNavigate();
  const [params] = useSearchParams<{ q?: string }>();
  let input!: HTMLInputElement;
  const onKey = (event: KeyboardEvent) => {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      input.focus();
      input.select();
    }
  };
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));
  return (
    <form
      class="search-field"
      role="search"
      onSubmit={(event) => {
        event.preventDefault();
        navigate(`/search?q=${encodeURIComponent(input.value.trim())}`);
      }}
    >
      <input ref={input} type="search" placeholder="Search or paste feed URL" aria-label="Search or paste feed URL" value={params.q ?? ""} />
    </form>
  );
}
