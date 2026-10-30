import { A } from "@solidjs/router";
import { For, type ParentProps, Show } from "solid-js";
import { podcasts } from "../data/podcasts";
import { episode } from "../player/player";
import { Artwork } from "./Artwork";
import { Icon } from "./Icon";
import { PlayerBar } from "./PlayerBar";
import { SearchField } from "./Search";
import { Toasts } from "./Toasts";

// Wireframe D1: the sidebar beside the content pane, the player along the
// bottom. Phones swap the sidebar for a tab bar under the content (the
// Podcasts grid stands in for the sidebar's show list) and the player
// shrinks to a mini player above it.
export function Shell(props: ParentProps) {
  return (
    <div class="shell" classList={{ "has-player": !!episode() }}>
      <nav class="sidebar" aria-label="Main">
        <A class="brand" href="/inbox">
          <Logo />
          Pod
        </A>
        <SearchField />
        <A class="nav-item" href="/inbox" activeClass="active">
          <Icon name="inbox" />
          <span class="label">Inbox</span>
        </A>
        <A class="nav-item" href="/settings" activeClass="active">
          <Icon name="settings" />
          <span class="label">Settings</span>
        </A>
        <A class="heading" href="/podcasts" activeClass="active" end>
          Podcasts
        </A>
        <Show when={podcasts()?.length === 0}>
          <A class="nav-item muted" href="/search">
            <Icon name="plus" />
            <span class="label">Add a podcast</span>
          </A>
        </Show>
        <For each={podcasts()}>
          {(podcast) => (
            <A class="nav-item show" href={`/podcasts/${podcast.id}`} activeClass="active">
              <Artwork url={podcast.artwork_url} size={28} title={podcast.title} />
              <span class="label">{podcast.title ?? podcast.feed_url}</span>
            </A>
          )}
        </For>
      </nav>
      <main class="content">{props.children}</main>
      <PlayerBar />
      <nav class="tab-bar" aria-label="Main">
        <A class="tab" href="/inbox" activeClass="active">
          <Icon name="inbox" size={24} />
          Inbox
        </A>
        <A class="tab" href="/podcasts" activeClass="active">
          <Icon name="shows" size={24} />
          Podcasts
        </A>
        <A class="tab" href="/search" activeClass="active">
          <Icon name="search" size={24} />
          Search
        </A>
        <A class="tab" href="/settings" activeClass="active">
          <Icon name="settings" size={24} />
          Settings
        </A>
      </nav>
      <Toasts />
    </div>
  );
}

/** The favicon's mark, a pea pod, in the theme's accent. */
export function Logo() {
  return (
    <svg class="logo" viewBox="0 0 32 32" width="26" height="26" aria-hidden="true">
      <rect width="32" height="32" rx="8" fill="var(--accent)" />
      <path d="M5.5 26.5C6 15 13 6.5 26.5 5.5C26 17 18 25.5 5.5 26.5Z" fill="var(--on-accent)" />
      <circle cx="11.4" cy="20.6" r="2.7" fill="var(--accent)" />
      <circle cx="16" cy="16" r="2.9" fill="var(--accent)" />
      <circle cx="20.6" cy="11.4" r="2.7" fill="var(--accent)" />
    </svg>
  );
}
