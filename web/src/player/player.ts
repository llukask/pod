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
//   localStorage with every position write, and come back paused on load,
//   until the Connect stream says what the account played last.
// - Media keys and the OS's now-playing view go through the Media Session API.
//
// Connect (spec 14): while the tab is signed in it holds the event stream
// (stream.ts) and is a device the others can see and steer. The output is
// `local` while this tab plays or nothing does, and `remote` while another
// device (a phone, the desktop app, a Sonos group) has the playback. Remote,
// the <audio> is paused, the contract is null and the tab writes nothing; the
// same signals then show the playback as the stream sends it, and the
// commands go to the device that plays, so the UI and the media keys work the
// same either way. Local, the tab reports what its player does (14.3) and runs
// the commands other devices send it as if its own buttons were pressed.
//
// TODO: the sleep timer (D1 player bar, Android M8). Not in this first cut.

import { createEffect, createRoot, createSignal, onCleanup, untrack } from "solid-js";
import { ApiError } from "../api/http";
import {
  type EpisodeView,
  adoptServerState,
  current,
  findEpisode,
  fromPlayback,
  knownEpisode,
  localPosition,
  playable,
  refetchState,
  setPlayed,
  setPosition,
} from "../data/episodes";
import { currentPlayerSettings, setSetting } from "../data/settings";
import { flushWrites } from "../data/writes";
import { expire, session } from "../session";
import { notify } from "../ui/Toasts";
import * as connect from "./connect";
import { type ConnectCommand, type ConnectEvent, type ConnectState, type Playback, type SonosPlayer, isSonos, latestOnly } from "./connect";
import { Contract, type Stored, type Write, startPosition } from "./contract";
import { holdStream } from "./stream";

const audio = new Audio();
audio.preload = "metadata";

const [episode, setEpisode] = createSignal<EpisodeView | null>(null);
const [playing, setPlaying] = createSignal(false);
const [positionMs, setPositionMs] = createSignal(0);
const [durationMs, setDurationMs] = createSignal<number | null>(null);
const [error, setError] = createSignal<string | null>(null);

export { episode, playing, positionMs, durationMs, error };

/** Where the episode plays: here (or nowhere yet), or on another device whose playback the player follows. */
export type Output = { kind: "local" } | { kind: "remote"; playback: Playback };

const LOCAL: Output = { kind: "local" };
const [output, setOutput] = createSignal<Output>(LOCAL);

/** The group volume, 0 to 100, while a Sonos group plays; null otherwise. */
const [volume, setVolumeShown] = createSignal<number | null>(null);

export { output, volume };

/** The contract of the current play, or null while a restored episode waits for its first play, and while remote. */
let contract: Contract | null = null;

const ownId = () => session()?.deviceId ?? "";
const local = () => output().kind === "local";

// ==============================================================================
// Commands
// ==============================================================================

/** Plays `next`, or toggles it if it is the current episode, the restored one included. */
export function play(next: EpisodeView) {
  if (episode()?.id === next.id) {
    togglePlayback();
    return;
  }
  if (!local()) {
    playRemote(next);
    return;
  }
  playLocal(next, current(next));
}

export function togglePlayback() {
  const now = episode();
  if (!now) return;
  if (!local()) {
    const before = shown();
    showRemote(!before.playing, before.positionMs);
    void send(before.playing ? { type: "pause" } : { type: "resume" }, before);
    return;
  }
  if (!contract) {
    void startShown(now);
    return;
  }
  if (audio.paused) void start();
  else audio.pause();
}

export function seek(toMs: number) {
  const duration = durationMs();
  const target = Math.max(0, duration ? Math.min(toMs, duration) : toMs);
  if (!local()) {
    const before = shown();
    showRemote(before.playing, target);
    void send({ type: "seek", position_ms: Math.round(target) }, before);
    return;
  }
  audio.currentTime = target / 1000;
  setPositionMs(target);
  if (contract) apply(contract.seek(target, Date.now()));
  else remember();
  report();
}

export function skip(direction: "back" | "forward") {
  const settings = currentPlayerSettings();
  const deltaMs = direction === "back" ? -settings.skipBackMs : settings.skipForwardMs;
  if (local()) {
    seek(positionMs() + deltaMs);
    return;
  }
  // The device skips from its own position; the interpolated one stands in for it here.
  const before = shown();
  showRemote(before.playing, Math.max(0, Math.min(before.positionMs + deltaMs, durationMs() ?? Infinity)));
  void send({ type: "skip", delta_ms: deltaMs }, before);
}

/** Sets the speed here and, as a synced setting, on every device (spec 8), which reaches a remote one through its sync. */
export function setSpeed(speed: number) {
  if (local()) audio.playbackRate = speed;
  setSetting("playback_speed", speed);
}

export function setSkip(direction: "back" | "forward", ms: number) {
  setSetting(direction === "back" ? "skip_back_ms" : "skip_forward_ms", ms);
}

// ==============================================================================
// Loading and the contract
// ==============================================================================

function playLocal(next: EpisodeView, stored: Stored) {
  stopCurrent();
  load(next, startPosition(stored));
  begin(stored);
}

function load(next: EpisodeView, atMs: number) {
  show(next, atMs);
  audio.src = next.audioUrl;
  audio.currentTime = atMs / 1000;
  audio.playbackRate = currentPlayerSettings().speed;
}

/** Shows `next` without touching the <audio>: a remote playback, or one this tab can't play until it fetched the episode. */
function show(next: EpisodeView, atMs: number) {
  setError(null);
  setEpisode(next);
  setDurationMs(next.durationMs);
  setPositionMs(atMs);
  updateMediaSession(next);
}

/** Empties the <audio>, so a shown episode it can't play doesn't sit beside the one it last loaded. */
function unload() {
  audio.removeAttribute("src");
  audio.load();
}

function begin(stored: Stored) {
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
    // Autoplay rules can refuse a play that didn't follow a click, such as one
    // another device asked for. The button then simply shows Play again, and
    // the other devices learn that this one waits paused (14.2).
    if (e instanceof DOMException && e.name === "NotAllowedError") {
      report();
      return;
    }
    if (e instanceof DOMException && e.name === "AbortError") return;
    setError("This episode can't be played.");
  }
}

/**
 * Plays the episode the player shows but hasn't played yet: a restored one,
 * or the account's last playback. Its first play starts the contract at the
 * shown spot. One known only from Connect's metadata is fetched first.
 */
async function startShown(now: EpisodeView) {
  let ready = now;
  if (!playable(now)) {
    const found = await findEpisode(now.id, showOf(now)).catch(() => null);
    if (episode() !== now || !local() || contract) return;
    if (!found) {
      setError("This episode can't be played here.");
      return;
    }
    ready = found;
  }
  if (audio.getAttribute("src") !== ready.audioUrl) load(ready, positionMs());
  begin({ positionMs: positionMs(), played: current(ready).played });
}

/** Ends the current play, with a last position save if it was playing; a paused one saved at its pause. */
function stopCurrent() {
  if (contract?.isPlaying && episode()) apply(contract.stop(Date.now()));
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

// While remote the <audio> is paused, but a pause it was given on the way
// there can still report in; the remote playback owns the signals then.

audio.addEventListener("play", () => {
  if (!local()) return;
  setPlaying(true);
  contract?.resume(Date.now());
  report();
});

audio.addEventListener("pause", () => {
  if (!local()) return;
  setPlaying(false);
  if (contract && !audio.ended) apply(contract.pause(Date.now()));
  report();
});

audio.addEventListener("timeupdate", () => {
  if (!local()) return;
  const ms = Math.round(audio.currentTime * 1000);
  setPositionMs(ms);
  if (contract && !audio.paused) apply(contract.progress(ms, Date.now()));
  updatePositionState();
});

audio.addEventListener("durationchange", () => {
  if (!local() || !Number.isFinite(audio.duration)) return;
  const ms = Math.round(audio.duration * 1000);
  setDurationMs(ms);
  contract?.setDuration(ms);
});

// Loading an episode sets the rate too; only a change during a play is news.
audio.addEventListener("ratechange", () => {
  if (local() && contract) report();
});

audio.addEventListener("ended", () => {
  if (!local()) return;
  setPlaying(false);
  if (contract) apply(contract.ended(Date.now()));
  contract = null;
  report();
});

audio.addEventListener("error", () => {
  if (!audio.src || !local()) return;
  setPlaying(false);
  setError("This episode can't be played. The publisher's server may be down.");
});

// ==============================================================================
// Connect: the stream and what arrives on it (spec 14.2)
// ==============================================================================

/** The devices and the playback as the last `state` had them; null without a stream. */
const [connectState, setConnectState] = createSignal<ConnectState | null>(null);

/** The other devices of the account that hold a stream, for the output picker. */
export const otherDevices = () => (connectState()?.devices ?? []).filter((device) => device.id !== ownId());

/** The device the playback is on, or null when nothing plays anywhere (or without a stream). */
export const activeDevice = () => connectState()?.playback?.device_id ?? null;

/** Whether the server accepted the stream, which reports need (14.3). */
let streamOpen = false;
/** The next `state` is the first of its connection. */
let firstState = false;
/** Counts the playbacks shown from the stream, so a failed command doesn't undo a newer one. */
let stateCount = 0;

function onEvent(event: ConnectEvent) {
  switch (event.type) {
    case "state":
      onState(event);
      break;
    case "command":
      onCommand(event.command);
      break;
    case "load":
      void onLoad(event.episode_id, event.position_ms ?? null);
      break;
    case "release":
      onRelease(event.transfer_id ?? null);
      break;
    case "replaced":
      // Another tab of this browser speaks for the device now. This one plays
      // on locally, without Connect, until it is reloaded.
      leaveRemote();
      setConnectState(null);
      break;
  }
}

function onState(state: ConnectState) {
  const first = firstState;
  firstState = false;
  stateCount++;
  setConnectState(state);
  const playback = state.playback ?? null;
  const change = connect.stateChange(playback, { ownId: ownId(), wasRemote: !local(), first, localPlaying: local() && !audio.paused });
  if (change.remote && playback) {
    follow(playback);
  } else {
    if (change.ended) {
      leaveRemote();
      // The device that played wrote the final position and played state.
      const now = episode();
      if (now) void refetchState(now).catch(() => {});
    }
    if (change.adopt && playback) adopt(playback, { ended: change.ended });
    if (change.report) report();
  }
  if (first) void fetchPlayers().catch(() => {});
}

/** A command from another device, run as if this tab's button was pressed (14.2). */
function onCommand(command: ConnectCommand) {
  if (!local()) return;
  const action = connect.localAction(command, {
    episodeId: episode()?.id ?? null,
    playing: !audio.paused,
    positionMs: positionMs(),
    durationMs: durationMs(),
  });
  switch (action.kind) {
    case "pause":
      audio.pause();
      break;
    case "resume":
      togglePlayback();
      break;
    case "seek":
      seek(action.positionMs);
      break;
    case "play":
      void playById(action.episodeId);
      break;
  }
}

async function playById(id: string) {
  const view = await resolve(id);
  if (!view) {
    notify("Another device asked to play an episode this tab can't find.");
    return;
  }
  if (local()) playLocal(view, localStored(view));
}

/** A transfer made this tab the target (14.5): play there, then say so. */
async function onLoad(id: string, positionMs: number | null) {
  // The playback is this device's from now on, even while the episode loads.
  leaveRemote();
  const view = await resolve(id);
  if (!view) {
    notify("Couldn't find the episode to play here.");
    return;
  }
  if (!local()) return;
  playLocal(view, connect.loadStart(positionMs, localStored(view)));
  report();
}

/** Another device took the playback: pause here, with the ordinary pause write, and say where it stood. */
function onRelease(transferId: string | null) {
  if (!local()) return;
  audio.pause();
  report(transferId);
}

/** The latest playback the stream sent, for its episode's metadata and position. */
const lastPlayback = () => connectState()?.playback ?? null;

/** The episode by id, ready to play: the one loaded, one the tab listed, or one fetched. */
async function resolve(id: string): Promise<EpisodeView | null> {
  const now = episode();
  if (now?.id === id && playable(now)) return now;
  const last = lastPlayback();
  const show = last?.episode.id === id ? last.episode : now?.id === id ? showOf(now) : null;
  return findEpisode(id, show).catch(() => null);
}

const showOf = (view: EpisodeView) => ({ podcast_id: view.podcastId, podcast_title: view.podcastTitle, artwork_url: view.artworkUrl });

/**
 * What the start rule of spec 8 starts from for `view` here: a position this
 * tab wrote, else the account's last playback when it is this episode, else
 * the position its list showed.
 */
function localStored(view: EpisodeView): Stored {
  const last = lastPlayback();
  const fromPlayback = last?.episode.id === view.id ? last.position_ms : null;
  return { positionMs: localPosition(view.id) ?? fromPlayback ?? view.positionMs, played: current(view).played };
}

/**
 * Shows the account's last playback, paused (14.7, "Nothing active"): at the
 * position this tab knows for the episode, or the playback's. After a remote
 * ended, the playback's position is the final one.
 */
function adopt(playback: Playback, options: { ended: boolean }) {
  const now = episode();
  const id = playback.episode.id;
  // This tab's own paused play of the episode knows its position best.
  if (now?.id === id && contract) return;
  const view = (now?.id === id ? now : null) ?? knownEpisode(id) ?? fromPlayback(playback.episode);
  const atMs = options.ended ? playback.position_ms : (localPosition(id) ?? playback.position_ms);
  stopCurrent();
  setPlaying(false);
  if (playable(view)) load(view, atMs);
  else {
    show(view, atMs);
    unload();
  }
  remember();
}

// ==============================================================================
// Reports (spec 14.3)
// ==============================================================================

let reportQueued = false;
/** The `transfer_id` the next report answers, sent once. */
let releasing: string | null = null;

/**
 * Tells the server what the local player does. Calls in one task go out as
 * one report, built when it is sent, and while one is out only the newest
 * waits. Reports need the stream; without it this does nothing.
 */
function report(released: string | null = null) {
  if (released) releasing = released;
  if (reportQueued) return;
  reportQueued = true;
  queueMicrotask(() => {
    reportQueued = false;
    sendReport();
  });
}

const sendReport = latestOnly<void>(async () => {
  const body = reportBody();
  if (!body) return;
  releasing = null;
  try {
    await connect.report(body);
  } catch {
    // A `409 no_session` means the stream closed meanwhile, and the next one
    // reports again. The answer isn't needed: the `state` it causes arrives
    // over the stream in order with every other.
  }
});

function reportBody(): connect.ConnectReport | null {
  const now = episode();
  if (!streamOpen || !now || !local()) return null;
  const atMs = contract ? audio.currentTime * 1000 : positionMs();
  return {
    episode_id: now.id,
    position_ms: Math.max(0, Math.round(atMs)),
    playing: !audio.paused,
    speed: audio.playbackRate > 0 ? audio.playbackRate : 1,
    released: releasing,
  };
}

// ==============================================================================
// Remote: another device plays (spec 14.7)
// ==============================================================================

/** When the shown remote playback held, so the clock can move it on. */
let remoteAt = 0;
let clock: ReturnType<typeof setInterval> | undefined;
/** The Sonos group whose name was last looked up, so a state doesn't fetch the list again. */
let lookedUp: string | null = null;

/** Follows `playback`, which another device plays, as its remote. */
function follow(playback: Playback) {
  // The local play ends; the release before this already paused it.
  if (local()) stopCurrent();
  setOutput({ kind: "remote", playback });
  remoteAt = Date.now();
  const id = playback.episode.id;
  if (episode()?.id !== id) show(knownEpisode(id) ?? fromPlayback(playback.episode), playback.position_ms);
  if (playback.episode.duration_ms) setDurationMs(playback.episode.duration_ms);
  setPlaying(playback.playing);
  setPositionMs(playback.position_ms);
  adoptServerState(id, { positionMs: playback.position_ms });
  if (Date.now() - volumeTouchedAt >= VOLUME_HOLD_MS) setVolumeShown(playback.volume ?? null);
  updatePositionState();
  clock ??= setInterval(tick, 500);

  const deviceId = playback.device_id ?? null;
  if (isSonos(deviceId) && deviceId !== lookedUp && !speakers().some((player) => player.id === deviceId)) {
    lookedUp = deviceId;
    void fetchPlayers().catch(() => {});
  }
}

function tick() {
  const out = output();
  if (out.kind !== "remote" || !out.playback.playing) return;
  setPositionMs(connect.positionAt(out.playback, remoteAt, Date.now(), durationMs()));
}

function leaveRemote() {
  clearInterval(clock);
  clock = undefined;
  if (local()) return;
  setOutput(LOCAL);
  setPlaying(false);
  setVolumeShown(null);
}

/** The remote playback as the UI shows it now, for a failed command to go back to. */
type Shown = { episode: EpisodeView | null; playing: boolean; positionMs: number };
const shown = (): Shown => ({ episode: episode(), playing: playing(), positionMs: positionMs() });

/** Shows the remote playback at `atMs`, playing or not, before the device confirms it. */
function showRemote(isPlaying: boolean, atMs: number) {
  const out = output();
  if (out.kind !== "remote") return;
  setOutput({ kind: "remote", playback: { ...out.playback, playing: isPlaying, position_ms: Math.round(atMs) } });
  remoteAt = Date.now();
  setPlaying(isPlaying);
  setPositionMs(atMs);
  updatePositionState();
}

/** Asks the device that plays to start `next` there; it applies the start rule itself. */
function playRemote(next: EpisodeView) {
  const before = shown();
  const atMs = startPosition(current(next));
  show(next, atMs);
  showRemote(true, atMs);
  void send({ type: "play", episode_id: next.id }, before);
}

/**
 * Sends a command the UI already shows. If it fails before a newer state
 * arrived, the playback shows as it was, since the device never heard it.
 * When the device is gone (`409 no_session`) the snapshot says what is left.
 */
async function send(body: ConnectCommand, before: Shown) {
  const sent = stateCount;
  try {
    await connect.command(body);
  } catch (error) {
    if (local() || stateCount !== sent) return;
    if (before.episode && episode()?.id !== before.episode.id) show(before.episode, before.positionMs);
    showRemote(before.playing, before.positionMs);
    if (error instanceof ApiError && error.code === "no_session") void refresh();
    else notify(failure(activeName() ?? "the other device", error));
  }
}

/** Reads the playback once outside the stream and shows it like a `state`. */
async function refresh() {
  const state = await connect.snapshot().catch(() => null);
  if (state && streamOpen) onState(state);
}

/**
 * How long after a volume change the stream leaves the slider alone. A state
 * sent before the change landed would otherwise jump it back for a moment.
 */
const VOLUME_HOLD_MS = 3_000;
let volumeTouchedAt = 0;

/** Sets the volume of the Sonos group that plays. */
export function setVolume(level: number) {
  const out = output();
  if (out.kind !== "remote" || out.playback.volume == null) return;
  const clamped = Math.max(0, Math.min(100, Math.round(level)));
  setVolumeShown(clamped);
  volumeTouchedAt = Date.now();
  sendVolume(clamped);
}

const sendVolume = latestOnly(async (level: number) => {
  if (local()) return;
  try {
    await connect.command({ type: "volume", volume: level });
  } catch (error) {
    if (local()) return;
    // The next state shows the volume the group really has.
    volumeTouchedAt = 0;
    if (error instanceof ApiError && error.code === "no_session") void refresh();
    else notify(failure(activeName() ?? "the speaker", error));
  }
});

// ==============================================================================
// Outputs: the devices and Sonos groups to move the playback to (spec 14.5)
// ==============================================================================

/** The Sonos groups as last fetched. Empty with Sonos off on the server. */
const [speakers, setSpeakers] = createSignal<SonosPlayer[]>([]);
/** The name of the output a transfer goes to while it is under way. */
const [transferring, setTransferring] = createSignal<string | null>(null);

export { speakers, transferring };

export async function fetchPlayers(): Promise<SonosPlayer[]> {
  const players = await connect.listPlayers();
  setSpeakers(players);
  return players;
}

/** What the active device is called, while another one plays. */
export function activeName(): string | null {
  const out = output();
  const id = out.kind === "remote" ? out.playback.device_id : null;
  if (!id) return null;
  const name = connectState()?.devices.find((device) => device.id === id)?.name ?? speakers().find((player) => player.id === id)?.name;
  return name ?? (isSonos(id) ? "a speaker" : "another device");
}

/**
 * Moves the playback to `target`, a device id or a Sonos group, and plays it
 * there. This device as the target plays here, which claims the playback.
 */
export async function transferTo(target: { id: string; name: string }) {
  if (target.id === ownId() && local()) {
    if (!playing()) togglePlayback();
    return;
  }
  if (transferring()) return;
  setTransferring(target.name);
  try {
    const body = local() ? reportBody() : null;
    if (body) {
      // The server moves its own idea of the playback, so it learns the
      // episode this tab shows first, and gets the writes behind it for a
      // target that starts by the start rule.
      await flushWrites();
      await connect.report(body).catch(() => {});
    }
    await connect.transfer(target.id);
  } catch (error) {
    notify(failure(target.name, error));
  } finally {
    setTransferring(null);
  }
}

/** A `502` is the speaker; `no_session` means nothing played on the account yet; anything else says what it is. */
function failure(name: string, error: unknown): string {
  if (error instanceof ApiError && error.status === 502) return `Couldn't reach ${name}.`;
  if (error instanceof ApiError && error.code === "no_session") return "Nothing to play yet.";
  return error instanceof ApiError ? error.message : `Couldn't reach ${name}.`;
}

// ==============================================================================
// The stream's lifetime
// ==============================================================================

createRoot(() => {
  createEffect(() => {
    const token = session()?.token;
    if (!token) return;
    let periodic: ReturnType<typeof setInterval> | undefined;
    const closed = () => {
      streamOpen = false;
      clearInterval(periodic);
    };
    const stop = holdStream(token, {
      onOpen: () => {
        streamOpen = true;
        firstState = true;
        periodic = setInterval(() => local() && !audio.paused && report(), connect.REPORT_EVERY_MS);
      },
      onEvent,
      onClose: closed,
      onUnauthorized: () => expire(token),
    });
    onCleanup(() => {
      stop();
      closed();
      setConnectState(null);
    });
  });
});

// ==============================================================================
// Leaving the page and coming back
// ==============================================================================

// The tab is hidden before it can be closed, and a hidden phone tab can be
// killed without another event. Saving here, while requests still go out,
// keeps the position the next device sees exact.
document.addEventListener("visibilitychange", () => {
  // A paused play saved at its pause; saving it again would only restamp an
  // old position over whatever another device wrote since.
  if (document.visibilityState !== "hidden" || !contract?.isPlaying || !episode()) return;
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
  if (!key || !now || !playable(now) || !local()) return;
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
    // Only the user counts: what the steps below read mustn't run this again.
    untrack(() => {
      leaveRemote();
      setSpeakers([]);
      lookedUp = null;
      stopCurrent();
      setEpisode(null);
      unload();
      if (!key) return;
      try {
        const saved: Saved | null = JSON.parse(localStorage.getItem(key) ?? "null");
        if (saved?.episode?.audioUrl) load(saved.episode, saved.positionMs);
      } catch {
        // A broken entry is as good as none.
      }
    });
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
  const out = output();
  try {
    navigator.mediaSession.setPositionState({
      duration: duration / 1000,
      position: Math.min(positionMs(), duration) / 1000,
      playbackRate: out.kind === "remote" ? out.playback.speed : audio.playbackRate,
    });
  } catch {
    // A position past a duration the element revised; the next update fixes it.
  }
}

// The handlers call the same commands as the buttons, so the keys steer
// whichever device plays (13.4).
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

// A synced speed change from Settings applies to the episode playing here.
createRoot(() => {
  createEffect(() => {
    const speed = currentPlayerSettings().speed;
    if (local()) audio.playbackRate = speed;
  });
});
