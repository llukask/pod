import { A } from "@solidjs/router";
import { For } from "solid-js";
import { podcasts } from "../data/podcasts";
import { Artwork } from "./Artwork";
import { Icon } from "./Icon";

// Android M3 as a page: the subscribed shows as a grid of covers, ending in
// a tile that adds another. On wide screens the sidebar lists them too.
export function Podcasts() {
  return (
    <div class="page">
      <header class="page-header">
        <h1>Podcasts</h1>
      </header>
      <div class="podcast-grid">
        <For each={podcasts()}>
          {(podcast) => (
            <A class="podcast-tile" href={`/podcasts/${podcast.id}`}>
              <Artwork url={podcast.artwork_url} size={180} title={podcast.title} />
              <span class="tile-title">{podcast.title ?? podcast.feed_url}</span>
              <span class="tile-meta">{podcast.status === "pending" ? "Fetching the feed…" : podcast.author}</span>
            </A>
          )}
        </For>
        <A class="podcast-tile add-tile" href="/search">
          <span class="add-art">
            <Icon name="plus" size={32} />
          </span>
          <span class="tile-title">Add a podcast</span>
          <span class="tile-meta">Search the directory or paste a feed</span>
        </A>
      </div>
    </div>
  );
}
