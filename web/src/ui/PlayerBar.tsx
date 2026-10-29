import { A } from "@solidjs/router";
import { Show, createSignal, onCleanup, onMount } from "solid-js";
import { currentPlayerSettings } from "../data/settings";
import { clockLabel, remainingLabel } from "../format";
import { durationMs, episode, error, playing, positionMs, seek, setSpeed, skip, togglePlayback } from "../player/player";
import { nextStep, SPEEDS } from "../player/settings";
import { speedLabel } from "../player/labels";
import { Artwork } from "./Artwork";

// Wireframe D1, the player bar: what's playing, the transport, the scrubber
// and the speed. It is always there; with nothing loaded it says so.
export function PlayerBar() {
  // While the thumb is dragged the label follows it; the seek happens on release.
  const [dragMs, setDragMs] = createSignal<number | null>(null);
  const shownMs = () => dragMs() ?? positionMs();

  // Space plays and pauses, unless a field or button has the keyboard.
  const onKey = (event: KeyboardEvent) => {
    if (event.code !== "Space" || event.repeat || !episode()) return;
    const target = event.target as HTMLElement;
    if (target.closest("input, textarea, select, button, a, [contenteditable]")) return;
    event.preventDefault();
    togglePlayback();
  };
  onMount(() => window.addEventListener("keydown", onKey));
  onCleanup(() => window.removeEventListener("keydown", onKey));

  const settings = () => currentPlayerSettings();

  return (
    <footer class="player-bar">
      <Show when={episode()} fallback={<p class="muted nothing">Nothing playing</p>}>
        {(now) => (
          <>
            <A class="now-playing" href={`/podcasts/${now().podcastId}/${now().id}`}>
              <Artwork url={now().artworkUrl} size={48} />
              <span class="episode-text">
                <span class="title">{now().title}</span>
                <span class="meta">{error() ?? now().podcastTitle}</span>
              </span>
            </A>
            <div class="transport">
              <div class="buttons">
                <button class="icon-button skip" title={`Back ${settings().skipBackMs / 1000} s`} onClick={() => skip("back")}>
                  ↺{settings().skipBackMs / 1000}
                </button>
                <button class="play" title={playing() ? "Pause" : "Play"} onClick={() => togglePlayback()}>
                  {playing() ? "❚❚" : "▶"}
                </button>
                <button class="icon-button skip" title={`Forward ${settings().skipForwardMs / 1000} s`} onClick={() => skip("forward")}>
                  {settings().skipForwardMs / 1000}↻
                </button>
              </div>
              <div class="scrubber">
                <span class="clock">{clockLabel(shownMs())}</span>
                <input
                  type="range"
                  aria-label="Position"
                  min={0}
                  max={durationMs() ?? 0}
                  step={1000}
                  value={shownMs()}
                  disabled={!durationMs()}
                  onInput={(e) => setDragMs(Number(e.currentTarget.value))}
                  onChange={(e) => {
                    seek(Number(e.currentTarget.value));
                    setDragMs(null);
                  }}
                />
                <span class="clock">{durationMs() ? remainingLabel(shownMs(), durationMs()!) : "--:--"}</span>
              </div>
            </div>
            <button class="button speed" title="Playback speed, on every device" onClick={() => setSpeed(nextStep(SPEEDS, settings().speed))}>
              {speedLabel(settings().speed)}
            </button>
          </>
        )}
      </Show>
    </footer>
  );
}
