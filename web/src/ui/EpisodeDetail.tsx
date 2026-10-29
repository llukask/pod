import { Show } from "solid-js";
import { type EpisodeView, current, setArchived, setPlayed } from "../data/episodes";
import { dateLabel, durationLabel, leftLabel } from "../format";
import { episode as playingEpisode, play, playing } from "../player/player";
import { showNotes } from "../showNotes";
import { Artwork } from "./Artwork";

/** The selected episode (D1, detail pane): what it is, what you can do with it, and its notes. */
export function EpisodeDetail(props: { episode: EpisodeView; onClose: () => void }) {
  const episode = () => current(props.episode);
  return (
    <article class="episode-detail">
      <button class="icon-button close" title="Close" onClick={() => props.onClose()}>
        ✕
      </button>
      <header>
        <Artwork url={episode().artworkUrl} size={96} />
        <div>
          <p class="meta">{episode().podcastTitle}</p>
          <h2>{episode().title}</h2>
          <p class="meta">
            {dateLabel(episode().publishedAt)}
            <Show when={episode().durationMs}>{(ms) => <> · {durationLabel(ms())}</>}</Show>
          </p>
        </div>
      </header>
      <div class="actions">
        <button class="button primary" onClick={() => play(episode())}>
          {playLabel(episode())}
        </button>
        <button class="button" onClick={() => setPlayed(episode(), !episode().played)}>
          {episode().played ? "Mark unplayed" : "Mark played"}
        </button>
        <button class="button" onClick={() => setArchived(episode(), !episode().archived)}>
          {episode().archived ? "Return to inbox" : "Archive"}
        </button>
        <Show when={episode().link}>
          {(link) => (
            <a class="button" href={link()} target="_blank" rel="noopener noreferrer">
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
function playLabel(episode: EpisodeView): string {
  if (playingEpisode()?.id === episode.id && playing()) return "Pause";
  if (!episode.played && episode.positionMs > 0 && episode.durationMs) return `Resume · ${leftLabel(episode.durationMs, episode.positionMs)}`;
  return "Play";
}
