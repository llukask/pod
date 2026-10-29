import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import { classify, normalizeFeedUrl } from "./feedUrl";

// The fixture the server and the other clients run (spec 9).
const fixture = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../conformance/ids.json"), "utf8"));

type FeedCase = { name: string; input: string; feed_url: string; normalized: string; podcast_id: string };
type RejectedCase = { name: string; input: string; error: string };

describe("feed URL normalization (conformance/ids.json)", () => {
  test.each(fixture.feed_urls as FeedCase[])("$name", (c) => {
    expect(normalizeFeedUrl(c.input)).toEqual({ ok: true, feedUrl: c.feed_url, normalized: c.normalized, podcastId: c.podcast_id });
  });
  test.each(fixture.rejected_feed_urls as RejectedCase[])("rejects $name", (c) => {
    expect(normalizeFeedUrl(c.input)).toEqual({ ok: false, error: c.error });
  });
});

test("a scheme or a host with a path is a feed URL, anything else searches", () => {
  expect(classify("  ")).toEqual({ kind: "empty" });
  expect(classify("syntax")).toEqual({ kind: "terms", terms: "syntax" });
  expect(classify("syntax.fm"), "a bare domain finds the show by name").toEqual({ kind: "terms", terms: "syntax.fm" });
  expect(classify("darknet diaries")).toEqual({ kind: "terms", terms: "darknet diaries" });
  expect(classify("feeds.example.com/show.xml")).toMatchObject({ kind: "feed", url: { ok: true, feedUrl: "https://feeds.example.com/show.xml" } });
  expect(classify("http://example.com/feed")).toMatchObject({ kind: "feed", url: { ok: true, feedUrl: "http://example.com/feed" } });
  expect(classify("feed://example.com/rss"), "reaches the normalizer for its error").toMatchObject({
    kind: "feed",
    url: { ok: false, error: "unsupported_scheme" },
  });
});
