import { A } from "@solidjs/router";
import { Show } from "solid-js";
import { type EpisodeView, current, setArchived, setPlayed } from "../data/episodes";
import { dateLabel, durationLabel, leftLabel } from "../format";
import { Artwork } from "./Artwork";

/** One episode in a list (D1 inbox rows, D2 podcast rows): where it's from, when, how long, how far. */
export function EpisodeRow(props: { episode: EpisodeView; href: string; showPodcast: boolean }) {
  const episode = () => current(props.episode);
  const progress = () => {
    const { durationMs, positionMs, played } = episode();
    if (played) return "Played";
    if (positionMs > 0 && durationMs) return leftLabel(durationMs, positionMs);
    return null;
  };
  return (
    <li class="episode-row" classList={{ played: episode().played }}>
      <A class="episode-link" href={props.href} activeClass="selected">
        <Show when={props.showPodcast}>
          <Artwork url={episode().artworkUrl} size={48} />
        </Show>
        <span class="episode-text">
          <span class="meta">
            <Show when={props.showPodcast}>{episode().podcastTitle} · </Show>
            {dateLabel(episode().publishedAt)}
            <Show when={episode().durationMs}>{(ms) => <> · {durationLabel(ms())}</>}</Show>
          </span>
          <span class="title">{episode().title}</span>
          <Show when={progress()}>{(label) => <span class="progress">{label()}</span>}</Show>
        </span>
      </A>
      <span class="row-actions">
        <button class="icon-button" title={episode().played ? "Mark unplayed" : "Mark played"} onClick={() => setPlayed(episode(), !episode().played)}>
          {episode().played ? "↺" : "✓"}
        </button>
        <button class="icon-button" title={episode().archived ? "Return to inbox" : "Archive"} onClick={() => setArchived(episode(), !episode().archived)}>
          {episode().archived ? "⇡" : "⇣"}
        </button>
      </span>
    </li>
  );
}
