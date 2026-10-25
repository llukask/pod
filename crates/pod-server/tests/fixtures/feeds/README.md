# Feed fixtures

Inputs for `tests/feed_fixtures.rs`. Each real feed is trimmed to its channel
plus the first five items (three for the 80,000 Hours feed, whose show notes
are long), byte for byte otherwise: everything before the first `<item>`,
the first N `<item>…</item>` blocks, and everything after the last `</item>`.

The real feeds are captures from the test suite of
[Tombarr/podcast-feed-parser](https://github.com/Tombarr/podcast-feed-parser)
(MIT), `test/testfiles/`. They were captured in 2020 to 2022, not fetched live.

| Fixture | Source file | Publisher, host | Covers |
| --- | --- | --- | --- |
| `art19-the-daily.xml` | `the-daily.xml` | The New York Times, ART19 | iTunes tags, `content:encoded`, podtrac-wrapped enclosures |
| `npr-planet-money.xml` | `planet-money.xml` | NPR | Public broadcaster, `content:encoded`, long tracking query strings |
| `bbc-global-news.xml` | `worldserviceradio.xml` | BBC World Service | Public broadcaster, `itunes:new-feed-url`, Media RSS, plain `http` URLs |
| `soundon-gooaye.xml` | `gooaye.xml` | Gooaye, SoundOn | CJK text, `itunes:new-feed-url`, plain-text notes with escaped entities, `length="1"` placeholders |
| `backtracks-80000-hours.xml` | `80000HoursPodcast.xml` | 80,000 Hours, Backtracks via FeedBurner | Long HTML show notes |

The rest are hand-written:

- `atom-podcast.xml` is shaped like a Podlove Publisher Atom feed, with
  `<link rel="enclosure">` and iTunes tags in Atom.
- `malformed-missing-fields.xml` has no channel title or artwork, and items
  without an enclosure, an enclosure URL, a MIME type, a guid, a title or a
  valid date.
- `malformed-duplicate-guids.xml` repeats guids, also after trimming, and
  repeats an enclosure URL among items without a guid.
- `malformed-show-notes.xml` has scripts, handlers, iframes, forms, images
  and `javascript:` links in show notes, plus plain-text and entity-escaped
  descriptions.
