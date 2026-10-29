import { A } from "@solidjs/router";
import { For, type ParentProps, Show } from "solid-js";
import { podcasts } from "../data/podcasts";
import { Artwork } from "./Artwork";
import { PlayerBar } from "./PlayerBar";

// Wireframe D1: the sidebar beside the content pane. The player bar joins
// along the bottom.
export function Shell(props: ParentProps) {
  return (
    <div class="shell">
      <nav class="sidebar" aria-label="Main">
        <div class="brand">Pod</div>
        <A class="nav-item" href="/inbox" activeClass="active">
          <span class="label">Inbox</span>
        </A>
        <A class="nav-item narrow-only" href="/podcasts" activeClass="active" end>
          <span class="label">Podcasts</span>
        </A>
        <A class="nav-item" href="/settings" activeClass="active">
          <span class="label">Settings</span>
        </A>
        <div class="heading">Podcasts</div>
        <Show when={podcasts()?.length === 0}>
          <p class="nav-item show muted">No podcasts yet</p>
        </Show>
        <For each={podcasts()}>
          {(podcast) => (
            <A class="nav-item show" href={`/podcasts/${podcast.id}`} activeClass="active">
              <Artwork url={podcast.artwork_url} size={24} />
              <span class="label">{podcast.title ?? podcast.feed_url}</span>
            </A>
          )}
        </For>
      </nav>
      <main class="content">{props.children}</main>
      <PlayerBar />
    </div>
  );
}
