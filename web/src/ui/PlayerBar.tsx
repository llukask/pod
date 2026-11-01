import { A } from "@solidjs/router";
import { For, Show, createEffect, createSignal, onCleanup, onMount } from "solid-js";
import { currentPlayerSettings } from "../data/settings";
import { clockLabel, remainingLabel } from "../format";
import { type SonosPlayer, isSonos } from "../player/connect";
import {
  activeDevice,
  activeName,
  durationMs,
  episode,
  error,
  fetchPlayers,
  otherDevices,
  output,
  playing,
  positionMs,
  seek,
  setSpeed,
  setVolume,
  skip,
  speakers,
  togglePlayback,
  transferTo,
  transferring,
  volume,
} from "../player/player";
import { nextStep, SPEEDS } from "../player/settings";
import { deviceKindLabel, skipLabel, speedLabel } from "../player/labels";
import { session } from "../session";
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
// While another device plays (spec 14.7), the line under the title says
// which, and the controls steer it. For a Sonos group the speed pill steps
// aside, since Sonos plays only at 1×, and the group volume takes its place.
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
              <VolumeControl />
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
            <VolumeControl />
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

/** The group volume, in the speed pill's place while a Sonos group plays. */
function VolumeControl() {
  return (
    <Show when={output().kind === "remote" && volume() !== null}>
      <div class="volume">
        <input
          type="range"
          aria-label={`Volume on ${activeName() ?? "the speaker"}`}
          min={0}
          max={100}
          step={1}
          value={volume() ?? 0}
          style={{ "--fill": `${volume() ?? 0}%` }}
          onInput={(e) => setVolume(Number(e.currentTarget.value))}
        />
      </div>
    </Show>
  );
}

/** The synced speed. A remote device other than a Sonos group gets it through sync (spec 8). */
function SpeedButton() {
  const settings = () => currentPlayerSettings();
  return (
    <Show when={!isSonos(activeDevice()) || output().kind === "local"}>
      <button class="speed" title="Playback speed, on every device" onClick={() => setSpeed(nextStep(SPEEDS, settings().speed))}>
        {speedLabel(settings().speed)}
      </button>
    </Show>
  );
}

// ==============================================================================
// Where it plays: the output picker and the line under the title
// ==============================================================================

/** "Playing on Couch" while another device has the episode, or null while it is here. */
function whereLabel(): string | null {
  const moving = transferring();
  if (moving) return `Connecting to ${moving}…`;
  const name = activeName();
  if (name === null) return null;
  return playing() ? `Playing on ${name}` : `Paused on ${name}`;
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
 * "This device", the account's other connected devices and each Sonos group,
 * fetched when the menu opens (spec 14.7). Choosing one moves the playback
 * there. The button shows while there is anywhere else to play, or another
 * device plays.
 */
function OutputPicker() {
  const [open, setOpen] = createSignal(false);
  /** The groups as fetched for this opening, or null while the fetch runs. */
  const [players, setPlayers] = createSignal<SonosPlayer[] | null>(null);
  let opened = 0;

  const toggle = async () => {
    if (open()) return setOpen(false);
    const mine = ++opened;
    setPlayers(null);
    setOpen(true);
    const list = await fetchPlayers().catch(() => speakers());
    if (mine === opened) setPlayers(list);
  };
  const choose = (target: { id: string; name: string }) => {
    setOpen(false);
    void transferTo(target);
  };
  const remote = () => output().kind === "remote";
  const thisDevice = () => ({ id: session()?.deviceId ?? "", name: "This device" });

  return (
    <Show when={otherDevices().length > 0 || speakers().length > 0 || remote() || open()}>
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
          title={whereLabel() ?? "Play on another device"}
          aria-haspopup="menu"
          aria-expanded={open()}
          onClick={() => void toggle()}
        >
          <Icon name="speaker" size={18} />
        </button>
        <Show when={open()}>
          <div class="menu output-menu" role="menu" aria-label="Play on">
            <OutputItem name="This device" checked={!remote()} onChoose={() => choose(thisDevice())} />
            <For each={otherDevices()}>
              {(device) => (
                <OutputItem name={device.name} note={deviceKindLabel(device.kind)} checked={activeDevice() === device.id} onChoose={() => choose(device)} />
              )}
            </For>
            <Show when={players()} fallback={<p class="menu-note">Looking for speakers…</p>}>
              {(found) => (
                <For each={found()} fallback={otherDevices().length === 0 ? <p class="menu-note">No other devices found</p> : undefined}>
                  {(player) => (
                    <OutputItem
                      name={player.name}
                      note={player.state === "other" ? "Playing something else" : undefined}
                      checked={activeDevice() === player.id}
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
