// ==============================================================================
// The playback contract (spec 8) as a pure state machine
// ==============================================================================
//
// What the player reports in (progress, pause, seek, stop, the end of the file)
// and the `episode_state` writes that follow. The clock comes in with every
// call, so tests drive it with made-up instants and no audio element. A port of
// Android's `Contract.kt`, itself a port of pod-desktop's `player/contract.rs`.
//
// One `Contract` covers one play of one episode, from the moment it starts until
// it stops, changes or ends.

/** How often a playing episode saves its position (spec 8). */
export const SAVE_EVERY_MS = 5_000;

/** A write to the playing episode's state. `whilePlaying` marks the periodic saves, which may wait for a later request. */
export type Write = { kind: "position"; positionMs: number; whilePlaying: boolean } | { kind: "played"; played: boolean };

/** The episode's state as stored when playback starts. */
export type Stored = { positionMs: number; played: boolean };

/** Where playback starts: the stored position, or the beginning of a played episode. */
export function startPosition(stored: Stored): number {
  return stored.played ? 0 : stored.positionMs;
}

export class Contract {
  isPlaying = true;
  /** Whether this play wrote `played = true`, which it does once. */
  private markedPlayed = false;

  private constructor(
    /** `played_threshold_ms` from the synced settings. */
    private readonly thresholdMs: number,
    public durationMs: number | null,
    public positionMs: number,
    /** When the last position write was made, or playback last resumed. */
    private savedAt: number,
  ) {}

  /**
   * Starts playing at `startPosition(stored)`. Replaying a played episode
   * writes `played = false` as its own mutation, so coalescing position saves
   * can never lose the reopen.
   */
  static start(stored: Stored, durationMs: number | null, thresholdMs: number, now: number): [Contract, Write[]] {
    const contract = new Contract(thresholdMs, durationMs, startPosition(stored), now);
    return [contract, stored.played ? [{ kind: "played", played: false }] : []];
  }

  /** The player's own duration, which beats the feed's. */
  setDuration(durationMs: number) {
    this.durationMs = durationMs;
  }

  /** The player reports where it is. Saves the position every `SAVE_EVERY_MS` while playing. */
  progress(positionMs: number, now: number): Write[] {
    this.positionMs = positionMs;
    const writes = this.checkPlayed();
    if (this.isPlaying && now - this.savedAt >= SAVE_EVERY_MS) writes.push(this.save(true, now));
    return writes;
  }

  pause(now: number): Write[] {
    if (!this.isPlaying) return [];
    this.isPlaying = false;
    return [this.save(false, now)];
  }

  resume(now: number) {
    if (this.isPlaying) return;
    this.isPlaying = true;
    this.savedAt = now;
  }

  seek(positionMs: number, now: number): Write[] {
    this.positionMs = Math.max(positionMs, 0);
    const writes = this.checkPlayed();
    writes.push(this.save(false, now));
    return writes;
  }

  /** Stop, episode change or the page going away: the last position write of this play. */
  stop(now: number): Write[] {
    return [this.save(false, now)];
  }

  /** The player reached the end of the file. Marks the episode played if the threshold didn't, which happens without a known duration. */
  ended(now: number): Write[] {
    if (this.durationMs !== null) this.positionMs = Math.max(this.positionMs, this.durationMs);
    const writes: Write[] = this.markedPlayed ? [] : [{ kind: "played", played: true }];
    this.markedPlayed = true;
    writes.push(this.save(false, now));
    return writes;
  }

  private save(whilePlaying: boolean, now: number): Write {
    this.savedAt = now;
    return { kind: "position", positionMs: this.positionMs, whilePlaying };
  }

  /** `played = true`, once, when the remaining time drops below `played_threshold_ms` or 2 % of the duration, whichever is smaller. */
  private checkPlayed(): Write[] {
    if (this.durationMs === null) return [];
    const limit = Math.min(this.thresholdMs, this.durationMs / 50);
    if (this.markedPlayed || this.durationMs - this.positionMs >= limit) return [];
    this.markedPlayed = true;
    return [{ kind: "played", played: true }];
  }
}
