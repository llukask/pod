// ==============================================================================
// Connect: playback across the account's devices (spec 14)
// ==============================================================================
//
// The server keeps one playback per account and names the device it plays on.
// This module holds the requests around it and the pure decisions the player
// makes from what the event stream says: whether this tab is a remote, where a
// remote episode stands by now, what a `state` changes here, and what a
// command from another device does to the local player. player.ts owns the
// signals and the <audio>; stream.ts holds the connection.

import type { components } from "../api/schema";
import { api } from "../session";
import type { Stored } from "./contract";

type Schemas = components["schemas"];
export type Playback = Schemas["Playback"];
export type PlaybackEpisode = Schemas["PlaybackEpisode"];
export type ConnectState = Schemas["ConnectState"];
export type ConnectEvent = Schemas["ConnectEvent"];
export type ConnectCommand = Schemas["ConnectCommand"];
export type ConnectReport = Schemas["ConnectReport"];
export type ConnectDevice = Schemas["ConnectDevice"];
export type SonosPlayer = Schemas["SonosPlayer"];

/** How often a playing device reports while nothing else makes it (14.3). */
export const REPORT_EVERY_MS = 15_000;

// ==============================================================================
// Requests
// ==============================================================================

/** Every Sonos group of the household (13.2). Empty when the server has Sonos off. */
export async function listPlayers(): Promise<SonosPlayer[]> {
  return (await api<{ players: SonosPlayer[] }>("GET", "/v1/sonos/players")).players;
}

/** The devices and the playback once, outside the stream (14.6). */
export function snapshot(): Promise<ConnectState> {
  return api<ConnectState>("GET", "/v1/connect");
}

export function report(body: ConnectReport): Promise<Playback> {
  return api<Playback>("POST", "/v1/connect/report", { body });
}

/**
 * Steers the active device (14.4). A Sonos group answers with the result and a
 * client device before it acted, so the answer isn't shown: the `state` the
 * change causes comes over the stream either way.
 */
export function command(body: ConnectCommand): Promise<Playback> {
  return api<Playback>("POST", "/v1/connect/command", { body });
}

/** Moves the playback to a client device or a Sonos group and plays it there (14.5). */
export function transfer(deviceId: string): Promise<Playback> {
  return api<Playback>("POST", "/v1/connect/transfer", { body: { device_id: deviceId } });
}

// ==============================================================================
// Decisions
// ==============================================================================

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Whether `deviceId` is a Sonos group. Client devices are UUIDs and group ids (`RINCON_…`) can't be (14.1). */
export function isSonos(deviceId: string | null | undefined): boolean {
  return !!deviceId && !UUID.test(deviceId);
}

/** Whether another device plays the account's playback, which makes this one its remote (14.3). */
export function isRemote(playback: Playback | null | undefined, ownId: string): boolean {
  return !!playback?.device_id && playback.device_id !== ownId;
}

/**
 * Where a remote episode stands at `now`, from the playback as it arrived at
 * `at`: it moves on by the elapsed time times the speed while playing, so the
 * clocks of the two devices never need to agree (14.1). It stops at the
 * duration, where a playing device stops too.
 */
export function positionAt(playback: Pick<Playback, "position_ms" | "playing" | "speed">, at: number, now: number, durationMs: number | null): number {
  if (!playback.playing) return playback.position_ms;
  const moved = playback.position_ms + Math.max(0, now - at) * playback.speed;
  return Math.min(moved, durationMs ?? Infinity);
}

/** What a `state` from the stream means for this tab. */
export type StateChange = {
  /** Follow the playback as the remote of the device that plays it. */
  remote: boolean;
  /** This tab was a remote and the other device stopped: pull the final state (14.7, "Remote ended"). */
  ended: boolean;
  /** Show the account's last playback, paused, in place of what the local player shows. */
  adopt: boolean;
  /** Tell the server what the local player does. */
  report: boolean;
};

/**
 * Decides what a `state` changes here (14.7).
 *
 * - Another device plays: this tab is its remote. While the local player
 *   plays it isn't, though: a device that takes the playback over sends this
 *   one a `release` first, which pauses it. A state that names another device
 *   while this one plays was sent before this tab's own claim landed.
 * - Nothing plays any more after this tab was a remote: the remote ended, and
 *   the last playback shows here, paused.
 * - The first state of a connection replaces what the local player restored
 *   with the account's last playback, unless the local player plays. With
 *   nothing active that is the cold-start rule; when the playback still names
 *   this device, it is this tab come back after a reload, which no longer
 *   plays, and it reports so.
 * - The first state of a connection while the local player plays sends a
 *   report, which claims the playback again (14.1, "Device gone").
 */
export function stateChange(playback: Playback | null | undefined, here: { ownId: string; wasRemote: boolean; first: boolean; localPlaying: boolean }): StateChange {
  const remote = isRemote(playback, here.ownId) && !here.localPlaying;
  const ended = here.wasRemote && !remote;
  const adopt = !remote && !!playback && !here.localPlaying && (ended || here.first);
  const own = !!playback && playback.device_id === here.ownId;
  const report = !remote && here.first && (here.localPlaying || own);
  return { remote, ended, adopt, report };
}

/**
 * Where a `load` starts (14.2): at the position it names, which isn't a
 * replay, or with a null position by the start rule of spec 8 over what this
 * tab knows of the episode, a replay included.
 */
export function loadStart(positionMs: number | null | undefined, local: Stored): Stored {
  return positionMs === null || positionMs === undefined ? local : { positionMs, played: false };
}

/** What a command from another device does to the local player. */
export type LocalAction =
  | { kind: "pause" }
  | { kind: "resume" }
  | { kind: "seek"; positionMs: number }
  | { kind: "play"; episodeId: string }
  /** Nothing to do: the player is there already, or the command is for a Sonos group. */
  | { kind: "none" };

/**
 * Maps a command that arrived over the stream to what a press of the matching
 * button does here (14.2). A skip goes from the local position, clamped to the
 * episode, and `play` of the episode already loaded resumes it.
 */
export function localAction(command: ConnectCommand, here: { episodeId: string | null; playing: boolean; positionMs: number; durationMs: number | null }): LocalAction {
  switch (command.type) {
    case "pause":
      return here.playing ? { kind: "pause" } : { kind: "none" };
    case "resume":
      return here.playing || here.episodeId === null ? { kind: "none" } : { kind: "resume" };
    case "seek":
    case "skip": {
      if (here.episodeId === null) return { kind: "none" };
      const target = command.type === "seek" ? command.position_ms : here.positionMs + command.delta_ms;
      return { kind: "seek", positionMs: Math.max(0, Math.min(target, here.durationMs ?? Infinity)) };
    }
    case "play":
      if (command.episode_id !== here.episodeId) return { kind: "play", episodeId: command.episode_id };
      return here.playing ? { kind: "none" } : { kind: "resume" };
    case "volume":
      return { kind: "none" };
  }
}

// ==============================================================================
// Timing
// ==============================================================================

/** The first and the longest wait before the stream reconnects (14.2). */
export const RECONNECT_FIRST_MS = 1_000;
export const RECONNECT_MAX_MS = 30_000;

/** The reconnect delay: 1 s, doubling to 30 s, back to 1 s once a `state` arrives. */
export class Backoff {
  private nextMs = RECONNECT_FIRST_MS;

  /** The wait before the next attempt, which doubles the one after it. */
  next(): number {
    const wait = this.nextMs;
    this.nextMs = Math.min(this.nextMs * 2, RECONNECT_MAX_MS);
    return wait;
  }

  reset() {
    this.nextMs = RECONNECT_FIRST_MS;
  }
}

/**
 * Wraps `send` so its calls go out one at a time, and while one is out only
 * the newest value waits. A dragged slider then sends the value the user ends
 * on, without a request for every step. `send` must not reject.
 */
export function latestOnly<T>(send: (value: T) => Promise<void>): (value: T) => void {
  let pending: { value: T } | null = null;
  let busy = false;
  const drain = async () => {
    busy = true;
    while (pending) {
      const { value } = pending;
      pending = null;
      await send(value);
    }
    busy = false;
  };
  return (value) => {
    pending = { value };
    if (!busy) void drain();
  };
}
