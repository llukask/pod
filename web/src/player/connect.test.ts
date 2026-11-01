import { expect, test, vi } from "vitest";
import { Backoff, type Playback, RECONNECT_MAX_MS, isRemote, isSonos, latestOnly, loadStart, localAction, positionAt, stateChange } from "./connect";

const ME = "0192f000-0000-7000-8000-000000000001";
const PHONE = "0192f000-0000-7000-8000-000000000002";
const COUCH = "RINCON_804AF246458401400";
const EPISODE = "4b1e0000-0000-0000-0000-000000000001";
const OTHER_EPISODE = "4b1e0000-0000-0000-0000-000000000002";

const playback = (deviceId: string | null, fields: Partial<Playback> = {}): Playback => ({
  device_id: deviceId,
  episode: { id: EPISODE, podcast_id: "5a0f3c1e-2b4d-4e6f-8a1b-3c5d7e9f0001", title: "Episode", podcast_title: "The Show", duration_ms: 3_600_000 },
  position_ms: 683_000,
  playing: true,
  speed: 1,
  volume: null,
  ...fields,
});

// ==============================================================================
// Who plays
// ==============================================================================

test("client devices are UUIDs and Sonos groups aren't", () => {
  expect(isSonos(COUCH)).toBe(true);
  expect(isSonos(PHONE)).toBe(false);
  expect(isSonos(null)).toBe(false);
  expect(isSonos(undefined)).toBe(false);
});

test("this tab is a remote exactly while another device is active", () => {
  expect(isRemote(playback(PHONE), ME)).toBe(true);
  expect(isRemote(playback(COUCH), ME)).toBe(true);
  expect(isRemote(playback(ME), ME)).toBe(false);
  expect(isRemote(playback(null, { playing: false }), ME)).toBe(false);
  expect(isRemote(null, ME)).toBe(false);
});

// ==============================================================================
// The clock between updates
// ==============================================================================

test("a playing remote moves on by the elapsed time times its speed", () => {
  const at = 1_000_000;
  expect(positionAt(playback(PHONE), at, at, null)).toBe(683_000);
  expect(positionAt(playback(PHONE), at, at + 2_000, null)).toBe(685_000);
  expect(positionAt(playback(PHONE, { speed: 1.5 }), at, at + 2_000, null)).toBe(686_000);
});

test("a paused remote stays put, and a playing one stops at the duration", () => {
  const at = 1_000_000;
  expect(positionAt(playback(PHONE, { playing: false }), at, at + 60_000, null)).toBe(683_000);
  expect(positionAt(playback(PHONE), at, at + 60_000, 700_000)).toBe(700_000);
});

test("a clock that runs behind the arrival time doesn't move the position back", () => {
  expect(positionAt(playback(PHONE), 1_000_000, 999_000, null)).toBe(683_000);
});

// ==============================================================================
// What a state changes
// ==============================================================================

const here = { ownId: ME, wasRemote: false, first: false, localPlaying: false };

test("another device playing makes this tab its remote", () => {
  expect(stateChange(playback(PHONE), here)).toEqual({ remote: true, ended: false, adopt: false, report: false });
  expect(stateChange(playback(COUCH), { ...here, wasRemote: true }).remote).toBe(true);
});

test("a state naming another device doesn't stop the local player, which a release would have paused first", () => {
  expect(stateChange(playback(PHONE), { ...here, localPlaying: true })).toEqual({ remote: false, ended: false, adopt: false, report: false });
});

test("a remote whose device stopped ends, and the last playback shows here paused", () => {
  expect(stateChange(playback(null, { playing: false }), { ...here, wasRemote: true })).toEqual({ remote: false, ended: true, adopt: true, report: false });
});

test("a remote ends when the playback comes to this device too", () => {
  expect(stateChange(playback(ME), { ...here, wasRemote: true })).toMatchObject({ remote: false, ended: true });
});

test("the first state replaces the restored episode with the last playback, unless the local player plays", () => {
  const idle = playback(null, { playing: false });
  expect(stateChange(idle, { ...here, first: true })).toEqual({ remote: false, ended: false, adopt: true, report: false });
  expect(stateChange(idle, { ...here, first: true, localPlaying: true })).toEqual({ remote: false, ended: false, adopt: false, report: true });
});

test("later states with nothing active leave the local player alone", () => {
  expect(stateChange(playback(null, { playing: false }), here)).toEqual({ remote: false, ended: false, adopt: false, report: false });
  expect(stateChange(null, here)).toEqual({ remote: false, ended: false, adopt: false, report: false });
});

test("states this tab's own reports cause change nothing", () => {
  expect(stateChange(playback(ME), { ...here, localPlaying: true })).toEqual({ remote: false, ended: false, adopt: false, report: false });
});

test("a reconnect while playing reports at once to claim the playback again", () => {
  expect(stateChange(playback(ME), { ...here, first: true, localPlaying: true }).report).toBe(true);
  expect(stateChange(null, { ...here, first: true, localPlaying: true }).report).toBe(true);
});

test("a tab reloaded while the server still names its device shows the playback and reports itself paused", () => {
  expect(stateChange(playback(ME), { ...here, first: true })).toEqual({ remote: false, ended: false, adopt: true, report: true });
});

test("no playback at all on a first state changes nothing", () => {
  expect(stateChange(null, { ...here, first: true })).toEqual({ remote: false, ended: false, adopt: false, report: false });
});

// ==============================================================================
// Loads and commands from other devices
// ==============================================================================

test("a load with a position starts there, which isn't a replay", () => {
  expect(loadStart(101_500, { positionMs: 3_000_000, played: true })).toEqual({ positionMs: 101_500, played: false });
  expect(loadStart(0, { positionMs: 42_000, played: false })).toEqual({ positionMs: 0, played: false });
});

test("a load without a position goes by the local state, a replay included", () => {
  expect(loadStart(null, { positionMs: 42_000, played: false })).toEqual({ positionMs: 42_000, played: false });
  expect(loadStart(undefined, { positionMs: 3_000_000, played: true })).toEqual({ positionMs: 3_000_000, played: true });
});

const player = { episodeId: EPISODE, playing: true, positionMs: 60_000, durationMs: 600_000 };

test("pause and resume act only when they change something", () => {
  expect(localAction({ type: "pause" }, player)).toEqual({ kind: "pause" });
  expect(localAction({ type: "pause" }, { ...player, playing: false })).toEqual({ kind: "none" });
  expect(localAction({ type: "resume" }, { ...player, playing: false })).toEqual({ kind: "resume" });
  expect(localAction({ type: "resume" }, player)).toEqual({ kind: "none" });
  expect(localAction({ type: "resume" }, { ...player, episodeId: null, playing: false })).toEqual({ kind: "none" });
});

test("seeks and skips land inside the episode", () => {
  expect(localAction({ type: "seek", position_ms: 90_000 }, player)).toEqual({ kind: "seek", positionMs: 90_000 });
  expect(localAction({ type: "seek", position_ms: 900_000 }, player)).toEqual({ kind: "seek", positionMs: 600_000 });
  expect(localAction({ type: "skip", delta_ms: -15_000 }, player)).toEqual({ kind: "seek", positionMs: 45_000 });
  expect(localAction({ type: "skip", delta_ms: -90_000 }, player)).toEqual({ kind: "seek", positionMs: 0 });
  expect(localAction({ type: "skip", delta_ms: 30_000 }, { ...player, durationMs: null })).toEqual({ kind: "seek", positionMs: 90_000 });
  expect(localAction({ type: "skip", delta_ms: 30_000 }, { ...player, episodeId: null })).toEqual({ kind: "none" });
});

test("play starts another episode, and resumes the loaded one", () => {
  expect(localAction({ type: "play", episode_id: OTHER_EPISODE }, player)).toEqual({ kind: "play", episodeId: OTHER_EPISODE });
  expect(localAction({ type: "play", episode_id: OTHER_EPISODE }, { ...player, episodeId: null })).toEqual({ kind: "play", episodeId: OTHER_EPISODE });
  expect(localAction({ type: "play", episode_id: EPISODE }, { ...player, playing: false })).toEqual({ kind: "resume" });
  expect(localAction({ type: "play", episode_id: EPISODE }, player)).toEqual({ kind: "none" });
});

test("volume is for Sonos groups and does nothing here", () => {
  expect(localAction({ type: "volume", volume: 20 }, player)).toEqual({ kind: "none" });
});

// ==============================================================================
// Timing
// ==============================================================================

test("reconnects wait 1 s, doubling to 30 s, and start over after a state", () => {
  const backoff = new Backoff();
  expect(Array.from({ length: 7 }, () => backoff.next())).toEqual([1_000, 2_000, 4_000, 8_000, 16_000, RECONNECT_MAX_MS, RECONNECT_MAX_MS]);
  backoff.reset();
  expect(backoff.next()).toBe(1_000);
});

test("a dragged volume sends one request at a time and ends on the last value", async () => {
  const sent: number[] = [];
  const answers: (() => void)[] = [];
  const send = latestOnly((value: number) => {
    sent.push(value);
    return new Promise<void>((resolve) => answers.push(resolve));
  });

  send(10);
  send(20);
  send(30);
  expect(sent).toEqual([10]);

  answers.shift()!();
  await vi.waitFor(() => expect(sent).toEqual([10, 30]));
  answers.shift()!();
  await Promise.resolve();
  expect(sent).toEqual([10, 30]);

  send(40);
  expect(sent).toEqual([10, 30, 40]);
});
