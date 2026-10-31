// ==============================================================================
// Sonos groups as an output of the player (spec 13.4)
// ==============================================================================
//
// pod-server drives the speakers and writes listening state for a session it
// plays (13.3), so the browser only asks it to start, steer and release one,
// and polls the player list to show the session. This module holds those
// requests, the polling cadence and the pure decisions around them; player.ts
// owns the output mode and the signals the UI reads.

import type { components } from "../api/schema";
import { api } from "../session";
import { type Stored, startPosition } from "./contract";

type Schemas = components["schemas"];
export type SonosPlayer = Schemas["SonosPlayer"];
export type SonosSession = Schemas["SonosSessionView"];
export type SonosCommand = Schemas["SonosCommand"];
export type SonosReleased = Schemas["SonosReleased"];

/** How often the session is polled while the tab is visible, and while it is hidden (13.4). */
export const POLL_VISIBLE_MS = 2_000;
export const POLL_HIDDEN_MS = 15_000;

// ==============================================================================
// Requests
// ==============================================================================

const playerPath = (playerId: string) => `/v1/sonos/players/${encodeURIComponent(playerId)}`;

/** Every group of the household. Empty when the server has Sonos off, which hides it. */
export async function listPlayers(): Promise<SonosPlayer[]> {
  return (await api<{ players: SonosPlayer[] }>("GET", "/v1/sonos/players")).players;
}

export function playOn(playerId: string, episodeId: string, positionMs: number): Promise<SonosPlayer> {
  return api<SonosPlayer>("POST", `${playerPath(playerId)}/play`, { body: { episode_id: episodeId, position_ms: Math.round(positionMs) } });
}

export function command(playerId: string, body: SonosCommand): Promise<SonosPlayer> {
  return api<SonosPlayer>("POST", `${playerPath(playerId)}/command`, { body });
}

export function release(playerId: string): Promise<SonosReleased> {
  return api<SonosReleased>("POST", `${playerPath(playerId)}/release`);
}

// ==============================================================================
// Decisions
// ==============================================================================

/**
 * Where a hand-off starts, by the start rule of spec 8. A played episode
 * starts over, and `unplay` says to write `played = false` first, as a local
 * start would.
 */
export function handOffStart(stored: Stored): { positionMs: number; unplay: boolean } {
  return { positionMs: startPosition(stored), unplay: stored.played };
}

/** What a player list says about the session the player shows. */
export type Observation =
  /** The user's session still plays `episodeId` on `player`. */
  | { kind: "session"; player: SonosPlayer; session: SonosSession }
  /** The session is over: the episode ended, or the group plays something else. */
  | { kind: "ended" }
  /** The player is missing from the list. The server leaves out a group that didn't answer this time. */
  | { kind: "missing" };

/**
 * Reads the session for `episodeId` out of a player list: on `playerId`, or on
 * any player when that is null (restoring after a reload). A session of
 * another episode counts as ended, since the player has nothing to show for it.
 */
export function observe(players: SonosPlayer[], episodeId: string, playerId: string | null): Observation {
  const candidates = playerId === null ? players : players.filter((player) => player.id === playerId);
  if (candidates.length === 0 && playerId !== null) return { kind: "missing" };
  for (const player of candidates) {
    if (player.state === "pod" && player.session?.episode_id === episodeId) return { kind: "session", player, session: player.session };
  }
  return { kind: "ended" };
}

// ==============================================================================
// Polling
// ==============================================================================

/**
 * Runs `tick` every `POLL_VISIBLE_MS` while the tab is visible and every
 * `POLL_HIDDEN_MS` while it is hidden, and at once when it comes back into
 * view. One tick runs at a time. Returns the function that stops it.
 */
export function poll(tick: () => Promise<void>): () => void {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let running = false;
  let stopped = false;

  const schedule = () => {
    clearTimeout(timer);
    if (!stopped) timer = setTimeout(run, document.visibilityState === "visible" ? POLL_VISIBLE_MS : POLL_HIDDEN_MS);
  };
  const run = async () => {
    if (running || stopped) return;
    running = true;
    try {
      await tick();
    } catch {
      // A failed poll changes nothing; the next one tries again.
    } finally {
      running = false;
      schedule();
    }
  };
  const onVisibility = () => {
    if (document.visibilityState === "visible") void run();
    else schedule();
  };

  document.addEventListener("visibilitychange", onVisibility);
  schedule();
  return () => {
    stopped = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVisibility);
  };
}
