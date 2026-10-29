import { NS_PODCAST, uuidv5 } from "./ids";

// ==============================================================================
// Feed URLs (spec 1)
// ==============================================================================
//
// The platform's URL class is a WHATWG parser, which is what the spec names,
// and `conformance/ids.json` holds it to the Rust `url` crate the server uses.
// Parsers differ in a few places the fixture covers, and each is evened out
// here rather than trusted:
//
// - `^` and `|` in paths: the crate keeps both. Node and jsdom encode `^` as
//   current WHATWG does; Chromium encodes `|`. Each raw one is swapped for a
//   private-use character before parsing, which every parser encodes alike,
//   and swapped back after. In credentials WHATWG and the crate both encode
//   them, so there they become `%5E` and `%7C`.
// - Forbidden host characters: WHATWG rejects a host with a space (or any
//   other forbidden code point), Chromium percent-encodes it. A valid host
//   never holds `%`, so one in the parsed host is `invalid_url`.
//
// The fixture runs in Node with jsdom (`feedUrl.test.ts`).
//
// TODO: run it in real browsers in CI (Vitest browser mode). Chromium passes
// it when run by hand; Firefox and Safari haven't been checked.

export type FeedUrlError = "invalid_url" | "unsupported_scheme";

export type FeedUrl =
  | { ok: true; feedUrl: string; normalized: string; podcastId: string }
  | { ok: false; error: FeedUrlError };

/** Raw character, its stand-in, the stand-in as every parser encodes it, and the character as credentials encode it. */
const STAND_INS = [
  { raw: "^", standIn: "", encoded: "%EE%80%80", inCredentials: "%5E" },
  { raw: "|", standIn: "", encoded: "%EE%80%81", inCredentials: "%7C" },
];

export function normalizeFeedUrl(input: string): FeedUrl {
  // 1. Trim, then parse as an absolute URL.
  let url: URL;
  try {
    url = new URL(STAND_INS.reduce((text, c) => text.replaceAll(c.raw, c.standIn), input.trim()));
  } catch {
    return { ok: false, error: "invalid_url" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return { ok: false, error: "unsupported_scheme" };
  if (url.hostname.includes("%")) return { ok: false, error: "invalid_url" };
  for (const c of STAND_INS) {
    url.username = url.username.replaceAll(c.encoded, c.inCredentials);
    url.password = url.password.replaceAll(c.encoded, c.inCredentials);
  }

  // 2. Drop the fragment, an empty `#` included.
  url.hash = "";
  const feedUrl = restore(url.href);

  // 3. http becomes https. The setter drops the port when it is https's
  // default, so `http://host:443/` loses it and `https://host:80/` keeps it.
  url.protocol = "https:";
  const normalized = restore(url.href);

  // 4. Path and query stay as given.
  return { ok: true, feedUrl, normalized, podcastId: uuidv5(NS_PODCAST, normalized) };
}

function restore(href: string): string {
  return STAND_INS.reduce((text, c) => text.replaceAll(c.encoded, c.raw), href);
}

export type Query = { kind: "empty" } | { kind: "terms"; terms: string } | { kind: "feed"; url: FeedUrl };

/**
 * Whether the search field holds a feed URL or search terms, the desktop's
 * rule (`search::classify`). A scheme (`://`) makes it a URL, so `feed://…`
 * reaches the normalizer and gets its error rather than a search. Without one,
 * a host and a path with no whitespace (`feeds.example.com/show.xml`) is a URL
 * that gets `https://`. A bare domain such as `syntax.fm` stays a search: a
 * site's home page is rarely its feed, and the name finds the show.
 */
export function classify(input: string): Query {
  const trimmed = input.trim();
  if (trimmed === "") return { kind: "empty" };
  if (trimmed.includes("://")) return { kind: "feed", url: normalizeFeedUrl(trimmed) };
  const slash = trimmed.indexOf("/");
  if (slash > 0 && trimmed.slice(0, slash).includes(".") && !/\s/.test(trimmed)) {
    return { kind: "feed", url: normalizeFeedUrl(`https://${trimmed}`) };
  }
  return { kind: "terms", terms: trimmed };
}
