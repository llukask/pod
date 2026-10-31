import { A } from "@solidjs/router";
import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { currentPlayerSettings } from "../data/settings";
import { clockLabel, remainingLabel } from "../format";
import {
  durationMs,
  episode,
  error,
  fetchPlayers,
  handOff,
  output,
  playing,
  positionMs,
  seek,
  setSpeed,
  skip,
  sonosAvailable,
  takeBack,
  togglePlayback,
} from "../player/player";
import type { SonosPlayer } from "../player/sonos";
import { nextStep, SPEEDS } from "../player/settings";
import { skipLabel, speedLabel } from "../player/labels";
import { Ambient, Artwork } from "./Artwork";
import { Icon } from "./Icon";

// ==============================================================================
// The player (D1 player bar, Android's mini player and player sheet)
// ==============================================================================
//
// Wide screens get one bar along the bottom: what's playing, the transport,
// the scrubber and the speed. Phones get a mini player above the tab bar that
// opens a full-screen sheet with the same controls at thumb size. Both are
// rendered, and the stylesheet shows the one that fits. Neither shows while
// nothing is loaded; a restored episode (spec 8, cold start) counts as loaded.
//
// Each surface is washed with the episode's cover (`Ambient`), the one place
// the interface takes its colour from the show.
//
// While a Sonos group plays the episode (spec 13.4), the line under the title
// says where, and the speed pill steps aside: Sonos plays only at 1×.
//
// TODO: the browser's back button doesn't close the sheet yet.

// While the thumb is dragged the clocks follow it; the seek happens on release.
const [dragMs, setDragMs] = createSignal<number | null>(null);
const shownMs = () => dragMs() ?? positionMs();

export function PlayerBar() {
  const [expanded, setExpanded] = createSignal(false);

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

  return (
    <Show when={episode()}>
      {(now) => (
        <>
          <footer class="player-bar" aria-label="Player">
            <Ambient url={now().artworkUrl} />
            <A class="now-playing" href={`/podcasts/${now().podcastId}/${now().id}`}>
              <Artwork url={now().artworkUrl} size={52} title={now().podcastTitle} />
              <span class="episode-text">
                <span class="title">{now().title}</span>
                <Meta podcastTitle={now().podcastTitle} />
              </span>
            </A>
            <div class="transport">
              <Controls size="bar" />
              <Scrubber />
            </div>
            <div class="bar-extra">
              <OutputPicker />
              <SpeedButton />
            </div>
          </footer>

          <div class="mini-player" aria-label="Player">
            <Ambient url={now().artworkUrl} />
            <button class="mini-open" aria-label={`Open player: ${now().title}`} onClick={() => setExpanded(true)}>
              <Artwork url={now().artworkUrl} size={44} title={now().podcastTitle} />
              <span class="episode-text">
                <span class="title">{now().title}</span>
                <Meta podcastTitle={now().podcastTitle} />
              </span>
            </button>
            <button class="mini-play" title={playing() ? "Pause" : "Play"} onClick={() => togglePlayback()}>
              <Icon name={playing() ? "pause" : "play"} size={22} />
            </button>
            <span class="mini-progress" style={{ "--fill": fillPercent() }} />
          </div>

          <Show when={expanded()}>
            <PlayerSheet onClose={() => setExpanded(false)} />
          </Show>
        </>
      )}
    </Show>
  );
}

/** The phone's full player: the cover large, then the scrubber and the controls within thumb reach. */
function PlayerSheet(props: { onClose: () => void }) {
  let sheet!: HTMLDivElement;
  const onKey = (event: KeyboardEvent) => event.key === "Escape" && props.onClose();
  onMount(() => {
    window.addEventListener("keydown", onKey);
    sheet.focus();
  });
  onCleanup(() => window.removeEventListener("keydown", onKey));
  // Leaving the phone layout (a rotated tablet, a resized window) closes it.
  const wide = window.matchMedia("(min-width: 721px)");
  const onWide = () => wide.matches && props.onClose();
  wide.addEventListener("change", onWide);
  onCleanup(() => wide.removeEventListener("change", onWide));
  // Nothing loaded any more (signed out), nothing to show.
  createEffect(() => !episode() && props.onClose());

  return (
    <Show when={episode()}>
      {(now) => (
        <div class="player-sheet" role="dialog" aria-modal="true" aria-label="Now playing" tabindex="-1" ref={sheet}>
          <Ambient url={now().artworkUrl} />
          <button class="icon-button sheet-close" title="Close player" onClick={() => props.onClose()}>
            <Icon name="chevronDown" size={26} />
          </button>
          <div class="sheet-art">
            <Artwork url={now().artworkUrl} size={600} title={now().podcastTitle} />
          </div>
          <div class="sheet-text">
            <A class="title" href={`/podcasts/${now().podcastId}/${now().id}`} onClick={() => props.onClose()}>
              {now().title}
            </A>
            <A class="meta" href={`/podcasts/${now().podcastId}`} onClick={() => props.onClose()}>
              {now().podcastTitle}
            </A>
            <Show when={error()} fallback={<Where />}>
              {(message) => <p class="error">{message()}</p>}
            </Show>
          </div>
          <Scrubber />
          <Controls size="sheet" />
          <div class="sheet-extra">
            <OutputPicker />
            <SpeedButton />
          </div>
        </div>
      )}
    </Show>
  );
}

function Controls(props: { size: "bar" | "sheet" }) {
  const settings = () => currentPlayerSettings();
  const big = () => props.size === "sheet";
  return (
    <div class="controls" classList={{ big: big() }}>
      <button class="icon-button skip" title={`Back ${skipLabel(settings().skipBackMs)}`} onClick={() => skip("back")}>
        <Icon name="back" size={big() ? 32 : 22} />
        <span class="skip-amount">{settings().skipBackMs / 1000}</span>
      </button>
      <button class="play" title={playing() ? "Pause" : "Play"} onClick={() => togglePlayback()}>
        <Icon name={playing() ? "pause" : "play"} size={big() ? 34 : 20} />
      </button>
      <button class="icon-button skip" title={`Forward ${skipLabel(settings().skipForwardMs)}`} onClick={() => skip("forward")}>
        <Icon name="forward" size={big() ? 32 : 22} />
        <span class="skip-amount">{settings().skipForwardMs / 1000}</span>
      </button>
    </div>
  );
}

function Scrubber() {
  return (
    <div class="scrubber">
      <span class="clock">{clockLabel(shownMs())}</span>
      <input
        type="range"
        aria-label="Position"
        aria-valuetext={`${clockLabel(shownMs())} of ${durationMs() ? clockLabel(durationMs()!) : "unknown length"}`}
        min={0}
        max={durationMs() ?? 0}
        step={1000}
        value={shownMs()}
        disabled={!durationMs()}
        style={{ "--fill": fillPercent() }}
        onInput={(e) => setDragMs(Number(e.currentTarget.value))}
        onChange={(e) => {
          seek(Number(e.currentTarget.value));
          setDragMs(null);
        }}
      />
      <span class="clock">{durationMs() ? remainingLabel(shownMs(), durationMs()!) : "--:--"}</span>
    </div>
  );
}

function SpeedButton() {
  const settings = () => currentPlayerSettings();
  return (
    <Show when={output().kind === "local"}>
      <button class="speed" title="Playback speed, on every device" onClick={() => setSpeed(nextStep(SPEEDS, settings().speed))}>
        {speedLabel(settings().speed)}
      </button>
    </Show>
  );
}

// ==============================================================================
// Where it plays: the output picker and the line under the title
// ==============================================================================

/** "Playing on Couch" while a group plays the episode, or null on this device. */
function whereLabel(): string | null {
  const out = output();
  if (out.kind === "sonos") return `Playing on ${out.name}`;
  if (out.kind === "connecting") return `Connecting to ${out.name}…`;
  return null;
}

/** The line under the title: an error, else where it plays, else the show. */
function Meta(props: { podcastTitle: string }) {
  return (
    <Show when={!error() && whereLabel()} fallback={<span class="meta" classList={{ error: !!error() }}>{error() ?? props.podcastTitle}</span>}>
      {(label) => (
        <span class="meta where">
          <Icon name="speaker" size={14} />
          <span>{label()}</span>
        </span>
      )}
    </Show>
  );
}

/** The sheet's own line for where it plays, under the show's name. */
function Where() {
  return (
    <Show when={whereLabel()}>
      {(label) => (
        <p class="where">
          <Icon name="speaker" size={16} />
          <span>{label()}</span>
        </p>
      )}
    </Show>
  );
}

/**
 * "This device" and each Sonos group, fetched when the menu opens. The button
 * shows only while the server lists groups, or one plays the episode.
 */
function OutputPicker() {
  const [open, setOpen] = createSignal(false);
  /** The groups as last fetched, or null while the fetch runs. */
  const [players, setPlayers] = createSignal<SonosPlayer[] | null>(null);
  let opened = 0;

  const toggle = async () => {
    if (open()) return setOpen(false);
    const mine = ++opened;
    setPlayers(null);
    setOpen(true);
    const list = await fetchPlayers().catch(() => []);
    if (mine === opened) setPlayers(list);
  };
  const chosen = () => {
    const out = output();
    return out.kind === "local" ? null : out.playerId;
  };
  const choose = (player: SonosPlayer | null) => {
    setOpen(false);
    if (player === null) void takeBack();
    else void handOff(player);
  };
  const remote = () => output().kind !== "local";

  return (
    <Show when={sonosAvailable() || remote() || open()}>
      <div
        class="menu-anchor output-picker"
        onFocusOut={(e) => !e.currentTarget.contains(e.relatedTarget as Node | null) && setOpen(false)}
        onKeyDown={(e) => {
          // Escape closes the menu, and only the menu, not the sheet around it.
          if (e.key !== "Escape" || !open()) return;
          e.stopPropagation();
          setOpen(false);
        }}
      >
        <button
          class="output"
          classList={{ remote: remote() }}
          title={whereLabel() ?? "Play on a speaker"}
          aria-haspopup="menu"
          aria-expanded={open()}
          onClick={() => void toggle()}
        >
          <Icon name="speaker" size={18} />
        </button>
        <Show when={open()}>
          <div class="menu output-menu" role="menu" aria-label="Play on">
            <OutputItem name="This device" checked={chosen() === null} onChoose={() => choose(null)} />
            <Show when={players()} fallback={<p class="menu-note">Looking for speakers…</p>}>
              {(found) => (
                <For each={found()} fallback={<p class="menu-note">No speakers found</p>}>
                  {(player) => (
                    <OutputItem
                      name={player.name}
                      note={player.state === "other" ? "Playing something else" : undefined}
                      checked={chosen() === player.id}
                      onChoose={() => choose(player)}
                    />
                  )}
                </For>
              )}
            </Show>
          </div>
        </Show>
      </div>
    </Show>
  );
}

function OutputItem(props: { name: string; note?: string; checked: boolean; onChoose: () => void }) {
  return (
    <button role="menuitemradio" aria-checked={props.checked} classList={{ checked: props.checked }} onClick={() => props.onChoose()}>
      <span class="output-name">
        <span>{props.name}</span>
        <Show when={props.note}>{(note) => <span class="output-note">{note()}</span>}</Show>
      </span>
      <Show when={props.checked}>
        <Icon name="check" size={18} />
      </Show>
    </button>
  );
}

/** How far through the episode, as the CSS the scrubber's filled track reads. */
function fillPercent(): string {
  const duration = durationMs();
  return duration ? `${Math.min((shownMs() / duration) * 100, 100)}%` : "0%";
}
