# Conformance fixtures

JSON cases that the server, the Rust client and the Android client all run
(spec section 9). Each fixture file has a section below describing its
format and the rules an implementation must follow to pass it. The Rust
implementation lives in `crates/pod-protocol`; when this README and the Rust
code disagree, the fixture decides.

The file name says which runner reads a file:

| File | Kind | Rust runner |
| --- | --- | --- |
| `ids.json` | id table | `crates/pod-protocol/tests/conformance_ids.rs` |
| `lww.json` | rule tables: pure inputs and expected outputs | `crates/pod-protocol/tests/conformance_lww.rs` |
| `sync_*.json` | client state transitions: local tables, a sync response, expected local tables | `crates/pod-protocol/tests/conformance_sync.rs` |

`conformance_sync.rs` also fails on any `.json` file none of these names
claims, so a new file can't be skipped by accident.

Two kinds cover the sync rules. Some rules only exist on the server (the
clamp, and two mutations in one batch clamped to the same millisecond),
and a client state transition can't express them, because a client never
sees the server's row between two mutations of one batch. Those live in
`lww.json` as pure functions of their inputs, which the server runs too.
Everything a client does with a response is a state transition and lives
in `sync_*.json`. Merge cases are transitions as well: a pulled row merging
into a local one is exactly what `sync_merge.json` sets up, so a separate
merge table would duplicate it.

Every case has a `name` that states the rule it pins, as a sentence. Ids
are real UUIDs. Device ids are UUIDv7, so their string order is the order
the sessions were created in. Each file's `description` (or `devices` map)
says which id is which device.

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

## lww.json

The last-writer-wins rule and the server's clock clamp (spec 4.4, 5.4).
Run by `crates/pod-protocol/tests/conformance_lww.rs`.

### Format

| Key | Rows | Check |
| --- | --- | --- |
| `devices` | device id → description | documentation; every id the cases use is listed |
| `wins` | `at`, `dev`, `old_at` (or `null`), `old_dev` (or `null`), `wins` | `wins(at, dev, old_at, old_dev)` from spec 4.4 |
| `clamp` | `at`, `server_now`, `stored_at` | `truncate_ms(min(at, server_now + 60 s))` is `stored_at` |
| `apply` | `server_now`, `before` (field or `null`), `mutations` (fields), `results`, `after` (field) | spec 4.4's `apply` for one field, below |

A field is `{ "value", "at", "dev" }`; `value` is any JSON value. A result
is `{ "status": "applied" | "superseded", "at" }`.

An `apply` case starts with the stored field `before` (`null` when nobody
has written it) and applies `mutations` in order, all at `server_now` as in
one batch. For each one: clamp its `at`; if `wins(clamped at, dev, field's
at, field's dev)`, replace the field with the mutation's value, clamped
`at` and `dev` and report `applied`, else report `superseded`. Either way
the result carries the clamped `at`. After the last mutation the field
equals `after` and the results equal `results`.

### Rules

- `wins` is true when `old_at` is null, when `at > old_at`, or when
  `at = old_at` and `dev >= old_dev`. The `=` case is the same device
  making an exact tie, which goes to the newer write (spec 4.4).
- Timestamps compare as instants at millisecond precision. Inputs may carry
  more digits or another offset; floor to the millisecond first.
- Device ids compare as their lowercase hyphenated strings. Don't use
  `java.util.UUID.compareTo`: it compares the two 64-bit halves as signed
  numbers, so an id starting with `8` to `f` sorts before one starting with
  `0` to `7`. A `wins` case pins this. Comparing the 16 bytes unsigned, as
  Rust's `Uuid` does, agrees with the string order.
- An `old_at` with a null `old_dev` (or the reverse) can't be written by
  any rule. pod-protocol reads such a pair as unstamped; the fixtures
  don't use it.

## sync_*.json

Client state transitions (spec 5.3, 5.4, section 9). Each file groups cases
on one topic. Run by `crates/pod-protocol/tests/conformance_sync.rs`
against the in-memory reference store in `pod_protocol::conformance`. A
Rust client runs the same files against its own store by implementing
`pod_protocol::conformance::ConformanceStore` and calling
`run_sync_fixtures`.

| File | Topic |
| --- | --- |
| `sync_merge.json` | pulled user-scope rows merging into local rows: newer, older, ties, null stamps, per-field independence, inserts, and the scope rows that follow subscriptions |
| `sync_clamp.json` | mutation results: outbox deletion and the result-`at` rewrite after the server clamped a fast clock, including two mutations clamped to one millisecond and a mutation stamped with a device id from before a re-login |

### Format

```json
{
  "description": "...",
  "cases": [
    {
      "name": "A sentence stating the rule.",
      "local_before": { "<table>": [ { "<column>": ... } ] },
      "server_response": { "server_time": "...", "results": [], "scopes": { ... } },
      "local_after": { "<table>": [ { "<column>": ... } ] }
    }
  ]
}
```

To run a case: replace the local store's contents with `local_before`,
apply `server_response` as one sync loop iteration does after its request
returns, then compare the tables with `local_after`.

Tables and columns are the local schema of spec 5.1, with this JSON
encoding:

- Timestamps (`*_at`, `created_at`) are RFC 3339 strings in UTC with exactly
  three fraction digits (`2026-10-24T09:13:04.317Z`), not epoch ms.
- Booleans are `true` / `false`, not 0 / 1.
- `setting.value` is the JSON value itself, not its text.
- `outbox.payload` is the mutation object as sent on the wire (spec 4.3).
- `sync_scope.scope` is `user` or `podcast:<id>`.

Tables in use: `account` (an array of at most one row, no `id` column),
`subscription`, `episode_state`, `setting`, `sync_scope`, `outbox`.
Unknown tables are an error, so a case that needs a table the runner
doesn't know fails instead of passing on half its data.

`local_before` lists complete rows: every column without an SQL default
must be present. Columns with a default (`position_ms`, `played`,
`archived`, the nullable `*_at` / `*_dev` pairs, `replaced_by`,
`sync_scope.complete`, `outbox.attempts`) may be left out. A table it
doesn't name starts empty.

`server_response` is a complete `POST /v1/sync` response body (spec 4.2),
so fixtures double as valid wire examples. It answers a request made of the
outbox mutations its `results` name. `scopes` is absent for a push-only
request.

`local_after` is a partial expectation:

- Only the tables it names are checked. An empty array asserts the table
  is empty.
- A named table's rows must match the store's one to one by primary key
  (`subscription.podcast_id`, `episode_state.episode_id`, `setting.key`,
  `sync_scope.scope`, `outbox.mutation_id`; `account` has one row and no
  key). Each expected row must name its key. A missing row and an extra
  row both fail.
- Within a row, only the columns it lists are compared, by JSON value in
  the encoding above. A column the table doesn't have fails, so a typo
  can't pass.

### Applying a response

The cases cover this part of spec 5.3's apply transaction, in this order:

1. For each result, delete the outbox row with its `id`. If the result has
   an `at` that differs from the sent mutation's `at`, find the field the
   mutation writes (`subscription.subscribed_*` for `subscription.set`,
   `episode_state.position_*`, `played_*` or `archived_*` for the
   `episode.*` types, `setting.value_*` for `setting.set`). If that field
   still carries exactly the sent `at` and the mutation's own `dev`, set
   its `*_at` to the result's `at`. Compare with the mutation's `dev`, not
   the account's current `device_id`: a mutation queued before a re-login
   carries the old id, and so does the field it wrote.
2. Merge each user-scope row into the local row with the same key (spec
   5.4), or insert it when there is none. Each LWW field merges on its own
   with `wins` from `lww.json`, using the pulled row's stamp as the new one.
   A pulled field whose stamp is null never replaces a local field.
   `subscription.feed_url` moves with `subscribed`. `replaced_by` isn't
   LWW: always take the pulled value.
3. Set the `user` scope row to the response's `version` and `complete`.
4. Reconcile scope rows with the merged subscription table: every
   `subscribed = true` podcast has a `podcast:<id>` row (inserted at version
   0, `complete = false`), and no `subscribed = false` podcast has one.

Results go before the merge, so a fast device's own row in the same
response ties with the rewritten stamp instead of losing to the
unclamped one.

Not covered yet, with the spec section that will need fixtures: the account
check that drops a response after logout (5.3, 5.7), podcast scopes and
deleted episodes (5.3), `replaced_by` handling and `rejected:
invalid_feed_url` cleanup (5.5), outbox coalescing (5.2). The reference
store fails on a response with podcast scopes or a `replaced_by` row, so
such a case can't pass by accident.
