import { A } from "@solidjs/router";
import { Show } from "solid-js";
import { type EpisodeView, current, setArchived, setPlayed } from "../data/episodes";
import { dateLabel, durationLabel, leftLabel } from "../format";
import { episode as playingEpisode, play, playing } from "../player/player";
import { showNotes } from "../showNotes";
import { Artwork } from "./Artwork";
import { Icon } from "./Icon";

/** The selected episode (D1, detail pane): what it is, what you can do with it, and its notes. */
export function EpisodeDetail(props: { episode: EpisodeView; onClose?: () => void; backLabel: string }) {
  const episode = () => current(props.episode);
  const isPlaying = () => playingEpisode()?.id === episode().id && playing();
  return (
    <article class="episode-detail">
      {/* Wide screens close the pane; on phones the detail is a page of its own, so it goes back. */}
      <Show when={props.onClose}>
        {(close) => (
          <>
            <button class="icon-button close" title="Close" onClick={() => close()()}>
              <Icon name="close" />
            </button>
            <button class="back" onClick={() => close()()}>
              <Icon name="arrowLeft" size={18} />
              {props.backLabel}
            </button>
          </>
        )}
      </Show>
      <header>
        <Artwork url={episode().artworkUrl} size={112} title={episode().podcastTitle} />
        <div class="heading">
          <A class="show-link" href={`/podcasts/${episode().podcastId}`}>
            {episode().podcastTitle}
          </A>
          <h2>{episode().title}</h2>
          <p class="meta">
            {dateLabel(episode().publishedAt)}
            <Show when={episode().durationMs}>{(ms) => <> · {durationLabel(ms())}</>}</Show>
            <Show when={episode().played}> · Played</Show>
          </p>
        </div>
      </header>
      <div class="actions">
        <button class="button primary play-button" onClick={() => play(episode())}>
          <Icon name={isPlaying() ? "pause" : "play"} size={18} />
          {playLabel(episode(), isPlaying())}
        </button>
        <button class="button" onClick={() => setPlayed(episode(), !episode().played)}>
          <Icon name={episode().played ? "undo" : "check"} size={18} />
          {episode().played ? "Mark unplayed" : "Mark played"}
        </button>
        <button class="button" onClick={() => setArchived(episode(), !episode().archived)}>
          <Icon name={episode().archived ? "unarchive" : "archive"} size={18} />
          {episode().archived ? "Return to inbox" : "Archive"}
        </button>
        <Show when={episode().link}>
          {(link) => (
            <a class="button quiet" href={link()} target="_blank" rel="noopener noreferrer">
              <Icon name="external" size={18} />
              Episode page
            </a>
          )}
        </Show>
      </div>
      {/* Rebuilt from an allowlist, never assigned as HTML (showNotes.ts). */}
      <div class="notes">{showNotes(episode().notesHtml)}</div>
    </article>
  );
}

/** "Pause" while it plays, "Resume · 21 min left" once started, else "Play" (Android M7). */
function playLabel(episode: EpisodeView, isPlaying: boolean): string {
  if (isPlaying) return "Pause";
  if (!episode.played && episode.positionMs > 0 && episode.durationMs) return `Resume · ${leftLabel(episode.durationMs, episode.positionMs)}`;
  return "Play";
}
