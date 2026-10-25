# Conformance fixtures

JSON cases that the server, the Rust client and the Android client all run
(spec section 9). Each fixture file has a section below describing its
format and the rules an implementation must follow to pass it. The Rust
implementation lives in `crates/pod-protocol`; when this README and the Rust
code disagree, the fixture decides.

## ids.json

Feed URL normalization and the deterministic ids of spec section 1. Run by
`crates/pod-protocol/tests/conformance_ids.rs`.

### Format

The top-level object has these keys. Every row has a `name` describing the
case, for test failure messages.

| Key | Rows | Check |
| --- | --- | --- |
| `namespaces` | `podcast`, `episode` | `NS_PODCAST` and `NS_EPISODE` |
| `feed_urls` | `input`, `feed_url`, `normalized`, `podcast_id` | normalizing `input` gives `feed_url` (original scheme, stored and fetched) and `normalized` (`https` form), and `podcast_id` is UUIDv5 of `normalized` |
| `rejected_feed_urls` | `input`, `error` | normalizing `input` fails with error kind `error` |
| `episode_keys` | `guid` (string or `null`), `enclosure_url`, `key` | the episode key rule gives `key` |
| `episode_ids` | `podcast`, `podcast_id`, `key`, `episode_id` | UUIDv5 of `podcast_id` + `"\n"` + `key` is `episode_id`; `podcast` names the `feed_urls` row the id comes from |

Error kinds are `invalid_url` (the input doesn't parse as an absolute URL)
and `unsupported_scheme` (it parses, but the scheme isn't `http` or
`https`).

### Editing

Inputs and the expected `feed_url`, `normalized` and `key` strings are written
by hand. The ids are filled in by `fill_ids.py`, which uses Python's
`uuid.uuid5` and nothing from the implementations under test:

```bash
python3 -I conformance/fill_ids.py
```

### Feed URL normalization rules

Normalization is the WHATWG URL parser as implemented by the Rust `url`
crate (2.5), plus a few steps of our own. A JVM URL class is unlikely to
match it exactly (`java.net.URI` follows RFC 3986, not WHATWG), so run the
fixture rather than trusting one. In order:

1. Trim leading and trailing whitespace (Unicode `White_Space`, as Rust's
   `str::trim`).
2. Parse as an absolute URL with no base. Anything that fails is
   `invalid_url`, including empty input, a missing scheme
   (`example.com/feed.xml`), an empty or invalid host, and a port above
   65535.
3. If the scheme isn't `http` or `https`, fail with `unsupported_scheme`.
   `localhost:8080/feed.xml` parses with scheme `localhost` and lands here.
4. Drop the fragment, including an empty `#`. The result is `feed_url`.
5. If the scheme is `http`, change it to `https`. If the port is now 443,
   drop it. The result is `normalized`. `https://host:80/` keeps its port,
   since 80 is only the default for `http`.
6. `podcast_id` is UUIDv5(`NS_PODCAST`, UTF-8 bytes of `normalized`).

What step 2 normalizes, each with a fixture case:

- Scheme. Lowercased. Any number of slashes after `http:` or `https:`,
  including none, is accepted. Backslashes count as slashes.
- C0 controls (U+0000 to U+001F) and spaces at either end are stripped,
  which step 1 doesn't cover for U+0000. Tab, LF and CR anywhere in the
  input are removed.
- Host. Percent-decoded, then mapped with IDNA UTS 46: lowercased,
  fullwidth characters folded, non-ASCII labels converted to punycode
  (`xn--`). A trailing dot is kept, so `example.com.` is a different
  podcast. IPv4 addresses in any numeric form become dotted decimal. IPv6
  addresses are lowercased and compressed.
- Port. Dropped when it is the scheme's default (80 for `http`, 443 for
  `https`).
- Credentials. Kept, so two credential sets are two podcasts. Characters
  outside the userinfo set are percent-encoded (an `@` before the last one
  becomes `%40`). An empty password drops its `:`.
- Path. An empty path becomes `/`. Dot segments (`.`, `..`, and their
  percent-encoded forms `%2e`, `%2E`, `.%2e`) are resolved. Space, `"`, `<`,
  `>`, `` ` ``, `{`, `}`, C0 controls and non-ASCII characters (as UTF-8
  bytes) are percent-encoded. `|` and `^` are kept as they are. The current
  WHATWG standard encodes `^` as `%5E`, but the `url` crate doesn't, and
  the fixture follows the crate.
- Query. Kept in its original order, including an empty `?`. Space, `"`,
  `<`, `>`, `'`, C0 controls and non-ASCII characters are percent-encoded;
  `` ` ``, `{` and `}` are kept.
- Existing percent escapes in the path and query are kept as written: not
  decoded, hex case unchanged, invalid ones (`%zz`) left alone.

### Episode keys and ids

The episode key is the item's `<guid>` (or Atom `<id>`) with leading and
trailing whitespace trimmed. A missing, empty or all-whitespace guid falls
back to the enclosure URL, used as written. The guid is not otherwise
normalized; case and URL-looking guids are kept.

`episode_id` is UUIDv5(`NS_EPISODE`, UTF-8 bytes of `podcast_id + "\n" +
key`), with `podcast_id` as a lowercase hyphenated string.
