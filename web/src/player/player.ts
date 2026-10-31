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
// The output is this device or a Sonos group (spec 13.4). On a group,
// pod-server plays the episode and writes its listening state, so the <audio>
// is paused, the contract is null and the browser writes nothing. The same
// signals then follow the session as polled from the server, and the commands
// steer the group, so the UI and the media keys work the same on either.
//
// TODO: the sleep timer (D1 player bar, Android M8). Not in this first cut.

import { createEffect, createRoot, createSignal } from "solid-js";
import { ApiError } from "../api/http";
import { type EpisodeView, adoptServerState, current, refetchState, setPlayed, setPosition } from "../data/episodes";
import { currentPlayerSettings, setSetting } from "../data/settings";
import { flushWrites } from "../data/writes";
import { session } from "../session";
import { notify } from "../ui/Toasts";
import { Contract, type Stored, type Write } from "./contract";
import * as sonos from "./sonos";

const audio = new Audio();
audio.preload = "metadata";

const [episode, setEpisode] = createSignal<EpisodeView | null>(null);
const [playing, setPlaying] = createSignal(false);
const [positionMs, setPositionMs] = createSignal(0);
const [durationMs, setDurationMs] = createSignal<number | null>(null);
const [error, setError] = createSignal<string | null>(null);

export { episode, playing, positionMs, durationMs, error };

/** Where the episode plays. `connecting` waits for a group to answer a hand-off. */
export type Output = { kind: "local" } | { kind: "connecting"; playerId: string; name: string } | Remote;
type Remote = { kind: "sonos"; playerId: string; name: string };

const LOCAL: Output = { kind: "local" };
const [output, setOutput] = createSignal<Output>(LOCAL);
/** Whether the server lists any Sonos group. The output picker only shows when it does. */
const [sonosAvailable, setSonosAvailable] = createSignal(false);

export { output, sonosAvailable };

/** The contract of the current play, or null while a restored episode waits for its first play, and on a Sonos group. */
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
  const out = output();
  if (out.kind === "connecting") return;
  if (out.kind === "sonos") {
    void startOn(out, next, current(next));
    return;
  }
  stopCurrent();
  const stored = current(next);
  load(next, stored.played ? 0 : stored.positionMs);
  begin({ positionMs: stored.positionMs, played: stored.played });
}

export function togglePlayback() {
  const now = episode();
  const out = output();
  if (!now || out.kind === "connecting") return;
  if (out.kind === "sonos") {
    const before = shown();
    setRemote(!before.playing, before.positionMs);
    void send(out, before.playing ? { type: "pause" } : { type: "resume" }, before);
    return;
  }
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
  const out = output();
  if (out.kind === "connecting") return;
  if (out.kind === "sonos") {
    const before = shown();
    setRemote(before.playing, target);
    void send(out, { type: "seek", position_ms: Math.round(target) }, before);
    return;
  }
  audio.currentTime = target / 1000;
  setPositionMs(target);
  if (contract) apply(contract.seek(target, Date.now()));
  else remember();
}

export function skip(direction: "back" | "forward") {
  const settings = currentPlayerSettings();
  const deltaMs = direction === "back" ? -settings.skipBackMs : settings.skipForwardMs;
  const out = output();
  if (out.kind !== "sonos") {
    seek(positionMs() + deltaMs);
    return;
  }
  // The group skips from its own position; the clock between polls stands in for it here.
  const before = shown();
  setRemote(before.playing, Math.max(0, Math.min(before.positionMs + deltaMs, durationMs() ?? Infinity)));
  void send(out, { type: "skip", delta_ms: deltaMs }, before);
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
  show(next, atMs);
  audio.src = next.audioUrl;
  audio.currentTime = atMs / 1000;
  audio.playbackRate = currentPlayerSettings().speed;
}

/** Shows `next` without touching the <audio>, which a Sonos group doesn't use. */
function show(next: EpisodeView, atMs: number) {
  setError(null);
  setEpisode(next);
  setDurationMs(next.durationMs);
  setPositionMs(atMs);
  updateMediaSession(next);
}

function begin(stored: { positionMs: number; played: boolean }) {
  const now = episode();
  if (!now) return;
  const [started, writes] = Contract.start(stored, durationMs(), currentPlayerSettings().playedThresholdMs, Date.now());
  contract = started;
  apply(writes);
  // A replay starts over from wherever the <audio> was left.
  if (started.positionMs !== positionMs()) {
    audio.currentTime = started.positionMs / 1000;
    setPositionMs(started.positionMs);
  }
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

// The <audio> is paused on a Sonos group, but a pause or seek it was given
// before the hand-off can still report in; the session owns the signals then.
const local = () => output().kind === "local";

audio.addEventListener("play", () => {
  if (!local()) return;
  setPlaying(true);
  contract?.resume(Date.now());
});

audio.addEventListener("pause", () => {
  if (!local()) return;
  setPlaying(false);
  if (contract && !audio.ended) apply(contract.pause(Date.now()));
});

audio.addEventListener("timeupdate", () => {
  if (!local()) return;
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
  if (!local()) return;
  setPlaying(false);
  if (contract) apply(contract.ended(Date.now()));
  contract = null;
});

audio.addEventListener("error", () => {
  if (!audio.src || !local()) return;
  setPlaying(false);
  setError("This episode can't be played. The publisher's server may be down.");
});

// ==============================================================================
// Sonos groups (spec 13.4)
// ==============================================================================

/**
 * How long the group may be missing from the list before its session counts
 * as over. The server leaves out a group that didn't answer this time, and
 * keeps the session of one that stays silent this long (spec 13.3).
 */
const MISSING_LIMIT_MS = 2 * 60_000;

/** Stops the polling and the clock of the current Sonos session. */
let stopRemote: (() => void) | null = null;
/** The last position the session reported and when, so the clock runs between polls. */
let anchor = { positionMs: 0, at: 0 };
/** Counts commands, so a poll sent before one doesn't undo what it showed. */
let commands = 0;
/** When the group first went missing from the list, or null while it is there. */
let missingSince: number | null = null;

/** The groups the server lists. Keeps `sonosAvailable` current, so the picker hides when the list comes back empty. */
export async function fetchPlayers(): Promise<sonos.SonosPlayer[]> {
  const players = await sonos.listPlayers();
  setSonosAvailable(players.length > 0);
  return players;
}

/**
 * Hands the episode to `target`: the local play stops with its ordinary pause
 * write, and the group starts where a local play would have (spec 8). From
 * another group, the episode goes on where that one released it.
 */
export async function handOff(target: { id: string; name: string }) {
  const now = episode();
  const out = output();
  if (!now || out.kind === "connecting" || (out.kind === "sonos" && out.playerId === target.id)) return;
  let stored: Stored;
  if (out.kind === "sonos") {
    const released = await releaseFrom(out);
    if (released === null) return;
    stored = { positionMs: released.positionMs, played: false };
  } else {
    stopCurrent();
    setPlaying(false);
    stored = { positionMs: positionMs(), played: current(now).played };
  }
  await startOn({ kind: "sonos", playerId: target.id, name: target.name }, now, stored);
}

/** Takes the episode back from the group and goes on here, playing or paused as it was there. Not a replay. */
export async function takeBack() {
  const now = episode();
  const out = output();
  if (!now || out.kind !== "sonos") return;
  const released = await releaseFrom(out);
  if (released === null || episode()?.id !== now.id) return;
  load(now, released.positionMs);
  if (released.playing) begin({ positionMs: released.positionMs, played: false });
  else remember();
}

/**
 * Ends the session on `out` and leaves Sonos mode. Returns where the episode
 * stood and whether it played, or null when the group couldn't be reached and
 * the session goes on.
 */
async function releaseFrom(out: Remote): Promise<{ positionMs: number; playing: boolean } | null> {
  let atMs = positionMs();
  let wasPlaying = playing();
  try {
    const released = await sonos.release(out.playerId);
    if (released.episode_id === episode()?.id) {
      atMs = released.position_ms;
      wasPlaying = released.playing;
    }
  } catch (error) {
    if (output() !== out) return null;
    if (!sessionGone(error)) {
      notify(failure(out.name, error));
      return null;
    }
    // Over already, so nothing plays on that the browser could take over.
    wasPlaying = false;
  }
  if (output() !== out) return null;
  leaveRemote();
  const now = episode();
  if (now) adoptServerState(now.id, { positionMs: atMs });
  return { positionMs: atMs, playing: wasPlaying };
}

/** Starts `next` on the group of `target` at the start position for `stored`. */
async function startOn(target: Remote, next: EpisodeView, stored: Stored) {
  const start = sonos.handOffStart(stored);
  if (start.unplay) setPlayed(next, false);
  const connecting: Output = { ...target, kind: "connecting" };
  stopRemote?.();
  setOutput(connecting);
  setPlaying(false);
  show(next, start.positionMs);
  remember();
  try {
    // The server starts from its own copy of the episode's state, so the pause
    // write and the replay reset go out first.
    await flushWrites();
    const player = await sonos.playOn(target.playerId, next.id, start.positionMs);
    if (output() !== connecting) return;
    const seen = sonos.observe([player], next.id, target.playerId);
    // The group was just told to play and may still be buffering, which the
    // answer reports as not playing; the polls take over from here.
    enterRemote(target, seen.kind === "session" ? seen.session.position_ms : start.positionMs, true);
  } catch (error) {
    if (output() !== connecting) return;
    // TODO: when another episode was playing on this group, its session goes
    // on there and this tab no longer follows it.
    setOutput(LOCAL);
    load(next, start.positionMs);
    notify(failure(target.name, error));
  }
}

function enterRemote(target: Remote, atMs: number, isPlaying: boolean) {
  stopRemote?.();
  const out: Remote = { ...target };
  setOutput(out);
  missingSince = null;
  setRemote(isPlaying, atMs);
  const stopPolling = sonos.poll(() => pollOnce(out));
  const clock = setInterval(() => {
    if (!playing()) return;
    const duration = durationMs();
    setPositionMs(Math.min(anchor.positionMs + Date.now() - anchor.at, duration ?? Infinity));
  }, 500);
  stopRemote = () => {
    stopPolling();
    clearInterval(clock);
  };
}

function leaveRemote() {
  stopRemote?.();
  stopRemote = null;
  setOutput(LOCAL);
  setPlaying(false);
}

/** Shows the session at `atMs`, playing or not, here and in the episode lists. */
function setRemote(isPlaying: boolean, atMs: number) {
  anchor = { positionMs: atMs, at: Date.now() };
  setPlaying(isPlaying);
  setPositionMs(atMs);
  const now = episode();
  if (now) adoptServerState(now.id, { positionMs: atMs });
  remember();
  updatePositionState();
}

/**
 * Sends a command the UI already shows. If the group can't be reached, the
 * session shows as it was before, since the speaker never heard it.
 */
async function send(out: Remote, body: sonos.SonosCommand, before: { playing: boolean; positionMs: number }) {
  commands++;
  try {
    const player = await sonos.command(out.playerId, body);
    if (output() === out) follow(out, [player]);
  } catch (error) {
    if (output() !== out) return;
    if (sessionGone(error)) {
      endRemote();
      return;
    }
    setRemote(before.playing, before.positionMs);
    notify(failure(out.name, error));
  }
}

/** The session as the UI shows it now, for `send` to go back to. */
const shown = () => ({ playing: playing(), positionMs: positionMs() });

async function pollOnce(out: Remote) {
  const sent = commands;
  const players = await fetchPlayers();
  if (output() === out && sent === commands) follow(out, players);
}

function follow(out: Remote, players: sonos.SonosPlayer[]) {
  const now = episode();
  if (!now) return;
  const seen = sonos.observe(players, now.id, out.playerId);
  if (seen.kind === "session") {
    missingSince = null;
    setRemote(seen.session.playing, seen.session.position_ms);
  } else if (seen.kind === "ended") {
    endRemote();
  } else {
    missingSince ??= Date.now();
    if (Date.now() - missingSince >= MISSING_LIMIT_MS) endRemote();
  }
}

/**
 * The session ended on the group's side: the episode finished, or the group
 * plays something else now. The episode stays here, paused where it was last
 * seen, and its state is read again, since pod-server may have marked it played.
 */
function endRemote() {
  leaveRemote();
  const now = episode();
  if (!now) return;
  load(now, positionMs());
  remember();
  void refetchState(now).catch(() => {
    // Offline; the lists catch up on their next load.
  });
}

/** A 409 `no_session` or a 404 for a group that is gone: the session is over. */
function sessionGone(error: unknown): boolean {
  return error instanceof ApiError && (error.code === "no_session" || error.status === 404);
}

/** A `502` is the speaker; anything else, such as no answer from pod-server itself, says what it is. */
function failure(name: string, error: unknown): string {
  if (error instanceof ApiError && error.status === 502) return `Couldn't reach ${name}.`;
  return error instanceof ApiError ? error.message : `Couldn't play on ${name}.`;
}

/**
 * Picks up a session that plays on while this tab was closed (spec 13.4), and
 * learns whether to show the picker. Only a session of the restored episode is
 * followed.
 */
async function resumeRemote() {
  const userId = session()?.userId;
  const players = await fetchPlayers().catch(() => null);
  if (!players || session()?.userId !== userId) return;
  const now = episode();
  // TODO: follow a session of another episode, which needs that episode's
  // metadata; there is no read endpoint for one episode by id.
  if (!now || contract || output().kind !== "local") return;
  const seen = sonos.observe(players, now.id, null);
  if (seen.kind !== "session") return;
  audio.pause();
  enterRemote({ kind: "sonos", playerId: seen.player.id, name: seen.player.name }, seen.session.position_ms, seen.session.playing);
}

window.addEventListener("online", () => void resumeRemote());

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
    leaveRemote();
    setSonosAvailable(false);
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
    void resumeRemote();
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
      playbackRate: local() ? audio.playbackRate : 1,
    });
  } catch {
    // A position past a duration the element revised; the next update fixes it.
  }
}

if ("mediaSession" in navigator) {
  const handlers: [MediaSessionAction, MediaSessionActionHandler][] = [
    ["play", () => !playing() && togglePlayback()],
    ["pause", () => playing() && togglePlayback()],
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
