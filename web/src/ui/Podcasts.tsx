import { A } from "@solidjs/router";
import { For, Show } from "solid-js";
import { podcasts } from "../data/podcasts";
import { Artwork } from "./Artwork";

// Android M3 as a page: the subscribed shows as a grid of covers. On wide
// screens the sidebar lists them too.
export function Podcasts() {
  return (
    <div class="page">
      <h1>Podcasts</h1>
      <Show when={podcasts()?.length === 0}>
        <p class="muted">You haven't subscribed to anything yet.</p>
      </Show>
      <div class="podcast-grid">
        <For each={podcasts()}>
          {(podcast) => (
            <A class="podcast-tile" href={`/podcasts/${podcast.id}`}>
              <Artwork url={podcast.artwork_url} size={160} />
              <span>{podcast.title ?? podcast.feed_url}</span>
            </A>
          )}
        </For>
      </div>
    </div>
  );
}
