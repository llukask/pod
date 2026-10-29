import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import { NS_EPISODE, NS_PODCAST, sha1, uuidv5, uuidv7 } from "./ids";

// The fixture the server and the other clients run (spec 9).
const fixture = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../conformance/ids.json"), "utf8"));

const hex = (bytes: Uint8Array) => Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
const utf8 = (text: string) => new TextEncoder().encode(text);

test("SHA-1 matches the FIPS 180 vectors", () => {
  expect(hex(sha1(utf8("")))).toBe("da39a3ee5e6b4b0d3255bfef95601890afd80709");
  expect(hex(sha1(utf8("abc")))).toBe("a9993e364706816aba3e25717850c26c9cd0d89d");
  expect(hex(sha1(utf8("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")))).toBe(
    "84983e441c3bd26ebaae4aa1f95129e5e54670f1",
  );
  expect(hex(sha1(utf8("a".repeat(1_000_000))))).toBe("34aa973cd4c4daa4f61eeb2bdbad27316534016f");
});

test("the namespaces are the spec's", () => {
  expect({ podcast: NS_PODCAST, episode: NS_EPISODE }).toEqual(fixture.namespaces);
});

describe("UUIDv5 matches Python's uuid.uuid5 (conformance/ids.json)", () => {
  test.each(fixture.feed_urls as { name: string; normalized: string; podcast_id: string }[])("podcast id: $name", (c) => {
    expect(uuidv5(NS_PODCAST, c.normalized)).toBe(c.podcast_id);
  });
  test.each(fixture.episode_ids as { name: string; podcast_id: string; key: string; episode_id: string }[])("episode id: $name", (c) => {
    expect(uuidv5(NS_EPISODE, `${c.podcast_id}\n${c.key}`)).toBe(c.episode_id);
  });
});

test("UUIDv7 carries the time, version 7 and the RFC 9562 variant", () => {
  const id = uuidv7(Date.parse("2026-10-29T12:00:00.000Z"));
  expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  expect(parseInt(id.replace(/-/g, "").slice(0, 12), 16)).toBe(Date.parse("2026-10-29T12:00:00.000Z"));
  expect(uuidv7()).not.toBe(uuidv7());
});
