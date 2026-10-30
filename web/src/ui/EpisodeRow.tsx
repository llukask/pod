import { A } from "@solidjs/router";
import { Show } from "solid-js";
import { type EpisodeView, current, setArchived, setPlayed } from "../data/episodes";
import { dateLabel, durationLabel, leftLabel } from "../format";
import { episode as playingEpisode, play, playing, positionMs as playingPositionMs } from "../player/player";
import { Artwork } from "./Artwork";
import { Icon } from "./Icon";

/** One episode in a list (D1 inbox rows, D2 podcast rows): where it's from, when, how long, how far. */
export function EpisodeRow(props: { episode: EpisodeView; href: string; showPodcast: boolean; showDate?: boolean }) {
  const episode = () => current(props.episode);
  const meta = () => {
    const { publishedAt, durationMs, positionMs, played } = episode();
    const length = played ? "Played" : positionMs > 0 && durationMs ? leftLabel(durationMs, positionMs) : durationMs ? durationLabel(durationMs) : null;
    const when = props.showDate === false ? null : dateLabel(publishedAt);
    return [when, length].filter(Boolean).join(" · ");
  };
  return (
    <li class="episode-row" classList={{ played: episode().played, current: playingEpisode()?.id === props.episode.id }}>
      <A class="episode-link" href={props.href} activeClass="selected">
        <Show when={props.showPodcast}>
          <Artwork url={episode().artworkUrl} size={52} title={episode().podcastTitle} />
        </Show>
        <span class="episode-text">
          <Show when={props.showPodcast}>
            <span class="show-name">{episode().podcastTitle}</span>
          </Show>
          <span class="title">{episode().title}</span>
          <span class="meta">{meta()}</span>
        </span>
      </A>
      <span class="row-actions">
        <button class="icon-button" title={episode().played ? "Mark unplayed" : "Mark played"} onClick={() => setPlayed(episode(), !episode().played)}>
          <Icon name={episode().played ? "undo" : "check"} />
        </button>
        <button class="icon-button" title={episode().archived ? "Return to inbox" : "Archive"} onClick={() => setArchived(episode(), !episode().archived)}>
          <Icon name={episode().archived ? "unarchive" : "archive"} />
        </button>
      </span>
      <PlayToggle episode={episode()} />
    </li>
  );
}

/**
 * The round play button. Its ring fills with how much of the episode has been
 * heard, so a list shows at a glance what's started; the current episode's
 * ring follows the player live.
 */
export function PlayToggle(props: { episode: EpisodeView; size?: number }) {
  const isCurrent = () => playingEpisode()?.id === props.episode.id;
  const isPlaying = () => isCurrent() && playing();
  const fraction = () => {
    const { durationMs, positionMs, played } = props.episode;
    if (played) return 1;
    if (!durationMs) return 0;
    return Math.min((isCurrent() ? playingPositionMs() : positionMs) / durationMs, 1);
  };
  // r = 19 on a 40-unit box leaves room for the 2-unit stroke. A started
  // episode shows at least a short arc, which reads as progress; a sliver
  // under that reads as a stray dot.
  const circumference = 2 * Math.PI * 19;
  const shown = () => (fraction() > 0 ? Math.max(fraction(), 0.06) : 0);
  return (
    <button
      class="play-toggle"
      classList={{ active: isCurrent(), playing: isPlaying(), started: fraction() > 0, done: props.episode.played }}
      title={isPlaying() ? "Pause" : "Play"}
      aria-label={`${isPlaying() ? "Pause" : "Play"} ${props.episode.title}`}
      onClick={() => play(props.episode)}
    >
      <svg class="ring" viewBox="0 0 40 40" aria-hidden="true">
        <circle class="track" cx="20" cy="20" r="19" />
        <circle class="fill" cx="20" cy="20" r="19" stroke-dasharray={`${circumference}`} stroke-dashoffset={`${circumference * (1 - shown())}`} />
      </svg>
      <Icon name={isPlaying() ? "pause" : "play"} size={props.size ?? 18} />
    </button>
  );
}
