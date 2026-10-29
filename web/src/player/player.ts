// ==============================================================================
// The player: one <audio> element and the contract that decides its writes
// ==============================================================================
//
// The module owns the page's only audio element. The UI reads its signals and
// calls its commands; the element's events drive the contract (contract.ts),
// whose writes go through the same outbox as every other action. Nothing else
// writes listening state.
//
// Spec 8, as it applies to a browser:
// - Source: always the stream. The web client keeps no downloads.
// - Start position, replay reset, 5 s saves and the played threshold: the
//   contract.
// - Restore on cold start: the episode and its position are saved per user in
//   localStorage with every position write, and come back paused on load.
// - Media keys and the OS's now-playing view go through the Media Session API.
//
// TODO: the sleep timer (D1 player bar, Android M8). Not in this first cut.

import { createEffect, createRoot, createSignal } from "solid-js";
import { type EpisodeView, current, setPlayed, setPosition } from "../data/episodes";
import { currentPlayerSettings, setSetting } from "../data/settings";
import { flushWrites } from "../data/writes";
import { session } from "../session";
import { Contract, type Write } from "./contract";

const audio = new Audio();
audio.preload = "metadata";

const [episode, setEpisode] = createSignal<EpisodeView | null>(null);
const [playing, setPlaying] = createSignal(false);
const [positionMs, setPositionMs] = createSignal(0);
const [durationMs, setDurationMs] = createSignal<number | null>(null);
const [error, setError] = createSignal<string | null>(null);

export { episode, playing, positionMs, durationMs, error };

/** The contract of the current play, or null while a restored episode waits for its first play. */
let contract: Contract | null = null;

// ==============================================================================
// Commands
// ==============================================================================

/** Plays `next`, or toggles it if it is the current episode, the restored one included. */
export function play(next: EpisodeView) {
  if (episode()?.id === next.id) {
    togglePlayback();
    return;
  }
  stopCurrent();
  const stored = current(next);
  load(next, stored.played ? 0 : stored.positionMs);
  begin({ positionMs: stored.positionMs, played: stored.played });
}

export function togglePlayback() {
  const now = episode();
  if (!now) return;
  if (!contract) {
    // A restored episode: its first play starts the contract at the saved spot.
    begin({ positionMs: positionMs(), played: current(now).played });
    return;
  }
  if (audio.paused) void start();
  else audio.pause();
}

export function seek(toMs: number) {
  const duration = durationMs();
  const target = Math.max(0, duration ? Math.min(toMs, duration) : toMs);
  audio.currentTime = target / 1000;
  setPositionMs(target);
  if (contract) apply(contract.seek(target, Date.now()));
  else remember();
}

export function skip(direction: "back" | "forward") {
  const settings = currentPlayerSettings();
  seek(positionMs() + (direction === "back" ? -settings.skipBackMs : settings.skipForwardMs));
}

/** Sets the speed here and, as a synced setting, on every device (spec 8). */
export function setSpeed(speed: number) {
  audio.playbackRate = speed;
  setSetting("playback_speed", speed);
}

export function setSkip(direction: "back" | "forward", ms: number) {
  setSetting(direction === "back" ? "skip_back_ms" : "skip_forward_ms", ms);
}

// ==============================================================================
// Loading and the contract
// ==============================================================================

function load(next: EpisodeView, atMs: number) {
  setError(null);
  setEpisode(next);
  setDurationMs(next.durationMs);
  setPositionMs(atMs);
  audio.src = next.audioUrl;
  audio.currentTime = atMs / 1000;
  audio.playbackRate = currentPlayerSettings().speed;
  updateMediaSession(next);
}

function begin(stored: { positionMs: number; played: boolean }) {
  const now = episode();
  if (!now) return;
  const [started, writes] = Contract.start(stored, durationMs(), currentPlayerSettings().playedThresholdMs, Date.now());
  contract = started;
  apply(writes);
  void start();
}

async function start() {
  try {
    await audio.play();
  } catch (e) {
    // Autoplay rules can refuse a play that didn't follow a click; the
    // button then simply shows Play again.
    if (e instanceof DOMException && e.name === "NotAllowedError") return;
    if (e instanceof DOMException && e.name === "AbortError") return;
    setError("This episode can't be played.");
  }
}

/** Ends the current play with a last position save. */
function stopCurrent() {
  if (contract && episode()) apply(contract.stop(Date.now()));
  contract = null;
  audio.pause();
}

function apply(writes: Write[]) {
  const now = episode();
  if (!now) return;
  for (const w of writes) {
    if (w.kind === "played") setPlayed(now, w.played);
    else {
      setPosition(now, w.positionMs, { lazy: w.whilePlaying });
      remember();
    }
  }
}

audio.addEventListener("play", () => {
  setPlaying(true);
  contract?.resume(Date.now());
});

audio.addEventListener("pause", () => {
  setPlaying(false);
  if (contract && !audio.ended) apply(contract.pause(Date.now()));
});

audio.addEventListener("timeupdate", () => {
  const ms = Math.round(audio.currentTime * 1000);
  setPositionMs(ms);
  if (contract && !audio.paused) apply(contract.progress(ms, Date.now()));
  updatePositionState();
});

audio.addEventListener("durationchange", () => {
  if (!Number.isFinite(audio.duration)) return;
  const ms = Math.round(audio.duration * 1000);
  setDurationMs(ms);
  contract?.setDuration(ms);
});

audio.addEventListener("ended", () => {
  setPlaying(false);
  if (contract) apply(contract.ended(Date.now()));
  contract = null;
});

audio.addEventListener("error", () => {
  if (!audio.src) return;
  setPlaying(false);
  setError("This episode can't be played. The publisher's server may be down.");
});

// ==============================================================================
// Leaving the page and coming back
// ==============================================================================

// The tab is hidden before it can be closed, and a hidden phone tab can be
// killed without another event. Saving here, while requests still go out,
// keeps the position the next device sees exact.
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState !== "hidden" || !contract || !episode()) return;
  apply(contract.stop(Date.now()));
  void flushWrites();
});

type Saved = { episode: EpisodeView; positionMs: number };

const savedKey = () => {
  const userId = session()?.userId;
  return userId ? `pod.player.${userId}` : null;
};

/** Saves the episode and position for the next load (spec 8, restore on cold start). */
function remember() {
  const key = savedKey();
  const now = episode();
  if (!key || !now) return;
  try {
    localStorage.setItem(key, JSON.stringify({ episode: now, positionMs: positionMs() } satisfies Saved));
  } catch {
    // Storage is off; nothing comes back after a reload.
  }
}

createRoot(() => {
  // A new user stops whatever the last one played and restores their own.
  createEffect(() => {
    const key = savedKey();
    stopCurrent();
    setEpisode(null);
    audio.removeAttribute("src");
    if (!key) return;
    try {
      const saved: Saved | null = JSON.parse(localStorage.getItem(key) ?? "null");
      if (saved?.episode?.audioUrl) load(saved.episode, saved.positionMs);
    } catch {
      // A broken entry is as good as none.
    }
  });
});

// ==============================================================================
// Media Session: media keys, lock screens and the OS's now-playing view
// ==============================================================================

function updateMediaSession(now: EpisodeView) {
  if (!("mediaSession" in navigator)) return;
  navigator.mediaSession.metadata = new MediaMetadata({
    title: now.title,
    artist: now.podcastTitle,
    artwork: now.artworkUrl ? [{ src: now.artworkUrl }] : [],
  });
}

function updatePositionState() {
  const duration = durationMs();
  if (!("mediaSession" in navigator) || !duration) return;
  try {
    navigator.mediaSession.setPositionState({
      duration: duration / 1000,
      position: Math.min(positionMs(), duration) / 1000,
      playbackRate: audio.playbackRate,
    });
  } catch {
    // A position past a duration the element revised; the next update fixes it.
  }
}

if ("mediaSession" in navigator) {
  const handlers: [MediaSessionAction, MediaSessionActionHandler][] = [
    ["play", () => togglePlayback()],
    ["pause", () => audio.pause()],
    ["seekbackward", () => skip("back")],
    ["seekforward", () => skip("forward")],
    ["seekto", (details) => details.seekTime !== undefined && seek(details.seekTime * 1000)],
  ];
  for (const [action, handler] of handlers) {
    try {
      navigator.mediaSession.setActionHandler(action, handler);
    } catch {
      // The browser doesn't know this action.
    }
  }
}

// A synced speed change from Settings applies to the episode playing now.
createRoot(() => {
  createEffect(() => {
    audio.playbackRate = currentPlayerSettings().speed;
  });
});
