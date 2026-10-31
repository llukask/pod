# Pod v2 technical spec

Status: draft, 2026-10-25. Reflects the server implementation on branch `v2` of the pod repo.
Read [design.md](design.md) first. This document defines the identifiers, schemas, wire protocol, client state machines and test fixtures. "Must" and "should" carry their RFC 2119 meaning.

## 1. Identifiers

All ids are lowercase hyphenated UUID strings.

| Entity | Id | Derivation |
| --- | --- | --- |
| Podcast | `podcast_id` | UUIDv5(`NS_PODCAST`, normalized feed URL) |
| Episode | `episode_id` | UUIDv5(`NS_EPISODE`, `podcast_id` + `\n` + episode key) |
| User | `user_id` | UUIDv7, server-generated |
| Device | `device_id` | UUIDv7, server-generated at login, one per session |
| Mutation | `mutation_id` | UUIDv7, client-generated |

Namespaces are fixed constants shared by server and clients.

```
NS_PODCAST = 6f1d3c2a-9b0e-5e7a-8c41-2d7f0a3b9e01
NS_EPISODE = 0c8a7e54-3f21-5b6d-9a10-7e4c2b1d8f02
```

Feed URL normalization produces two forms. The stored and fetched `feed_url` keeps the user's scheme. The normalized form, which the id hashes, is always `https`.

1. Trim whitespace. Parse as an absolute URL with the WHATWG URL parser, as the Rust `url` crate (2.5) implements it. Input that doesn't parse is `invalid_url`. A scheme other than `http` or `https` is `unsupported_scheme`, which includes `feed://` links.
2. Drop the fragment, including an empty `#`. The result is `feed_url`.
3. Replace scheme `http` with `https`. If the port is now 443, drop it. The result is the normalized form. Only the scheme's own default port is dropped, so `https://host:80/` keeps `:80` and `http://host:443/` becomes `https://host/`.
4. Keep path and query as given. Don't sort query parameters, some feed hosts sign them.

The WHATWG parser normalizes more than these steps, for example scheme and host case, IDNA, numeric IPv4 forms, percent-encoding, dot segments and an empty path becoming `/`. A trailing dot on the host is kept. Those behaviours are part of the id contract. `conformance/README.md` in the server repo lists every rule, and `conformance/ids.json` has a case for each. Implementations must pass that fixture rather than trust a platform URL class (`java.net.URI` follows RFC 3986, not WHATWG). The `url` crate keeps `^` in paths where the current WHATWG standard encodes it, and the fixture follows the crate. An upgrade of `url` or `idna` can therefore change podcast ids. The fixture catches that, and shipping such a change needs an alias migration, because the server redirects a mismatched id to the computed one (4.4).

Credentials in the URL (`https://user:pass@host/feed`, used by private feeds) stay in both forms. Two credential sets are two podcasts, and the id hashes the secret. `podcast.feed_url`, `podcast_alias` and `subscription.feed_url` hold such URLs. Logs and status messages never contain a feed URL (3.2), and a title derived from the URL drops the credentials (3.3).

The episode key is the item's `<guid>` (or Atom `<id>`) as written in the feed, trimmed. A missing, empty or all-whitespace guid falls back to the enclosure URL as the server's URL parser serializes it, so a parser upgrade that changes serialization changes those episode ids too. Two items in one feed with the same key are a feed bug. Ingest keeps the first one and logs the rest.

A podcast id is fixed at first subscription. If the feed later moves (HTTP 301/308 or `<itunes:new-feed-url>`), the server updates `feed_url` and records the old and new normalized URLs in `podcast_alias` (3.2 has the rules). A subscribe to any alias resolves to the existing podcast (section 4.4).

## 2. Server data model

PostgreSQL 16. Timestamps are `timestamptz`, always truncated to whole milliseconds before they're stored or compared (`date_trunc('milliseconds', ...)`), so they round-trip exactly through SQLite's epoch-ms integers. The exception is columns filled by `default now()` (`created_at`, `updated_at`, `last_seen`, `applied_at`), which keep microseconds. Clients never receive or compare them. Durations and positions are milliseconds in `bigint`.

```mermaid
erDiagram
    users ||--o{ session : "one device each"
    users ||--o{ subscription : "user scope"
    users ||--o{ episode_state : "user scope"
    users ||--o{ user_setting : "user scope"
    users ||--o{ applied_mutation : dedupes
    podcast ||--o{ episode : "podcast scope"
    podcast ||--o{ podcast_alias : "known URLs"
    episode ||--o{ episode_state : ""
```

Rows in a scope get their `version` from the scope owner's counter, `podcast.version_counter` or `users.version_counter`.

### 2.1 Version counters

Every syncable row has `version bigint not null`. Versions are allocated from a counter on the row's scope owner, never from a sequence.

```sql
-- 1. First statement of the writing transaction: lock the scope owner and
--    read its counter. The lock is held until commit, so writers in the same
--    scope serialize and commit in version order. A reader that sees version
--    v has already seen every version below v in that scope.
SELECT version_counter FROM podcast WHERE id = $1 FOR UPDATE;

-- 2. Assign counter + 1, counter + 2, ... to each row as it is written,
--    in memory, for as many rows as the transaction turns out to write.

-- 3. Before commit: store the final value.
UPDATE podcast SET version_counter = $final WHERE id = $1;
```

Where in the transaction the write-back goes doesn't matter for correctness, since the lock taken in step 1 is what serializes writers. Writes to tables outside the scope (`feed_poll` during ingest) may come before or after it. A transaction that allocated nothing writes no counter.

The same pattern applies to `users.version_counter` for user scope. Because the counter is read under the lock and written back at the end, the transaction never has to guess how many versions it needs up front, and two transactions can never hand out the same version. Taking the scope lock first also serializes all writers of a scope, so they can't deadlock on each other's rows. The ingest of one feed and the application of one sync request's mutations are the two writers, and each maps to one scope.

The guarantee above holds for a reader that sees one consistent snapshot. A scope spans several tables (user scope covers `subscription`, `episode_state` and `user_setting`, podcast scope covers `podcast` and `episode`). Reading them with separate statements under `READ COMMITTED` lets a writer commit between two of them, and the client's cursor can then pass a version it never received. The pull phase of a sync request (section 4.2) therefore runs in its own `REPEATABLE READ READ ONLY` transaction, which gives every statement in it the same snapshot.

### 2.2 Catalog

```sql
create table podcast (
    id                 uuid primary key,
    feed_url           text not null,
    status             text not null default 'pending'
                       check (status in ('pending', 'ok', 'failing', 'gone')),
    -- Why and since when a `failing` podcast fails (3.2). `feed_error` is
    -- the wire FeedError object (4.2). Set for failing rows and only them.
    failing_since      timestamptz,
    feed_error         jsonb,
    check ((status = 'failing') = (failing_since is not null and feed_error is not null)),

    title              text,
    author             text,
    description        text,
    link               text,
    artwork_url        text,
    language           text,
    metadata_hash      bytea,

    version            bigint not null,  -- 0 until the first successful ingest
    version_counter    bigint not null default 0,
    created_at         timestamptz not null default now(),
    updated_at         timestamptz not null default now()
);

-- Poller state, never sent to clients. Kept out of `podcast` so that
-- scheduling a poll (on subscribe, or on every poll lease) never locks or
-- writes the catalog row that version allocation locks.
create table feed_poll (
    podcast_id         uuid primary key references podcast(id),
    etag               text,
    last_modified      text,
    content_hash       bytea,
    next_poll_at       timestamptz not null default now(),
    consecutive_errors int not null default 0,
    -- Start of the current run of errors, null after a success. The
    -- 7-day failing rule (3.2) needs it, because the backoff (and any
    -- Retry-After) makes consecutive_errors useless as a measure of time.
    first_error_at     timestamptz,
    last_polled_at     timestamptz
);
create index on feed_poll (next_poll_at);

create table podcast_alias (
    normalized_url text primary key,
    podcast_id     uuid not null references podcast(id)
);

create table episode (
    id              uuid primary key,
    podcast_id      uuid not null references podcast(id),
    guid            text not null,

    title           text not null,
    summary_html    text not null default '',
    content_html    text not null default '',
    link            text,
    published_at    timestamptz not null,
    duration_ms     bigint,
    audio_url       text not null,
    audio_type      text not null,
    audio_bytes     bigint,
    artwork_url     text,
    season          int,
    number          int,

    content_hash    bytea not null,
    deleted         boolean not null default false,

    version         bigint not null,
    created_at      timestamptz not null default now(),
    updated_at      timestamptz not null default now(),

    unique (podcast_id, guid)
);
create index on episode (podcast_id, version);
create index on episode (podcast_id, published_at desc);
```

`summary_html` and `content_html` hold the feed's `description` (or `itunes:summary` when there is none) and `content:encoded`. `podcast.description` is sanitized HTML too, with the same allowlist, although its name has no `_html` suffix. Publishers put markup there as often as in show notes. Every one of these fields is stored as HTML, so clients always render HTML and never need a `*_type` column.

Ingest decides whether a text is plain or HTML. Atom and JSON Feed declare it, and the declaration is trusted. RSS never does, so the content decides. A text is HTML if it contains a tag-like `<x`, `</` or `<!`, or a character reference (`&amp;`, `&#8217;`, `&#x2019;`). A stray `<` or `&` in prose (`a < b`, `Q&A`) doesn't count. Plain text is HTML-escaped and its line breaks become `<br>`. Sanitized HTML that contains no line-level tag (`p`, `br`, `li`, `pre`, `blockquote`, `h1` to `h6`) also gets `<br>` for its newlines, since many hosts write plain notes with the odd link and rely on newlines for layout.

Show notes come from arbitrary publishers. Ingest sanitizes them with `ammonia` against an allowlist before storing. Kept tags are `p`, `br`, `a`, `strong`, `b`, `em`, `i`, `u`, `ul`, `ol`, `li`, `blockquote`, `code`, `pre` and `h1` to `h6`. The only kept attribute is `href` on `a`, restricted to absolute `http`, `https` and `mailto` URLs, and links get `rel="noopener noreferrer nofollow"`. A relative `href` is dropped, because clients have no base URL to resolve it against. Images, iframes, scripts, styles, forms and every event handler are dropped, and so is the text inside `script` and `style`. Images are dropped mainly because they're tracking pixels. Clients still treat the HTML as untrusted. Android and desktop render through their own whitelisting renderers rather than a web view. The web client inserts the sanitized HTML under a Content-Security-Policy that forbids inline script.

The podcast row is itself part of its catalog scope. A metadata change or a `status` change allocates a version and writes it to `podcast.version`, so clients see both.

### 2.3 Users and sessions

```sql
create table users (
    id              uuid primary key,
    username        text not null unique,
    password_hash   text not null,
    version_counter bigint not null default 0,
    created_at      timestamptz not null default now()
);

create table session (
    token_hash   bytea primary key,  -- sha256 of the bearer token
    user_id      uuid not null references users(id) on delete cascade,
    device_id    uuid not null unique,
    device_name  text not null,
    created_at   timestamptz not null default now(),
    last_seen    timestamptz not null default now(),
    expires_at   timestamptz not null
);
```

Tokens are 32 random bytes, base64url encoded. Only the hash is stored. Expiry is 90 days and slides. Any authenticated request more than 24 hours after the last extension pushes `expires_at` to now plus 90 days. The last extension has no column of its own. It is `expires_at - 90 days`. A session is valid while `expires_at > now`. `last_seen` is written at most every 5 minutes per session, so most authenticated requests are a single read. A daily job deletes sessions with `expires_at <= now`. It needs no index on `expires_at`, since there is one row per signed-in device. The device id lives only as long as its session. `*_dev` columns in user state keep old device ids, which is fine because they only break ties.

### 2.4 User state

Each mutable field carries an `*_at` (the client action time) and a `*_dev` (device id, tiebreaker). See section 4.4 for the merge rule.

```sql
create table subscription (
    user_id          uuid not null references users(id) on delete cascade,
    podcast_id       uuid not null,  -- no FK: alias rows point at ids with no podcast row
    feed_url         text not null,
    subscribed       boolean not null,
    subscribed_at    timestamptz not null,
    subscribed_dev   uuid not null,
    replaced_by      uuid,           -- server-authoritative, see 4.4
    version          bigint not null,
    primary key (user_id, podcast_id)
);
create index on subscription (user_id, version);

create table episode_state (
    user_id          uuid not null references users(id) on delete cascade,
    episode_id       uuid not null references episode(id),
    podcast_id       uuid not null,

    position_ms      bigint not null default 0,
    position_at      timestamptz,
    position_dev     uuid,

    played           boolean not null default false,
    played_at        timestamptz,
    played_dev       uuid,

    archived         boolean not null default false,
    archived_at      timestamptz,
    archived_dev     uuid,

    version          bigint not null,
    primary key (user_id, episode_id)
);
create index on episode_state (user_id, version);

create table user_setting (
    user_id     uuid not null references users(id) on delete cascade,
    key         text not null,
    value       jsonb not null,
    value_at    timestamptz not null,
    value_dev   uuid not null,
    version     bigint not null,
    primary key (user_id, key)
);
create index on user_setting (user_id, version);

create table applied_mutation (
    user_id      uuid not null,
    mutation_id  uuid not null,
    result       jsonb not null,
    applied_at   timestamptz not null default now(),
    primary key (user_id, mutation_id)
);
-- Every pushed mutation leaves a row here for 30 days, so this is the one
-- table that grows with use rather than with users. The daily prune
-- deletes by applied_at and would scan the whole table without this index.
create index on applied_mutation (applied_at);
```

`subscription.feed_url` is the normalized URL with its original scheme (section 1), credentials included. It has no `*_at` or `*_dev` of its own and moves with `subscribed`, on the server and in the client merge (5.4).

The daily prune deletes `applied_mutation` rows with `applied_at < now - 30 days`, in committed batches of 10,000, so a backlog never becomes one huge transaction. A client that holds a mutation longer than that and replays it gets a fresh evaluation, which is still safe because every mutation is LWW-guarded. Rejected mutations are recorded too (4.4).

Synced settings and their defaults:

| Key | Type | Default |
| --- | --- | --- |
| `inbox_window_days` | int | 30 |
| `playback_speed` | number | 1.0 |
| `skip_back_ms` | int | 15000 |
| `skip_forward_ms` | int | 30000 |
| `played_threshold_ms` | int | 30000 |

Unknown keys are stored and synced unchanged, so older clients don't drop settings that newer ones write. The server accepts any JSON value for any key, known keys included, so `playback_speed: "fast"` is `applied`. Clients must tolerate a value of the wrong type for a known key and use the default instead. The server's inbox does the same for `inbox_window_days` (section 6).

## 3. Feed poller and ingest

### 3.1 Claiming work

The poller runs a few workers (default 4). Each loops:

```sql
WITH due AS (
    SELECT podcast_id FROM feed_poll
    WHERE next_poll_at <= now()
    ORDER BY next_poll_at
    LIMIT 1
    FOR UPDATE SKIP LOCKED
)
UPDATE feed_poll SET next_poll_at = now() + interval '15 minutes'
FROM due WHERE feed_poll.podcast_id = due.podcast_id
RETURNING feed_poll.*;
```

The provisional `next_poll_at` lease keeps a crashed worker's podcast from being polled in a tight loop. It is also the only crash recovery. On shutdown a worker abandons its poll in flight instead of waiting out a slow host, its transaction rolls back, and the lease makes the feed due again.

Feeds marked `gone`, and feeds without subscribers, get `next_poll_at = 'infinity'` and stay out of the poll set until someone subscribes again. The mutation that unsubscribes the last subscriber sets it (section 4.4). The poller's own schedule writes never overwrite `infinity`, so an unsubscribe that parks a feed while it is being polled keeps it parked. They do overwrite a `now()` that a subscribe wrote during the poll. That subscribe then waits for the next scheduled poll, and the poll in flight has already fetched the feed. Subscribe and unsubscribe both lock the podcast's `feed_poll` row (`SELECT ... FOR UPDATE`) before counting subscribers. Without that lock, one user's unsubscribe could miss another user's concurrent, still uncommitted subscribe and park the feed at `infinity` with a live subscriber.

### 3.2 Fetch

- Send `If-None-Match` with `etag` if stored, else `If-Modified-Since` with `last_modified`.
- `User-Agent: pod/<version> (+<server base url>)`. 20 s timeout for the whole fetch, all redirect hops and the body included. Body cap 20 MiB, enforced while streaming, after decompression.
- Follow up to 5 redirects, re-checking each hop. A 301 or 308 in the chain moves the feed to the target of the last permanent hop, not to the final URL, so a temporary location after a permanent move is never stored. The move only happens when the poll ends in a parsed feed or a `304`. A permanent redirect to a page that errors isn't trusted.
- Feed URLs are user input, so every fetch, and every redirect hop, goes through an outbound guard against server-side request forgery. The scheme must be `http` or `https`. The guard resolves the host itself and rejects the request if any resolved address is loopback, private (RFC 1918), link-local (which includes cloud metadata at `169.254.169.254`), CGNAT, unique-local IPv6, multicast or unspecified. IPv4-mapped and NAT64 IPv6 addresses are judged by their embedded IPv4 address. A literal IP host is checked the same way before connecting. The guard then connects to the checked address, so a second DNS answer can't swap in an internal one (DNS rebinding). It ignores `HTTP(S)_PROXY`, because a proxy would resolve DNS itself and void the checks. Ports other than 80 and 443 are refused, except on hosts the allowlist names (next item).
- The `fetch_allowlist` config key (section 12) opens specific internal hosts for someone who self-hosts feeds on their LAN. Entries are host names (exact match, all their addresses allowed), IP addresses or CIDRs (any host resolving inside them allowed). A URL whose host the allowlist names may use any port, because LAN servers often listen on 8080 and the like: a host name matching a host-name entry, or a literal IP inside an IP or CIDR entry. A host name that only resolves into an allowlisted CIDR keeps the 80/443 rule. The port is checked before DNS, and opening ports by resolved address would let whoever controls a public name's DNS reach every service in the allowlisted network. The rule applies on every redirect hop and to the `subscription.set` guard check (4.4).
- Credentials in the URL (`https://user:pass@host/...`, used by some private feeds) are sent as basic auth. They stay on same-origin redirect hops and are dropped on a cross-origin hop unless its `Location` carries its own. They never appear in logs, errors or status messages. Logs record the podcast id, not the URL.
- `304` ends the poll as unchanged. It counts as a success.
- `410` sets `status = 'gone'`, clears the error run and stops polling. Existing episodes stay visible. No episode tombstones come from this or from any other feed content.
- `429` and `503` count as errors like any other, so the backoff (3.4) and the status rules below apply. A `Retry-After` header (delay-seconds or an HTTP-date) can only lengthen the wait, and is capped at 7 days, so a buggy or hostile header can't park a feed for good.
- Other errors, including a body that doesn't parse as a feed, increment `consecutive_errors` and set `first_error_at` if it is null. A podcast that is `pending` (never fetched successfully), `failing` or `gone` becomes `failing` on the first error, so a mistyped URL shows an error within seconds rather than a spinner. An `ok` podcast becomes `failing` once `now - first_error_at` reaches 7 days. A success (a parsed feed or a `304`) resets `consecutive_errors` and `first_error_at` and sets `ok` again.
- A `gone` podcast that someone subscribes to again is polled like a new one. A `410` keeps it `gone`, any other error makes it `failing` at once, and a success makes it `ok`.
- A `failing` podcast records why and since when. `feed_error` is one of a closed set of codes (the `FeedError` object of 4.2): `http` with the status, `not_a_feed`, `invalid_url`, `port_not_allowed`, `address_not_allowed`, `host_not_found`, `too_many_redirects`, `invalid_redirect`, `too_large`, `timeout` or `connection_failed`. Clients word the code themselves, so the server never sends free text and never the feed URL. `failing_since` is `feed_poll.first_error_at` at the time of the write, so an `ok` podcast that turns failing reports the week it has already been failing. `ok`, `pending` and `gone` carry neither.
- Every `status`, `failing_since` or `feed_error` change locks the podcast row, allocates a version (section 2.1) and writes it, so clients receive it through the podcast scope. A status write that changes nothing allocates no version, so a feed that keeps failing the same way doesn't burn one per poll. A successful ingest folds `status = 'ok'` (and a cleared reason) into the same podcast row write as any metadata change, so clients get one version for both. When ingest writes nothing (the feed hash matches, or the feed answered `304`), the poller sets `ok` itself, so a `failing` podcast whose feed recovers unchanged still recovers.

Feed moves. A permanent redirect (above) and an `<itunes:new-feed-url>` holding an `http(s)` URL both move the feed. Many feeds name their own URL there, which is no move. The poller applies a move before ingest, because the feed hash doesn't cover `itunes:new-feed-url` and an otherwise unchanged feed would skip it. The new URL is first fetched on the next poll. A move records the old and new normalized URLs in `podcast_alias` and updates `podcast.feed_url`, which is client-visible and therefore takes a podcast version. A podcast still at version 0 has never been sent to a client, so its move allocates no version, and its first ingest carries the new URL. A move to a URL that is already an alias of a different podcast is refused. The podcast keeps polling its old URL and a warning is logged, because taking over the alias would silently redirect the other podcast's subscribers. Merging two podcasts that turn out to be one feed has no rule yet.

### 3.3 Parse and diff

Parse with `feed-rs`. Never panic on missing fields. Only a body that isn't a feed at all is an error. Everything else degrades to `null`, a fallback, or a skipped item. A panic inside the parser counts as a parse error for the whole feed.

- A missing podcast title becomes the feed URL without its credentials, since the title goes to every subscriber. A missing episode title becomes the last path segment of the audio URL, percent-decoded. Missing artwork becomes `null`. `<itunes:image>` wins over RSS `<image>`.
- An item without a date gets the Unix epoch as `published_at`. The fallback must be the same on every poll, or the episode hash would churn. The feed's `pubDate` or `lastBuildDate` move whenever the publisher rebuilds the feed. The epoch also sorts undated items last.
- `link`, `artwork_url` and `audio_url` must be absolute `http` or `https` URLs. Anything else (`javascript:`, `data:`, relative) becomes `null`, because clients render these as anchors or fetch them.
- An item without a playable enclosure is skipped. Playable means `audio/*`, `video/mp4` or `video/x-m4v`. The first playable enclosure wins. When the declared type is missing, `application/octet-stream` or `binary/octet-stream` (common for S3-style hosts), the type comes from the URL's extension (`mp3` is `audio/mpeg`, `m4a` `audio/mp4`, `aac` `audio/aac`, `ogg` `audio/ogg`, `opus` `audio/opus`, `mp4` `video/mp4`, `m4v` `video/x-m4v`). Any other extension means not playable.
- `audio_bytes` below 1024 is stored as `null`. Hosts that don't know the size write placeholders such as `0` or `1`, and no real episode is under a kibibyte.
- `<itunes:duration>` is read as seconds, `MM:SS` or `HH:MM:SS`, each with optional fractional seconds. Zero means unknown and becomes `null`. `<podcast:episode>` maps to `number` only when it is a whole number.

Compute three SHA-256 hashes, each over canonical JSON (sorted keys, no whitespace):

- the episode hash, per episode, over its client-visible fields
- the metadata hash, over the normalized podcast metadata
- the feed hash, over the metadata hash followed by every `(guid, episode hash)` pair sorted by guid

Any added, removed or changed episode changes the feed hash. Volatile feed fields such as `lastBuildDate` aren't client-visible, so they don't. Neither does `itunes:new-feed-url`, which is why feed moves are applied before ingest (3.2). In one transaction:

1. Lock the podcast row and read its counter (section 2.1).
2. If the feed hash matches `feed_poll.content_hash`, commit without writing anything, status included, and stop. The check runs under the lock, not before it, so of two concurrent ingests of one feed the second finds the first's hash and writes nothing. The cost is a row lock on every unchanged poll.
3. Load `(guid, content_hash, deleted)` for all episodes of the podcast.
4. Build the change list. Unknown guid means insert. Known guid with a different hash means update, and the update also sets `deleted = false`. Known guid with the same hash means no write, deleted or not. Only an admin purge sets `deleted`, so a purged episode stays deleted while the feed carries it unchanged, however much else of the feed changes, and comes back as a fresh version once the publisher changes that episode. Episodes missing from the feed are left alone.
5. If the metadata hash differs from the one stored with the podcast row, or the podcast isn't `ok`, add the podcast row to the list (3.2).
6. Write the podcast row first, then episodes oldest first by `(published_at, guid)`. A client backfilling a large catalogue then gets the podcast's title and artwork on its first page, and versions follow publication order. The guid breaks ties, which are common because undated items all get the epoch, so the same feed always gets its versions in the same order.
7. Store the feed hash and other poller state, write back the counter and commit.

### 3.4 Schedule

After a successful poll, `next = now + poll_interval + random jitter of up to 10 %`. `poll_interval` is server config, default 15 minutes.

After an error, `next = now + max(min(15 min * 2^consecutive_errors, 24 h), Retry-After)`, where `consecutive_errors` already counts this error. The first error therefore waits 30 minutes. `Retry-After` is capped at 7 days (3.2).

A `subscription.set` that moves the user's canonical subscription row from unsubscribed to subscribed sets `feed_poll.next_poll_at = now()` (4.4). A subscribe that loses LWW, or finds the row already subscribed, leaves the schedule alone.

## 4. HTTP API

Base path `/v1`. JSON bodies, UTF-8. All timestamps RFC 3339 with `Z`. Authenticated endpoints take `Authorization: Bearer <token>`.

Errors use one shape:

```json
{ "error": { "code": "invalid_credentials", "message": "Username or password is wrong." } }
```

| Status | Codes |
| --- | --- |
| 400 | `bad_request`, `invalid_cursor` |
| 401 | `unauthenticated`, `invalid_credentials` |
| 403 | `registration_disabled` |
| 404 | `not_found` |
| 409 | `username_taken`, `no_session` |
| 413 | `batch_too_large` |
| 429 | `rate_limited` (with `Retry-After`) |
| 500 | `internal` |
| 502 | `upstream_unavailable` |

A body that isn't valid JSON for the endpoint, or a missing or wrong JSON `Content-Type`, is `400 bad_request` in this shape, not `415`. A malformed query parameter or path id is `400 bad_request` too. `upstream_unavailable` means a service the request depends on (the podcast directory, 4.6) failed, timed out or sent something unusable.

The code set is open. A later server version may add codes, so a client must accept a code it doesn't know and handle it by the response's HTTP status alone: `429` and `5xx` are retried later, any other `4xx` fails the request without a retry.

### 4.1 Auth

`POST /v1/auth/register` and `POST /v1/auth/login`

```json
{ "username": "alice", "password": "...", "device_name": "Pixel 8" }
```

```json
{ "token": "...", "device_id": "0192...", "user_id": "0192...", "expires_at": "2027-01-22T10:00:00Z" }
```

Both answer `200` with this body. Register checks its input and answers `400 bad_request` when a rule fails:

- `username` is trimmed and lowercased, then must be 1 to 64 characters of `a-z`, `0-9`, `.`, `_` and `-`. "Alice" and "alice" are one account, and Unicode look-alikes can't exist.
- `password` must be at least 8 characters and at most 1024 bytes.
- `device_name` is trimmed, then must be 1 to 100 characters.

A taken username is `409 username_taken`, and a server with registration off answers `403 registration_disabled`. Login applies the same normalization but not the register rules. A username or password that can't match any account is `401 invalid_credentials`, like a wrong password, so accounts made under older rules keep working. An unknown username is still checked against a dummy Argon2id hash, so response time doesn't reveal which names exist.

Login always creates a new device id. Logging in again on the same phone after a logout gets a new one, which is harmless because the device id only breaks LWW ties. Clients put their device id into each mutation (section 4.3), so mutations queued before a re-login keep the id they were stamped with locally.

`POST /v1/auth/logout` deletes the session and answers `204` with no body. `GET /v1/auth/me` returns `200` with `{ "user_id", "username", "device_id" }`.

Login and register share one rate limit, 10 attempts per minute in a sliding window, counted per client IP and per normalized username. IPv6 clients are grouped by /64, since one client usually owns a whole /64. An attempt must pass every key and then counts against all of them. A refused attempt counts against none, so hammering one username doesn't also use up the IP's budget. The client IP is the TCP peer. Behind a reverse proxy every peer is the proxy, so the `client_ip_header` config key (section 12) names a header the proxy sets, such as `X-Real-IP`. The server reads it only when the peer is in `trusted_proxies`, since any other client could forge it. The header must hold exactly one IP address. A trusted proxy that sends no such header, an unparsable one, or several values gets its own address as the client IP, and the server logs a warning the first time and at debug level after that. The limiter lives in the server's memory, so it assumes one server process.

### 4.2 Sync

`POST /v1/sync` does push and pull in one round trip. Clients may send an empty `mutations` array (pull only) or omit `scopes` (push only).

Request:

```json
{
  "mutations": [
    {
      "id": "0192f0a1-...",
      "at": "2026-10-24T09:12:03.120Z",
      "dev": "0192e7c4-...",
      "type": "episode.position",
      "episode_id": "4b1e...",
      "position_ms": 1234000
    }
  ],
  "scopes": {
    "user": 1841,
    "podcasts": [ { "id": "a71b...", "version": 5523 }, { "id": "9c3e...", "version": 0 } ]
  },
  "limit": 500
}
```

`limit` caps the total number of changes in the response (default 500). The server clamps it to 1..=2000 rather than rejecting a value outside that range. At most 200 mutations per request. More is `413 batch_too_large`. A mutation without a usable `id` (missing or not a UUID) is `400 bad_request`, because results are keyed by id and can't report it. Any other problem with one mutation only rejects that mutation (4.3).

Response:

```json
{
  "server_time": "2026-10-24T09:12:04Z",
  "results": [
    { "id": "0192f0a1-...", "status": "applied", "at": "2026-10-24T09:12:03.120Z" }
  ],
  "scopes": {
    "user": {
      "version": 1843,
      "complete": true,
      "subscriptions": [ ... ],
      "episode_states": [ ... ],
      "settings": [ ... ]
    },
    "podcasts": [
      { "id": "a71b...", "status": "not_subscribed" },
      {
        "id": "9c3e...",
        "status": "ok",
        "version": 812,
        "complete": false,
        "podcast": { ... },
        "episodes": [ ... ]
      }
    ]
  }
}
```

Server rules:

1. Apply mutations in request order, all in one transaction (section 4.4), and commit. A request without mutations skips this transaction, so a pull-only request takes no lock and never waits on another device.
2. Then read everything in one `REPEATABLE READ READ ONLY` transaction (section 2.1).
3. Fill the user scope first. Within a scope, take rows with `version > cursor` from each of the scope's tables, merge them by `version`, and keep the lowest `remaining limit` of them. One `UNION ALL ... ORDER BY version LIMIT n` query per scope does this. So does reading each table with its own `version > cursor ORDER BY version LIMIT n + 1` and merging in memory, which pod-server does. Versions are unique within a scope, so the lowest `n + 1` of the scope are always among each table's lowest `n + 1`, and the extra row shows whether more exist without a count. `version` in the response is the highest version returned, or the request cursor if none. `complete` is true when no rows beyond it exist.
4. Podcast scopes are filled in request order until the limit runs out. The podcast row counts against the limit like an episode and merges with the episodes by version. A podcast at version 0 (pending, 4.4) is never sent. Scopes not reached return `{ "id", "status": "ok", "version": <request cursor>, "complete": false }` with no rows. `scopes.podcasts` is an array, not an object, because its order matters and JSON object key order doesn't survive every serializer.
5. A podcast scope is served only while the user's subscription row for it is `subscribed` with no `replaced_by`. Any other scope returns `{ "id", "status": "not_subscribed" }`, and the client drops it. The check runs inside the snapshot, before the limit, so even a scope the limit didn't reach answers `not_subscribed`.
6. Deleted episodes appear as `{ "id": "...", "deleted": true, "version": 900 }`. Clients remove them locally.
7. No `410` and no cursor expiry exist. Rows are never physically deleted (except on account deletion), so any cursor stays valid.
8. Each entry in `scopes.podcasts` is answered on its own. Clients must not list a podcast twice. A duplicate gets the same rows twice and spends the limit twice.

Client loop: send, apply, repeat while any returned scope has `complete: false` or new scopes appeared (a new subscription in the user scope adds a podcast scope at cursor 0).

Entity shapes:

```json
// podcast. `status` tags the feed status; only "failing" carries more
// (3.2): `failing_since` and `error`, the FeedError object tagged by `code`,
// which carries `status` only for "http".
{ "id": "9c3e...", "feed_url": "https://...", "status": "ok",
  "title": "...", "author": "...", "description": "...", "link": "...",
  "artwork_url": "https://...", "language": "en", "version": 812 }

{ "id": "1a2b...", "feed_url": "https://...", "status": "failing",
  "failing_since": "2026-10-20T05:00:00.000Z", "error": { "code": "http", "status": 404 },
  "title": null, "author": null, "description": null, "link": null,
  "artwork_url": null, "language": null, "version": 1 }

// episode
{ "id": "4b1e...", "podcast_id": "9c3e...", "title": "...", "summary_html": "...",
  "content_html": "...", "link": null, "published_at": "2026-10-20T05:00:00Z",
  "duration_ms": 3612000, "audio_url": "https://...", "audio_type": "audio/mpeg",
  "audio_bytes": 57800000, "artwork_url": null, "season": null, "number": 212,
  "deleted": false, "version": 811 }

// subscription
{ "podcast_id": "9c3e...", "feed_url": "https://...", "subscribed": true,
  "subscribed_at": "2026-10-01T18:00:00Z", "subscribed_dev": "0192...",
  "replaced_by": null, "version": 1830 }

// episode_state
{ "episode_id": "4b1e...", "podcast_id": "9c3e...",
  "position_ms": 1234000, "position_at": "...", "position_dev": "...",
  "played": false, "played_at": null, "played_dev": null,
  "archived": false, "archived_at": null, "archived_dev": null,
  "version": 1843 }

// setting
{ "key": "playback_speed", "value": 1.3, "value_at": "...", "value_dev": "...", "version": 1790 }
```

States and settings include their `*_at` and `*_dev` fields so clients can run the same merge function as the server (section 5.4).

### 4.3 Mutation types

| `type` | Fields | Effect |
| --- | --- | --- |
| `subscription.set` | `podcast_id`, `feed_url`, `subscribed` | Create or update a subscription |
| `episode.position` | `episode_id`, `position_ms` | Set position |
| `episode.played` | `episode_id`, `played` | Mark played or unplayed |
| `episode.archived` | `episode_id`, `archived` | Hide from or return to the inbox |
| `setting.set` | `key`, `value` | Set a synced setting |

Every mutation has `id`, `at` and `dev`. `dev` is the device id the client stamped the change with locally (section 5.2), which isn't always the device id of the current session. The server uses `m.dev` for the tiebreak and doesn't check it against the session. A forged `dev` can only reorder ties in the user's own data. Each result carries the clamped `at` (section 4.4), which differs from the sent one when the server clamped it. For `applied` that is the stored stamp. `superseded` and `rejected` results carry it too. A `rejected` result has no `at` only when the mutation's own `at` was unreadable, since then there is nothing to echo. Results:

| `status` | Meaning | Client action |
| --- | --- | --- |
| `applied` | Stored | Delete from outbox |
| `superseded` | A newer value already exists for the field | Delete from outbox |
| `rejected` | Will never succeed. `code` is `invalid_mutation`, `unknown_episode` or `invalid_feed_url` | Delete from outbox, log, surface if user-visible |

The server parses each mutation separately. An unknown `type`, a missing field or a bad value rejects that one mutation with `invalid_mutation` and doesn't fail the request. A request-level `400` only happens when the envelope itself is broken, or a mutation has no usable `id` (4.2), which is a client bug. A `rejected` result carries `code` and no message. The server logs the reason. Rejection codes are an open set like the error codes (section 4). A client treats a `rejected` result whose `code` it doesn't know like any other `rejected`: it deletes the mutation from the outbox and logs the code.

A request that fails as a whole (network, 5xx, 429) leaves the outbox untouched, and the client retries with the same mutation ids. Replayed ids return the stored result from `applied_mutation`.

### 4.4 Applying a mutation

```
apply_batch(user, mutations):
    begin
    counter = SELECT version_counter FROM users ... FOR UPDATE   -- section 2.1
    for m in mutations: results += apply(user, m)
    UPDATE users SET version_counter = counter
    commit

write(row):                       -- every row write in the batch goes through this
    counter += 1; row.version = counter; upsert row

apply(user, m):
    if applied_mutation has (user, m.id): return stored result
    at = truncate_ms(min(m.at, server_now + 60 s))
    load the target row (or a default row)
    for each field f the mutation writes:
        if wins(at, m.dev, row.f_at, row.f_dev)
        then f = value, f_at = at, f_dev = m.dev; changed = true
    if changed: write(row) -> "applied"
    else -> "superseded"
    result.at = at
    record result in applied_mutation    -- rejections too, see below

wins(at, dev, old_at, old_dev) =
    old_at is null
    or at > old_at
    or (at = old_at and dev > old_dev)
    or (at = old_at and dev = old_dev)       -- same device, same stamp: later wins
```

Taking the `users` row lock first means two requests from one user (two devices, or a retry racing the original) run one after the other. That rules out deadlocks between them, and the replayed request sees the original's `applied_mutation` rows. Versions come from the locked counter as rows are written, so a batch can write any number of rows, including two per aliased subscribe.

The last line of `wins` matters when clamping collapses two stamps. A device with a fast clock retries an older in-flight mutation together with a newer one for the same field, and both clamp to the same millisecond. Mutations from one device always reach the server in the order the device made them: the outbox is sent in `created_at` order and only one request runs at a time. So for an exact tie from the same device, the later one is the newer action and must win. Clients use the same `wins` function (section 5.4).

A `rejected` result is recorded in `applied_mutation` like the others. A rejected mutation can never succeed later, so a replay that returns the stored rejection gives the same answer a fresh evaluation would.

Type-specific rules:

- `episode.position` only writes the position. A negative position is `rejected: invalid_mutation`. There is no upper bound. Feed durations are often wrong, and a real position past the stated duration must still sync.
- `episode.*` on an unknown episode id returns `rejected: unknown_episode`.
- `subscription.set`, subscribe (`subscribed: true`) and unsubscribe alike, runs the steps below. Say the mutation names X.

1. Normalize `feed_url` (section 1). If it doesn't normalize, reject with `invalid_feed_url`. A subscribe is also rejected with `invalid_feed_url` when the fetch guard (3.2) would refuse the URL without a DNS lookup (wrong scheme, a port other than 80 or 443 on a host the allowlist doesn't name, or a literal internal IP address that the allowlist doesn't open). Host names are resolved only when the poller fetches, never inside this transaction. An unsubscribe skips the guard check, so a user can always leave a feed, even after the allowlist that admitted it shrank.
2. Find the canonical podcast Y. If this user's row for X has `replaced_by`, Y is that. Otherwise Y is the podcast `podcast_alias` maps the normalized URL to, or else the id computed from it. The URL wins over `m.podcast_id` even when no alias explains the difference, so a client that derived the id differently heals by following `replaced_by` instead of losing the subscribe. If Y differs from X, the mutation is redirected (see "Alias redirect" below). A device that queued an unsubscribe of X before learning about the alias still unsubscribes Y.
3. On a subscribe to a URL the server doesn't know, insert a podcast row with `status = 'pending'` and `version = 0`, plus its `feed_poll` row (due now) and alias row, all `ON CONFLICT DO NOTHING` because two users may subscribe to a new feed at the same moment. The second insert waits for the first transaction and then finds the rows. No version is allocated. Pull returns rows with `version > cursor` and clients start at 0, so nobody receives the pending row. The client already shows its own placeholder. If a feed move the poller committed meanwhile now owns the URL as an alias, Y follows that alias.
4. Lock Y's `feed_poll` row (`SELECT ... FOR UPDATE`), if it has one.
5. Apply `subscribed`, `at` and `dev` to Y's row with `wins`. `feed_url` takes the mutation's normalized URL (original scheme) whenever `subscribed` wins, so after a redirect Y's row holds the URL the user subscribed with, which may be an alias.
6. If Y's row went from unsubscribed to subscribed, set `next_poll_at = now()`. That also brings back a feed parked as `gone` or without subscribers. If Y's row ends unsubscribed and no user is still subscribed, set `next_poll_at = 'infinity'`. A subscribe that loses LWW changes neither.

Alias redirect. The mutation names X and the canonical podcast is Y.

1. If this user has a row for X without `replaced_by`, it holds the user's own earlier choice for X. Carry its fields (`subscribed`, `subscribed_at`, `subscribed_dev`, and `feed_url` with them) into Y's row first, but only when X's stamp is strictly newer than Y's: a later `subscribed_at`, or the same `subscribed_at` and a greater `subscribed_dev`. On an exact tie Y keeps its fields. If Y has no row yet, X's fields become Y's. This is `wins` without its same-device tie rule. Such a tie means one device's mutations for X and then Y were clamped into the same millisecond, and Y, written later in batch order, holds the newer action. Clients carry X into Y with the same rule (5.5), so the server's Y ends in the state clients compute. The mutation itself then goes through `wins` in step 2 as usual.
2. Apply the mutation to Y as in step 5 above.
3. Write Y's row with a new version even when step 2 lost to a newer value. Clients that still hold X need Y's current state, and an unchanged row would never be sent again.
4. Write X's row with `subscribed = false`, `replaced_by = Y`, `feed_url` from the mutation, and a new version. Its stamp is the lowest there is, `subscribed_at` = the Unix epoch and `subscribed_dev` = the nil UUID, so X's row never wins a client's merge. With a real stamp, a client that already received Y would merge X's `false` into Y in 5.5 step 1 and show Y unsubscribed while the server has it subscribed. X has no podcast row behind it, which is why `subscription` has no foreign key.

The result's status reports step 2. `replaced_by` isn't an LWW field. The server sets it, and clients act on it whenever a received row carries it (section 5.5). X's and Y's rows can land in different pages, in either order. Section 5.5 makes the client handling order-independent.

A subscribe or unsubscribe therefore writes only to user scope and `feed_poll`. It never allocates from `podcast.version_counter` and never takes the `podcast` row lock that ingest holds. The podcast scope only moves when ingest finds a real change, which every subscriber should receive anyway.

Lock order. A `subscription.set` takes its locks in this order. First the `users` row (the batch's scope lock), then the new `podcast`, `feed_poll` and `podcast_alias` rows of step 3, then Y's `feed_poll` row, then the user's `subscription` rows. Ingest locks `podcast` and then writes `feed_poll`. The poller's `feed_poll` writes are single statements. Nothing takes `feed_poll` and then `users` or `podcast`. Taking the `feed_poll` lock before counting subscribers (3.1) keeps one user's last unsubscribe from missing another user's still uncommitted subscribe. One deadlock remains possible. Two users whose batches each subscribe to several new or shared feeds in opposite orders can deadlock in steps 3 and 4. Postgres aborts one request with a `500`, and that client retries it.

### 4.5 Read endpoints

These serve the online web client. Offline clients don't call them.

| Endpoint | Returns |
| --- | --- |
| `GET /v1/podcasts` | `{ "items": [PodcastItem] }`, every readable podcast in one response |
| `GET /v1/podcasts/{id}` | One `PodcastItem` |
| `GET /v1/podcasts/{id}/episodes?cursor=&limit=` | `{ "items": [EpisodeItem], "next_cursor" }` |
| `GET /v1/inbox?cursor=&limit=` | `{ "items": [InboxItem], "next_cursor" }`, the inbox of section 6 |
| `GET /v1/settings` | `{ "settings": { "<key>": <value> } }` |

A podcast is readable when the user's subscription row for it is `subscribed` with no `replaced_by` and the podcast row exists, the same rule as pull (4.2). Any other id, unsubscribed, unknown or alias-only, is `404 not_found`. A path id that isn't a UUID is `400 bad_request`. Deleted episodes never appear, not in lists, not as a podcast's newest episode.

Each item is the sync entity (4.2) flattened, plus the fields the web client would otherwise need a second request for:

```json
// PodcastItem: a pending podcast is included, with version 0 and null metadata
{ ...podcast, "latest_published_at": "2026-10-20T05:00:00.000Z" }

// EpisodeItem: episode_state is null when the user never touched the episode
{ ...episode, "deleted": false, "episode_state": { ... } | null }

// InboxItem
{ ...episode, "deleted": false, "podcast_title": "...", "podcast_artwork_url": "...",
  "position_ms": 0 }
```

Episodes keep `"deleted": false`, so one episode parser serves sync and the read endpoints. `latest_published_at` is the newest `published_at` among the podcast's live episodes, or `null` when it has none. `GET /v1/podcasts` sorts by it descending, with `null` first, so a fresh subscription shows at the top while it loads, then by id descending. `GET /v1/settings` returns every stored key, unknown ones included, plus every known key (2.4) at its default.

Episode lists and the inbox are ordered by `published_at` descending, then `id` descending, and paginated by keyset. The cursor is opaque to clients. pod-server encodes it as base64url without padding of 24 bytes, the last item's `published_at` as big-endian epoch milliseconds followed by its id, and the next page is every row strictly after it. A cursor the server didn't produce is `400 invalid_cursor`, and the client restarts from the first page. `next_cursor` is `null` on the last page. `limit` defaults to 50 and is clamped to 1..=200. An empty `cursor=` or `limit=` reads as absent.

The web client writes through `POST /v1/sync` without `scopes`.

### 4.6 Directory

`GET /v1/directory/search?q=<term>&limit=25` proxies `https://itunes.apple.com/search?media=podcast&entity=podcast`. The upstream base URL is the `directory_base_url` config key (section 12). It needs a bearer token, so the endpoint isn't an open proxy, although results don't depend on the user. A missing or blank `q` is `400 bad_request`. `limit` defaults to 25 and is clamped to 1..=50. Response, in the directory's relevance order:

```json
{ "results": [ { "title": "...", "author": "..." | null, "feed_url": "https://...",
                 "artwork_url": "https://..." | null, "podcast_id": "9c3e..." } ] }
```

A result without a title, or without a feed URL that normalizes (section 1), is dropped, since a client could neither show nor subscribe to it. Blank strings count as missing. An artwork URL that isn't `http(s)` becomes `null`. `feed_url` is the normalized URL with its original scheme. The server always asks upstream for 50 results and slices after dropping unusable ones, so `limit` counts usable results and one cache entry serves every limit.

The cache holds upstream results for 1 hour per normalized term (trimmed, inner whitespace collapsed, lowercased), at most 1000 terms. Failures aren't cached. The upstream fetch goes through the fetch guard (3.2) with the configured directory host allowed, and has a 10 s timeout. A failure, a timeout or a body that isn't the expected JSON is `502 upstream_unavailable`. The search term is only logged at debug level.

`podcast_id` is the canonical id. The cache holds results before this step, because aliases change when feeds move. On every request the server looks each `feed_url` up in `podcast_alias` and falls back to computing it (section 1). Clients mark results they're already subscribed to with a local lookup, which then also works for a directory URL that is an old alias of a show the user follows.

## 5. Client sync engine

Android and desktop implement the same engine. The schema below is SQLite. Android expresses it as Room entities with the same columns.

### 5.1 Local schema

```sql
create table podcast (
    id text primary key, feed_url text not null, status text not null,
    failing_since integer,             -- epoch ms, failing rows only (4.2)
    feed_error text,                   -- the FeedError object as JSON, failing rows only
    title text, author text, description text, link text,
    artwork_url text, language text,
    version integer not null,          -- 0 for a local placeholder
    local_title text,                  -- from directory search, shown while pending
    local_artwork_url text
);

create table episode (
    id text primary key, podcast_id text not null, title text not null,
    summary_html text not null, content_html text not null, link text,
    published_at integer not null,     -- epoch ms
    duration_ms integer, audio_url text not null, audio_type text not null,
    audio_bytes integer, artwork_url text, season integer, number integer,
    version integer not null
);
create index episode_podcast_pub on episode (podcast_id, published_at desc);
create index episode_pub on episode (published_at desc);

create table subscription (
    podcast_id text primary key, feed_url text not null,
    subscribed integer not null, subscribed_at integer not null, subscribed_dev text not null,
    replaced_by text
);

create table episode_state (
    episode_id text primary key, podcast_id text not null,
    position_ms integer not null default 0, position_at integer, position_dev text,
    played integer not null default 0, played_at integer, played_dev text,
    archived integer not null default 0, archived_at integer, archived_dev text
);
create index episode_state_played on episode_state (played, archived);

create table setting (
    key text primary key, value text not null,  -- JSON
    value_at integer not null, value_dev text not null
);

create table sync_scope (
    scope text primary key,            -- 'user' or 'podcast:<id>'
    version integer not null,
    complete integer not null default 0
);

create table outbox (
    mutation_id text primary key,      -- local timestamps are epoch ms everywhere
    coalesce_key text not null,        -- e.g. 'position:<episode_id>'
    payload text not null,             -- JSON mutation
    created_at integer not null,
    attempts integer not null default 0
);
create index outbox_key on outbox (coalesce_key);

create table account (
    id integer primary key check (id = 1),
    server_url text not null, user_id text not null, username text not null,
    device_id text not null, token text not null
);
```

`episode_state` has no foreign key to `episode`. A state can arrive in the user scope before its podcast scope has synced the episode, and must be kept.

### 5.2 Local writes

Every user action runs one local transaction:

1. Write the change to the local row unconditionally, with `at = max(now, row.f_at + 1 ms)` and `dev = own device id`. A local action always wins locally, even two actions in the same millisecond or one made after a pull brought in a value stamped by a device with a fast clock. The merge function is only for pulled rows.
2. Delete any outbox row with the same `coalesce_key` that isn't currently in flight, then insert the new mutation with the same `at` and `dev`.
3. Signal the sync engine. Position writes during playback wait up to 30 s, so playing doesn't cause a request every 5 s. Pause, stop, seek and every other kind of write signal at once (2 s debounce).

Coalesce keys: `position:<episode_id>`, `played:<episode_id>`, `archived:<episode_id>`, `sub:<podcast_id>`, `setting:<key>`.

"In flight" means the row is part of the request currently being sent. The engine tracks those ids in memory. On a response it deletes outbox rows by `mutation_id`, never by key, so a newer coalesced mutation written during the request survives.

### 5.3 Sync loop

```mermaid
sequenceDiagram
    participant UI as UI / player
    participant DB as Local DB
    participant E as Sync engine
    participant S as pod-server

    UI->>DB: local write (state + coalesced outbox row)
    DB-->>UI: observable query updates
    UI->>E: signal (debounced)
    loop until outbox empty and all scopes complete
        E->>DB: read outbox batch + sync_scope cursors
        E->>S: POST /v1/sync {mutations, scopes, limit}
        S->>S: apply mutations (LWW), one txn
        S-->>E: results + changes per scope
        E->>DB: one txn: drop acked outbox rows,<br/>merge rows, advance cursors,<br/>add/remove podcast scopes
    end
    E->>E: notify download reconciler
```

```
sync():                                   -- holds the process-wide sync mutex
    account = read account; if none: return
    pull_only = false
    loop:
        batch  = pull_only ? [] : up to 200 outbox rows ordered by created_at
        scopes = user scope, then podcast scopes that were complete last
                 round, then incomplete ones (fresh deltas before backfill)
        response = POST /v1/sync { mutations: batch, scopes, limit: 500 }
            on 401: stop, keep all local data, raise "signed out" state
            on 400 with a non-empty batch: increment attempts on the batch,
                drop rows with attempts >= 5 (log them), set pull_only, continue
            on network error / 5xx / 429: backoff (5 s * 2^n, max 15 min), return
        in one local transaction:
            if account row is missing or its user_id differs: abort, return
            for each result: delete the outbox row by mutation_id; if the
                result's at differs from the sent at and the local field still
                carries the sent (at, m.dev): set the local *_at to the
                result's at (section 4.4 clamping)
            for every pending outbox mutation that targets a podcast id
                replaced in this page: rewrite its podcast_id and coalesce
                key to the canonical id (5.5)
            merge user scope rows (5.4), set sync_scope.user
            for each received subscription row with replaced_by: rewrite the
                placeholder (5.5), whatever the merge decided
            reconcile scopes against the merged subscription table:
                every subscribed = 1 podcast has a sync_scope row (insert at 0)
                every subscribed = 0 podcast has no sync_scope row, no podcast
                row and no episodes (episode_state rows stay)
            for each ok podcast scope:
                if it has no sync_scope row now (reconcile removed it): skip it
                upsert podcast and episodes, delete episodes marked
                deleted, set sync_scope version and complete
            for not_subscribed scopes: delete the scope row
        if outbox empty (or pull_only) and every scope complete and no new
            scopes: break
    notify download reconciler
```

The cursor update and the row writes share one transaction, so a crash re-applies at most one page. Every write is an idempotent upsert or delete by id, so re-applying is harmless.

The scope rule is stated in terms of the merged subscription table on purpose. A pulled `subscribed = false` row that loses the merge against a newer local re-subscribe must not delete anything. Checking the merged table instead of the received rows makes that automatic.

Podcast pages are applied after the reconcile step, so a page can arrive for a scope that step just removed. That happens when the podcast's unsubscribe is still in the outbox, in a pull-only request or in a batch that didn't include it. The server still serves the scope, because it hasn't seen the unsubscribe. The client must skip such a page. Writing it would bring back catalog rows the scope rule forbids.

The reverse case is harmless. A podcast subscribed locally whose subscribe is still in the outbox gets `not_subscribed`, which deletes the scope row that reconcile kept. The podcast then ends the response subscribed with no scope, and the loop's exit test can stop there. The next response's reconcile adds the scope again at 0, which is where a new subscription's scope was anyway.

The result `at` rewrite runs for every result, `superseded` and `rejected` included. It only fires when the local field still carries the sent stamp, and for a mutation that didn't win, the winning row arrives by pull and merges over it.

A local write made while the request was in flight sets the rerun flag. The loop then runs once more before the mutex is released.

### 5.4 Merge function

Clients and server must use the same rule:

```
merge(local, remote) per field f:
    if remote.f_at is null: keep local                              -- nobody wrote it
    elif wins(remote.f_at, remote.f_dev, local.f_at, local.f_dev):  -- 4.4
        local.f = remote.f; local.f_at = remote.f_at; local.f_dev = remote.f_dev
```

A pulled field whose stamp is null was never written on the server, so it never replaces a local field, and `wins` has no case for it. `subscription.feed_url` has no stamp and moves with `subscribed`. `replaced_by` isn't LWW. The pulled value is always taken (5.5). A pulled row with no local row is inserted as it came.

Pulled rows go through `merge`, never a plain overwrite. A local edit still sitting in the outbox carries a newer `at` than the server row, so the pull leaves it in place. That fixes the v1 TUI bug where pulled progress replaced unpushed local progress.

Local user actions don't go through `merge`. They overwrite (5.2).

Clamping stays symmetric because of the result `at` rewrite in 5.3. A device whose clock runs an hour fast sends `at = now + 1 h`. The server stores `now + 60 s` and returns it. The device rewrites its local `*_at` to the same value. A later edit from another device then wins on both the server and the fast device's next pull.

Device ids compare as their lowercase hyphenated strings, which works for UUIDv7 and matches comparing the 16 bytes unsigned. Kotlin implementations must not use `java.util.UUID.compareTo`. It compares the two 64-bit halves as signed numbers, so an id starting with `8` to `f` sorts before one starting with `0` to `7`. `conformance/lww.json` pins this.

### 5.5 Offline subscribe and placeholders

Subscribing locally:

1. Normalize the URL (section 1). If that fails, show the error inline and stop. No placeholder, no mutation.
2. `podcast_id = uuidv5(NS_PODCAST, normalize(feed_url))`.
3. Insert a `podcast` row with `status = 'pending'`, `version = 0`, `local_title` and `local_artwork_url` from the directory result if any.
4. Insert the subscription row and the outbox mutation. Insert `sync_scope 'podcast:<id>'` at version 0.

If a pull returns a subscription row X with `replaced_by = Y`, the client acts on it whatever the merge decided:

1. Carry X's local subscription fields (`subscribed`, `subscribed_at`, `subscribed_dev`, and `feed_url` with them) into Y's local row, but only when X's stamp is strictly newer than Y's, the rule the server uses for the same carry (4.4, "Alias redirect" step 1). On an exact tie Y keeps its fields. This step must not use `merge`, whose `wins` gives a same-device tie to the newer write. Here a same-device tie means one device sent a mutation for X and a later one for Y, and the server clamped both into the same millisecond. The server applied them in batch order, so its Y reflects the later mutation, and Y's pulled row already shows that. Letting X win the tie would undo it. If Y has no local row yet, X's fields become Y's row. The server's convergence test pins this case (`a_clamped_alias_redirect_tie_keeps_the_canonical_row`).
2. Rewrite pending outbox mutations that name X so they name Y, coalesce key included. A mutation already in flight is redirected by the server (4.4).
3. Copy X's `podcast_device` settings to Y unless Y already has some.
4. Delete X's podcast, episode, `podcast_device`, scope and subscription rows, so nothing stays behind under a dead id. If Y has no podcast row yet and X's is still a placeholder (version 0), X's row is renamed to Y instead of deleted, so the directory title stays in the sidebar until Y's first page arrives. The scope rule in 5.3 then gives Y a scope if it ends up subscribed.

Y's own row may arrive before or after X's. Either way it goes through `merge`, and since the server always re-sends Y's row with a new version on a redirect, both orders end in the server's state for Y. The pulled X row itself carries the epoch stamp (4.4), so its `subscribed = false` never wins a merge on the way.

A podcast is named by its `title`, else the placeholder's `local_title`, else the host of its feed URL (with a non-default port), never the URL itself: a feed the server never fetched has no other name, and the URL may carry credentials and is too long for a sidebar.

While `status = 'pending'`, the UI shows the placeholder with a spinner and says it is waiting for the server to fetch the feed. `failing` shows the reason worded from `feed_error`, how long the feed has been failing (from `failing_since`), that the server keeps retrying, and offers to unsubscribe. `gone` offers to unsubscribe and says the episodes are kept. The sidebar marks `failing` and `gone` podcasts with a warning sign; `pending` ones look like any other there.

If the server answers the subscribe with `rejected: invalid_feed_url` (it can be stricter than the client, for example about blocked addresses, 3.2), the client deletes the placeholder, its scope and its subscription row, and shows "Couldn't add <url>: <reason>". A placeholder that never gets past `pending` this way can't happen. Either the server creates the podcast row and the poller moves it to `ok` or `failing`, or the subscribe is rejected.

Alias replacement (`replaced_by`) is silent. The placeholder or old show disappears and the canonical show takes its place, with this device's podcast settings carried over.

### 5.6 Triggers

| Trigger | Android | Desktop |
| --- | --- | --- |
| App start, account present | Yes | Yes |
| Local write | As in 5.2 | Same |
| Foreground timer | Every 5 min | Every 5 min |
| Connectivity regained | `ConnectivityManager` callback | NetworkManager signal, at once |
| Background | Periodic WorkManager job, 1 h, network required | None |
| Manual refresh | Pull to refresh | Refresh button, `Ctrl+R` |

Clients show one sync status, derived from the engine's state, in the same words on both platforms:

| Status | When |
| --- | --- |
| Synced N min ago | Last loop finished, outbox empty |
| Syncing… | Loop running. During a backfill: "Loading episodes for <show>" |
| Offline · N waiting | No connectivity. N is the outbox size, omitted when 0 |
| Can't reach server · retrying in T | Network error, 5xx or 429 while connected, with the backoff from 5.3 |
| Signed out | Last response was 401 (5.7) |

Rejected mutations and outbox rows dropped after 5 attempts are kept in a small local log (last 50) and shown under the sync status in Settings as "N changes couldn't be saved", each with its reason.

Only one sync runs at a time per process. On Android the foreground engine and the WorkManager job share a process-wide mutex. Logout takes the same mutex (5.7). If the worker runs in a separate process (it shouldn't, keep the default), the unique-work name ensures only one is queued.

### 5.7 Logout and account switch

Logout takes the sync mutex, cancelling any in-flight request first, so no sync response can be applied after it. It then cancels every download job and waits for them to stop (at most 10 s; a transfer checks for cancellation between chunks). Then, in one transaction, it deletes every table except `device_setting`. That includes `setting`, which holds the user's synced settings, and `podcast_device`, which belongs to that user's podcasts. Finally it deletes the downloads directory and calls `POST /v1/auth/logout` best effort. A transfer that outlived the wait finds its row gone at publish time and deletes its own file (7.2). The startup sweep catches anything left. Unsent outbox rows are lost, and the logout dialog says so when the outbox isn't empty. As a second line of defence, the sync apply transaction checks the account row (5.3).

A `401` doesn't log out, and it doesn't block the app. Browsing, playback, downloads and local edits keep working, and the outbox keeps filling. The sync status shows "Signed out" with a sign-in action that opens the password prompt as a sheet. If the same `user_id` logs back in, sync resumes with the existing outbox (the new device id only affects future mutations). A different user goes through logout first.

## 6. Inbox

```sql
SELECT e.*
FROM episode e
JOIN subscription s ON s.podcast_id = e.podcast_id AND s.subscribed = 1
LEFT JOIN episode_state st ON st.episode_id = e.id
WHERE coalesce(st.played, 0) = 0
  AND coalesce(st.archived, 0) = 0
  AND e.published_at >= :now - :window_ms
  AND (
        e.published_at >= s.subscribed_at
     OR e.id = (SELECT e2.id FROM episode e2
                WHERE e2.podcast_id = e.podcast_id
                ORDER BY e2.published_at DESC, e2.id DESC LIMIT 1)
  )
ORDER BY e.published_at DESC, e.id DESC
```

An episode published before the user subscribed counts only if it is the podcast's newest episode. Subscribing to a show then puts its latest episode in the inbox without pulling in 30 days of back catalogue. Older episodes are one tap away in the podcast's episode list.

`:window_ms` is `inbox_window_days * 86_400_000`. A value that isn't a non-negative integer reads as the default of 30, and values above 36,500 (a century) are capped. Both bounds are inclusive. An episode published exactly at the window start, or exactly at `subscribed_at`, is in. When the podcast's newest episode is played or archived, the exception doesn't move to the next one. Ids compare as their lowercase hyphenated strings (5.4).

The server's `GET /v1/inbox` runs the same filter in Postgres, with two additions the client gets for free. It requires `s.replaced_by is null`, because the server keeps redirected rows that clients delete (5.5). It skips deleted episodes, in the list and in the newest-episode subquery, because clients have already removed them. `conformance/inbox.json` pins these rules on a fixed dataset that clients and server both run.

## 7. Downloads

All download data is device-local.

### 7.1 Local schema

```sql
create table podcast_device (
    podcast_id        text primary key,
    notify            integer not null default 0,
    auto_keep         integer not null default 0,   -- keep newest N unplayed, 0 = off
    auto_on_metered   integer not null default 0
);

create table download_wish (
    episode_id text not null,
    source     text not null check (source in ('pin', 'auto')),
    created_at integer not null,
    metered_ok integer not null default 0,  -- pin only: "download on mobile data anyway"
    primary key (episode_id, source)
);

create table download (
    episode_id     text primary key,
    state          text not null,       -- see 7.2
    audio_url      text not null,       -- URL this file was fetched from
    path           text not null,       -- relative to the downloads root
    bytes_total    integer,
    bytes_done     integer not null default 0,   -- durable checkpoint, see 7.3
    validator      text,                -- ETag or Last-Modified for If-Range
    attempts       integer not null default 0,
    next_attempt_at integer,
    last_error     text,
    wait_reason    text,                -- why a queued row isn't running: connection, wifi, space, budget
    completed_at   integer,
    unwanted_since integer,             -- set when no wish covers the row, cleared when one does
    run_id         text                 -- id of the transfer that owns the row and its .part file
);

-- Sizes learned from Content-Length / Content-Range. Kept apart from
-- `download` so cancelling or evicting a row doesn't forget them, and
-- budget fitting doesn't fall back to a smaller guess and retry the
-- same download on the next pass.
create table known_size (
    episode_id text primary key,
    bytes      integer not null
);

create table device_setting (key text primary key, value text not null);
```

Device settings: `download_budget_bytes` (default 2 GiB), `download_on_metered` (default false), `evict_grace_hours` (default 24).

`download_on_metered` gates every download on a metered network, pins included. Someone on a capped plan who turns it off can trust that nothing downloads over mobile data behind their back. A single pin can still go through. A pin waiting for an unmetered network offers "Download on mobile data anyway", which sets `metered_ok` on that one wish.

Paths: `<root>/<podcast_id>/<episode_id>.<run_id>.<ext>`, with the extension from `audio_type`, and `.part` appended while it's in progress. Both names carry the run id, so two runs never share a partial file and a stale run can never rename over a newer run's finished file. `download.path` records which file is the published one. On Android `<root>` is `getExternalFilesDir("audio")`, on desktop the platform data directory (`dirs::data_dir()/pod/audio`, so `$XDG_DATA_HOME/pod/audio` on Linux).

### 7.2 State machine

```mermaid
stateDiagram-v2
    [*] --> queued: wish added
    queued --> transferring: reconciler starts job
    transferring --> complete: body complete, size ok
    transferring --> queued: no connection or disk full
    transferring --> waiting_retry: transient error
    transferring --> failed: permanent error or attempts >= 8
    waiting_retry --> queued: next_attempt_at reached
    failed --> queued: user retry
    complete --> [*]: no wish, grace passed, or budget eviction
    queued --> [*]: wish removed
    transferring --> [*]: wish removed, run cancelled
    waiting_retry --> [*]: wish removed
    failed --> [*]: wish removed
    note right of transferring
        Every row write carries WHERE run_id = :run_id.
        audio_url change from any state resets to queued
        under a new run_id.
    end note
```

| From | Event | To | Side effects |
| --- | --- | --- | --- |
| none | wish exists, episode known | `queued` | insert row |
| `queued` | reconciler starts job | `transferring` | new `run_id`. If the previous run's `.part` exists, its job is dead and `audio_url` is unchanged, rename it to the new run's name and resume from it |
| `transferring` | body complete, size check ok | `complete` | publish (see below) |
| `transferring` | wish removed | none | cancel the run, delete its `.part`, delete the row |
| `transferring` | no connection, or disk full | `queued` | reason recorded, `attempts` unchanged |
| `transferring` | transient error | `waiting_retry` | `attempts += 1`, `next_attempt_at = now + min(30 s * 2^attempts, 6 h)` |
| `transferring` | permanent error, or `attempts >= 8` | `failed` | keep `.part` only if resumable |
| `waiting_retry` | `next_attempt_at` reached | `queued` | |
| `failed` | user taps retry | `queued` | `attempts = 0` |
| any | episode `audio_url` changed | `queued` | new `run_id` (the old run's writes now miss), delete the final file, reset bytes. The old run deletes its own `.part` when its next row write misses |
| `complete` | no wish, not playing, `unwanted_since` older than the grace period | none | delete file and row |

Writes to a `download` row from a transfer must include `WHERE run_id = :run_id`. A transfer whose row was deleted (logout, eviction) or reclaimed by a newer run then updates zero rows, sees that, deletes its own files and stops. That closes the v1 race where a progress write re-created a row after logout. Byte progress for the UI is kept in memory. The row only gets durable checkpoints (7.3).

Publishing a finished transfer:

1. fsync the `.part` file and rename it to the run's final name (drop `.part`). The name is unique to this run, so this can't overwrite anything.
2. In one local transaction: `UPDATE download SET state = 'complete', path = <final name>, completed_at = now WHERE episode_id = ? AND run_id = ?`.
3. If that updated zero rows, the run lost ownership. Delete the final file and stop.

The file only counts once step 2 commits, because readers find files through `download.path`. A crash between steps 1 and 2 leaves a finished file and a `transferring` row with the same `run_id`, which the startup repair (7.4) publishes.

Error classes:

| No connection | Transient | Permanent |
| --- | --- | --- |
| The platform reports no connectivity, or the request fails with a DNS or connect error while it does | Timeouts, IO errors after connecting, connection reset mid-body | 404, 410 |
| | 408, 429, 5xx | other 4xx |
| | | `Content-Type: text/html` on the audio response |

No connection isn't a failure. The row goes back to `queued` with reason "waiting for connection", `attempts` is untouched, and the reconciler doesn't start anything until connectivity returns (7.4). A pin queued on a plane therefore downloads when the plane lands, however long the flight. On Android the transfer jobs carry `NetworkType.CONNECTED`. On desktop the app watches the OS network state (NetworkManager over D-Bus on Linux, `NWPathMonitor` on macOS) and treats a DNS or connect error as no connection.

Disk full isn't per-episode either. The transfer goes back to `queued`, and all downloads pause with reason "storage full" until free space rises above 500 MiB or the user frees space. The reconciler checks again on every pass.

### 7.3 Transfer

Checkpoints. At most every 5 s (and before any pause or cancel), the transfer fsyncs the `.part` file and then writes `bytes_done` to the row. `bytes_done` is therefore never more than what is safely on disk. After a crash the file may hold more bytes than the checkpoint, and the tail may be garbage if the OS hadn't flushed it.

1. If a `.part` exists with `bytes_done > 0` and a `validator`, truncate the file to `bytes_done`, then send `Range: bytes=<bytes_done>-` and `If-Range: <validator>`. Resuming from the file's own length instead could append the same bytes twice or keep a torn tail.
2. `206` appends. `200` truncates and starts over. `416` with a complete `.part` counts as done.
3. Record `bytes_total` from `Content-Range` or `Content-Length`, both on the row and in `known_size`, and `validator` from `ETag` (preferred) or `Last-Modified`.
4. On end of body, verify `bytes_done == bytes_total` when the total is known.

### 7.4 Reconciler

Runs after every sync, after every wish change, on connectivity change, on playback stop, and every 15 minutes while any row is `waiting_retry`.

```
reconcile():
    recompute auto wishes:
        for each podcast_device with auto_keep = N > 0:
            target = newest N episodes of the podcast with played = 0 and archived = 0
            add 'auto' wishes for target, remove 'auto' wishes for the rest
    clear finished pins:
        remove 'pin' wishes whose episode has played = 1 and
        played_at > the wish's created_at
        (finishing an episode releases its pin, on this device or any
         other; pinning an already played episode sticks until it is
         played through again)
    remove wishes for episodes that no longer exist locally
        (unsubscribing a podcast therefore removes its downloads; the
         unsubscribe dialog says so when any are pinned)

    -- Fit the wish set to the budget before downloading anything, so
    -- the reconciler never downloads a file it will evict on the next pass.
    size(e)  = known_size if present, else audio_bytes, else 100 MiB
    pinned_extra = the playing episode's file, if it is outside the wishes
    target   = all pinned episodes
    for each auto-wished episode, newest published first:
        if size(target) + size(e) + pinned_extra <= download_budget_bytes:
            add e to target
        else: mark e "skipped: storage budget" in the UI
    protected = target + the episode currently playing

    for each download row:
        if in protected: clear unwanted_since
        elif unwanted_since is null: set unwanted_since = now
    for each transferring row not in protected: cancel run, delete .part and row
    for each queued / waiting_retry / failed row not in protected: delete row
    for each complete row not in protected with unwanted_since older than
            evict_grace_hours: delete file and row

    for each target episode without a download row: insert queued
    if no connectivity: set wait_reason = 'connection' on queued rows, stop
    if storage full: set wait_reason = 'space', stop
    for each queued row, while running transfers < 2 and network allows:
        (network allows = unmetered, or download_on_metered, or the
         pin wish has metered_ok, or the row is auto-only and
         auto_on_metered is set for that podcast)
        -- Count what is really on disk: complete files, files waiting out
        -- their grace period, the playing file and every .part file.
        while disk_usage() + remaining_bytes(row) > download_budget_bytes
              and some complete file outside protected exists:
            delete the one with the oldest unwanted_since, grace or not
        if disk_usage() + remaining_bytes(row) > download_budget_bytes
              and the row isn't pinned: leave it queued with wait_reason =
              'budget', stop starting transfers
        else: start transfer
```

`disk_usage()` sums the sizes of the files that exist, not the wishes. The grace period protects a file only while there is room for it. When a new download needs the space, files outside the target go first, oldest first.

A row held back by the network check gets `wait_reason = 'wifi'`.

Pins are never skipped or evicted for budget reasons. They end when the user removes them or when the episode is played through, after which the file is an ordinary unwanted file with a grace period. When pins alone exceed the budget the app warns and keeps downloading pins, and every auto wish is skipped.

The episode being played is protected from eviction but never downloaded because it's playing. Streaming doesn't fetch the file a second time, and a cellular connection isn't used for a download nobody asked for.

Removing or cancelling a download row that only an auto wish covers archives the episode (an `episode.archived` local write). Otherwise the next pass would recompute the auto wish and fetch it again. The UI says "Archived and removed" with undo.

On Android each transfer is a unique `WorkManager` job named `download:<episode_id>` with `setExpedited` for pins, and the reconciler itself is a unique job named `reconcile`. The job carries only the episode id. On desktop the reconciler and transfers are tasks on the app's tokio runtime. They stop when the app quits and resume from `.part` files on next start.

On start-up the reconciler repairs crash leftovers. A `transferring` row whose run's final file exists with the expected size is published (7.2, step 2), because the crash came between rename and row write. Other `transferring` rows without a live job move to `queued`. A `complete` row whose file is missing is deleted, and the next pass re-queues it if it's still wanted. Files and `.part` files in the downloads root that no row accounts for are deleted.

### 7.5 Artwork

The reconciler also warms the image cache (Coil disk cache on Android, an `artwork/` directory next to `audio/` on desktop) for subscribed podcasts' `artwork_url` and for inbox episodes' `artwork_url`. Artwork follows the same metered-network rule as auto downloads and has its own 200 MiB cap.

### 7.6 Notifications (Android)

Desktop has no new-episode notifications in v2. It only syncs while open, and the sidebar counts already show what's new. `podcast_device.notify` is ignored there and the desktop UI doesn't show it.


After a background sync, episodes inserted by that sync for podcasts with `podcast_device.notify = 1` and `published_at` within the last 48 hours produce one grouped notification. "Inserted by that sync" comes from the apply transaction, which returns new episode ids. It's safe to use the return value here because only one sync runs at a time per device (5.6), unlike v1 where the foreground app and the worker raced on the same cursor.

## 8. Playback contract

Both platforms implement:

- **Source.** Play the local file if the `download` row is `complete` and the file exists, else stream `audio_url`. Playing never starts a download. The reconciler won't evict the file being played (7.4).
- **Start position.** `episode_state.position_ms`, unless `played` is true. In that case start at 0 and write `episode.played = false` once, as its own mutation (coalesce key `played:<id>`). Position ticks never touch `played`, so coalescing them can't lose the reopen.
- **Position writes.** Every 5 s while playing, plus on pause, seek, stop, episode change and service destroy. Each is a local write (5.2).
- **Played.** When the remaining time drops below `played_threshold_ms` or 2 % of duration, whichever is smaller, write `episode.played = true` once.
- **Settings.** Apply `playback_speed`, `skip_back_ms` and `skip_forward_ms` from the synced settings. Changing one from the player (for example long-pressing a skip button) writes the synced setting, so it changes on every device.
- **Episode removed while playing.** A sync can delete the playing episode's row (unsubscribed on another device, or an admin purge). The player keeps the metadata it started with and plays to the end of the current file or stream. Position and played writes still go to `episode_state`, which has no foreign key to `episode`. The file stays protected while it plays and is reclaimed afterwards.
- **Restore on cold start.** The last episode id and position are kept in `device_setting` (`last_episode_id`, written with every position write). On start the mini player shows that episode, paused at its position. A restored episode that no longer exists locally is dropped.
- **Android.** The `MediaSessionService` owns all of this and writes through the local store, so it works with no activity alive. The view model observes the store and session extras and doesn't write listening state itself.
- **Desktop.** A player entity owns a `libmpv` handle and applies the same rules. Position writes go to the store on the tokio side, never from the render path. Media keys and now-playing metadata go through `souvlaki`.
- **Sonos.** A session handed to a Sonos group follows the same rules, applied by pod-server, and the client's own player writes nothing meanwhile (section 13).

## 9. Conformance fixtures

A `conformance/` directory in the server repo holds JSON cases that both client implementations and the server run. `conformance/README.md` defines each file's format and the rules a runner must follow. Where the README and an implementation disagree, the fixture decides. The file name picks the runner, and a runner fails on any file no runner claims.

| File | Kind | Covers |
| --- | --- | --- |
| `ids.json` | Id table | Feed URL normalization, rejected URLs, episode keys, UUIDv5 ids (section 1). Expected ids come from Python's `uuid.uuid5` via `fill_ids.py`, not from the code under test. |
| `lww.json` | Rule tables | `wins`, the clamp, and server-side batch `apply` for one field (4.4). These are server rules a client transition can't show, such as two mutations of one batch clamped to the same millisecond. |
| `sync_merge.json` | Client transitions | Pulled user-scope rows merging into local rows, and the scope rows that follow subscriptions (5.3, 5.4). |
| `sync_clamp.json` | Client transitions | Outbox deletion and the result-`at` rewrite (5.3), including a mutation stamped with a device id from before a re-login. |
| `client_sync.json` | Client transitions with steps | Backfilling a new subscription, unsubscribes, deleted episodes, the account check after logout (5.3, 5.7), `replaced_by` and `invalid_feed_url` (5.5), coalescing during a request (5.2), and the replay rule (8). |
| `inbox.json` | Query on a fixed dataset | The inbox of section 6, run by clients on SQLite and by the server through `GET /v1/inbox`. |
| `downloads.json` | Client decision step | One reconciler pass (7.4) over local tables and conditions, then a second pass that must change nothing. |

A `sync_*.json` case has a `name` that states the rule as a sentence, `local_before`, `server_response` and `local_after`. Ids are real UUIDs and timestamps are RFC 3339 strings with three fraction digits. `server_response` is a complete `POST /v1/sync` response body, so fixtures double as wire examples. `local_before` lists complete rows. `local_after` is a partial expectation. Only the tables it names are checked, rows match one to one by primary key, and only the listed columns are compared. The first case of `sync_merge.json` pins the v1 TUI bug from 5.4. A pulled position older than the local one leaves the local position and its unsent mutation in place.

Required cases, at minimum:

- Feed URL normalization and UUIDv5 ids (a table of inputs and expected ids).
- Merge function ties, nulls and clock clamping, including the result-`at` rewrite for a fast device, two same-device mutations clamped to one millisecond, and an outbox mutation stamped with a device id from before a re-login.
- Pull with a new subscription adds a scope at 0 and fetches its history in the same loop.
- Pull with an unsubscribe removes the scope, podcast and episodes and keeps episode states.
- Pulled unsubscribe that loses the merge to a newer local re-subscribe deletes nothing.
- Sync response applied after logout changes nothing.
- `replaced_by` rewrite of a placeholder, including when the merge keeps the local row, with X's and Y's rows in either order, and with a pending unsubscribe for X in the outbox.
- Deleted episode removal.
- Outbox coalescing while a request is in flight, and replaying a played episode (the `played = false` mutation survives later position ticks).
- Inbox query on a fixed dataset.
- Auto-download wish computation and budget fitting on a fixed dataset, checking that a second pass changes nothing, including after a cancelled transfer whose size was only learned from the server.

The Rust reference store fails on a response with podcast scopes or a `replaced_by` row, so such a case can't pass by accident. Cases that need those, local writes or more than one response live in `client_*.json`, which only client runners read. Their `steps` replace the single `server_response`: `send` builds the request a loop round sends and marks its batch in flight, `receive` applies the fixture's response to it (and can assert whether the loop goes on), `write` is a user action at a given clock, `logout` is logout's table wipe, and `expect` checks the tables between steps.

Server-side, a property test (`crates/pod-server/tests/convergence.rs`) runs concurrent ingests and mutation batches against a real Postgres and checks that a client pulling at random moments ends up with exactly the server's rows. The pulls must include pull-only requests, which take no lock and are the case section 2.1's snapshot rule protects. That test is the guard for section 2.1. It runs 5 ingest actors and 6 simulated clients (3 users with 2 devices each, one device per user 120 s fast) that subscribe through alias URLs and sync through full loops, single rounds, pull-only and push-only requests with small limits. After a final drain every client must equal the server's rows, every `*_at` and `*_dev` included. The oracle reads the server with plain SQL, not through pull, so a pull bug can't hide itself. The client model follows this spec, not the server code. A test hook can run the pull under `READ COMMITTED` and commit a write between two table reads, which shows the divergence the snapshot rule prevents. Seeds print on every run and failure (`POD_CONVERGENCE_SEED`). CI runs it for 10 s, and `POD_CONVERGENCE_SOAK_SECS` runs it longer.

## 10. Implementation order

Each step ends in something runnable.

1. **Server core.** Schema, ids, ingest with fixtures from real feeds, poller, auth, `POST /v1/sync`. Conformance fixtures and the concurrency property test. Done on branch `v2`, together with the read endpoints, the directory proxy, housekeeping and the admin CLI.
2. **Desktop client core.** Local schema, store change events, sync engine, outbox, inbox and episode views, libmpv playback with position writes. It reuses `pod-protocol` directly, so it's the cheapest client to prove the protocol end to end.
3. **Desktop downloads.** Wishes, reconciler, resumable transfer, storage settings.
4. **Android client.** Room schema, sync engine shared between foreground and worker, service-owned playback writes, downloads through WorkManager, notifications.
5. **Web client.** Read endpoints and writes via `/v1/sync`.
6. **Sonos.** Server first: topology, sessions and the tracker, the endpoints (13). Then the output picker and remote mode in the web client, desktop and Android, in that order.

## 11. Stack

| Part | Choice |
| --- | --- |
| Server | Rust, axum, sqlx (Postgres), feed-rs, reqwest, tracing, eyre, jiff |
| Desktop | Rust, gpui-kit, rusqlite, tokio, reqwest, libmpv2, souvlaki |
| Shared Rust | `pod-protocol` crate: wire types, id derivation, merge function, conformance runner |
| Android | Kotlin, Compose, Room, Media3, WorkManager, OkHttp, kotlinx.serialization, Coil |
| Web | Static HTML and JS served by the server, as today |
| Deploy | A self-hosted NixOS module: one server process and a local Postgres, reachable over the tailnet behind nginx |

The Android wire types come from an OpenAPI document generated from `pod-protocol` (via `utoipa`), so the Kotlin client can't drift from the server types. v1 kept `openapi.yaml` by hand. The document is `openapi.json` at the server repo root. `cargo test` fails when it is stale, and `POD_UPDATE_OPENAPI=1 cargo test -p pod-protocol --test openapi` regenerates it. `utoipa` can't express the internally tagged unions (`Mutation`, the podcast scope entries), so their schemas are written by hand, and a test validates sample bodies against the generated document.

## 12. Server configuration and operations

Configuration is a TOML file (`pod.toml`, or the path in `POD_CONFIG`), overridden key by key by `POD_<KEY>` environment variables. The file parser rejects unknown keys, so a v1 config fails at startup instead of being half-read. `pod.example.toml` documents every key.

| Key | Default | Meaning |
| --- | --- | --- |
| `database_url` | `DATABASE_URL` | Postgres connection string. Falls back to plain `DATABASE_URL` when neither the key nor `POD_DATABASE_URL` is set. |
| `base_url` | none, required | Public URL of the server. Feed hosts see it in the poller's `User-Agent` (3.2), where a wrong default would go unnoticed. |
| `listen_addr` | `0.0.0.0:3000` | Address the HTTP server binds. |
| `poll_interval_secs` | 900 | `poll_interval` of 3.4. |
| `poller_workers` | 4 | Poller workers (3.1). |
| `fetch_allowlist` | `[]` | Host names, IP addresses or CIDRs the fetch guard lets through although they are internal, on any port (3.2). A bad entry fails at startup. |
| `allow_registration` | `true` | `false` makes register answer `403 registration_disabled`. |
| `client_ip_header` | unset | Header holding the client IP behind a reverse proxy, e.g. `X-Real-IP` (4.1). Unset, the auth rate limit keys on the TCP peer. |
| `trusted_proxies` | `["127.0.0.0/8", "::1/128"]` | IP addresses or CIDRs of the proxies whose `client_ip_header` counts (4.1). A bad entry fails at startup. |
| `sonos_hosts` | `[]` | Sonos speakers to ask for the household topology, host names or IP addresses on port 1400 (13.1). Empty turns Sonos off. |
| `directory_base_url` | `https://itunes.apple.com` | An iTunes Search API compatible service, with `/search` appended (4.6). Its host and port are trusted, so a mirror on the LAN works without `fetch_allowlist`. |

The server applies pending migrations on startup, before it binds.

A daily housekeeping job, first run a minute after startup, deletes expired sessions (2.3) and old `applied_mutation` rows (2.4). Neither table syncs, so these deletes take no version. A failed run is logged and retried the next day.

`pod-server admin <command>` runs one operator command against the configured database and exits without serving. It applies pending migrations first. Results go to stdout and logs to stderr. A failure (unknown episode or user, a password that breaks the register rules) exits with status 1.

| Command | Effect |
| --- | --- |
| `list-users` | Id, username, `created_at` and live session count per user |
| `purge-episode <episode_id>` | Sets `deleted` on the episode with a new podcast version, so every client removes it (3.3 says when it comes back) |
| `reset-password <username>` | Reads the new password from one line of stdin, so it stays out of argv and shell history, and deletes every session of that user |

## 13. Playback on Sonos

A user can hand an episode to a Sonos group from any client and control it there, the way Spotify hands playback to a speaker. pod-server drives the speakers over their local UPnP interface and records listening state for them, so the position syncs while every client is closed.

Pod is not a Sonos music service, so it doesn't appear for browsing in the Sonos app. S2 firmware 80 and later only accepts music services registered through Sonos's developer portal, and those must be reachable from the internet. The speakers' local registration page (`/customsd.htm`) answers `403`. Local UPnP control is unaffected.

The rules below that describe speaker behaviour were observed on S2 firmware 97.2 (an Era 100, and a Beam with two satellites).

### 13.1 Players

The `sonos_hosts` config key (section 12) names one or more speakers. pod-server asks the first one that answers for the household topology (`ZoneGroupTopology#GetZoneGroupState` on port 1400). Any speaker returns every group, its coordinator's UUID (`RINCON_…`), its name and each member's address, so one reachable seed is enough. The server doesn't use SSDP, because host firewalls commonly drop the unicast replies to a multicast search. Satellites (surrounds, a sub) belong to their group and are not players of their own.

A player is a group, identified by its coordinator's UUID and named like the Sonos app names it ("Couch", or "Couch + Küche" for a grouped pair). Every command goes to the coordinator. The server caches the topology for 10 seconds and refetches it early when a command or poll fails.

### 13.2 API

| Endpoint | Body | Returns |
| --- | --- | --- |
| `GET /v1/sonos/players` | | `{ "players": [Player] }` |
| `POST /v1/sonos/players/{id}/play` | `{ "episode_id", "position_ms" }` | `Player` |
| `POST /v1/sonos/players/{id}/command` | a command, below | `Player` |

```json
// Player
{ "id": "RINCON_804AF246458401400", "name": "Couch", "volume": 10,
  "state": "idle" | "other" | "pod",
  "session": { "episode_id": "…", "playing": true, "position_ms": 683000 } | null }
```

`state` is `pod` when the requesting user has a session (13.3) on the player, and `session` is set only then. `other` means the group plays or holds something else, including another user's Pod session. `volume` is the group volume, 0 to 100. With `sonos_hosts` empty the list is empty and clients hide Sonos. A player id that isn't a current coordinator is `404 not_found`. A speaker that doesn't answer or refuses a command is `502 upstream_unavailable`.

`play` starts a session. The episode must be readable by the user (4.5), else `404 not_found`. The client sends `position_ms`, worked out with the start rule of section 8 from its own local state, so a position it hasn't synced yet still counts. The server loads the episode on the group, seeks, and plays (13.3). Whatever the group was doing stops, another user's Pod session included, which ends as in 13.3.

Commands, all requiring the user's own session on the player except `volume` (otherwise `409 no_session`):

| `type` | Fields | Effect |
| --- | --- | --- |
| `pause` | | Pause the group |
| `resume` | | Play from the current position at 1× |
| `seek` | `position_ms` | Seek to a position |
| `skip` | `delta_ms` | Seek relative to the speaker's current position, clamped at 0. Clients send `skip_back_ms` negated or `skip_forward_ms` |
| `volume` | `volume` | Set the group volume, 0 to 100 |
| `release` | | Pause the group, write the position, end the session |

`release` answers `{ "episode_id", "position_ms", "playing" }` instead of a `Player`: the session as it was when released, so the client continues locally from that position, paused or playing.

The wire types live in `pod-protocol` and in `openapi.json` like every other endpoint's.

### 13.3 Sessions

```sql
create table sonos_session (
    player_id      text primary key,     -- the coordinator's UUID
    user_id        uuid not null references users(id) on delete cascade,
    episode_id     uuid not null,
    audio_url      text not null,        -- the URI sent to the speaker
    device_id      uuid not null,        -- *_dev of this session's writes, UUIDv7 per session
    duration_ms    bigint,               -- from the episode row at start
    position_ms    bigint not null,      -- last position observed while playing or paused
    played_written boolean not null default false,
    started_at     timestamptz not null default now()
);
```

A player has at most one session. Sessions live in Postgres so a server restart resumes them. On startup the server polls each one once and drops it unless the group still holds its URI.

Starting:

1. `SetAVTransportURI` with `CurrentURI` = the episode's `audio_url` and DIDL-Lite metadata (`dc:title`, `dc:creator` = the show, `upnp:albumArtURI`), so the Sonos app shows the episode.
2. `Seek` with `REL_TIME` to `position_ms`, before playing. The speaker accepts it while stopped, so playback doesn't start at 0:00 and jump. If it refuses, seek once it plays.
3. `Play` at speed 1. Sonos refuses any other speed (UPnP error 717), so `playback_speed` doesn't apply on Sonos.
4. Insert the session row, replacing a previous one on the player after ending it.

A tracker polls each session's coordinator every 5 seconds with `GetTransportInfo`, `GetPositionInfo` and `GetMediaInfo`. The speaker reports `RelTime` in whole seconds and `TrackDuration` as `0:00:00` for these streams, so the duration comes from the episode row. The group still holds the session's episode while `CurrentURI` equals `audio_url`. Each poll is one step of a pure function from the session row and the observation to writes and the next row:

| Observation | Effect |
| --- | --- |
| `PLAYING`, URI ours | Write the position. Write `played = true` once when the remaining time (by `duration_ms`) drops below `played_threshold_ms` or 2 % of the duration, whichever is smaller |
| `PAUSED_PLAYBACK`, URI ours | Write the position once on the change from playing, then nothing. The session stays, since the Sonos app can resume it |
| `TRANSITIONING` | Nothing |
| `STOPPED`, URI ours, after `PLAYING` | The episode ended. Write `played = true` if not yet written and end the session. The speaker reports position 0 at this point, which is never written |
| URI not ours | The group changed source (the Sonos app, another user's Pod session, TV input). End the session. Its last position is already written |
| Player is no longer a coordinator | The group was merged into another. End the session |
| No answer | Retry. After 2 minutes without an answer, end the session |

Feed durations are approximate. One observed episode played 24 seconds past its feed's `duration_ms` before stopping, which is why the end of an episode is recognised by the `STOPPED` transition and not by position.

Writes take the batch path of section 4.4: lock the `users` row, then `wins` and `write` for each field, stamped with `at` = the observation time and `dev` = the session's `device_id`. They aren't recorded in `applied_mutation`, since nothing replays them. Paused sessions write nothing, and a playing one writes at most every 5 seconds, so a session costs one small transaction per poll.

Commands that change the speaker (`pause`, `seek`, `skip`, `release`) also update the session row at once and write the position, so the next poll doesn't see a stale jump.

Every authenticated user can see and control every player. A household shares its speakers, and the server is meant for one household. The speaker fetches `audio_url` itself, so the fetch guard (3.2) doesn't apply to it. It only ever receives the `audio_url` of an episode the user can read.

### 13.4 Clients

- **Output picker.** The player offers "This device" and each player from `GET /v1/sonos/players`, fetched when the picker opens. Clients without network access or with an empty list don't show it.
- **Handing off.** Choosing a player pauses the local player (an ordinary pause write, section 8), then calls `play` with the start position of section 8. A played episode starts at 0 and writes `played = false` locally first, as the replay rule says.
- **Remote mode.** While the user's session plays on a player, the local player is stopped and writes nothing, so two writers can't fight over the position. The player UI shows the session from `GET /v1/sonos/players`, polled every 2 seconds while visible and every 15 seconds otherwise. Its buttons send commands. The skip buttons send `skip` with the synced skip settings, and the speed control is hidden.
- **Taking it back.** "This device" sends `release` and starts the local player at the returned position, paused or playing as the session was. Starting there isn't a replay.
- **Session ending elsewhere.** When the player's `state` stops being `pod` (the episode ended, the Sonos app switched source, another user took the group), the client leaves remote mode and requests a sync (5.6) to pull the final position and played state.
- **Media sessions.** Lock screen and MPRIS controls follow the local player only. TODO: drive a remote session from them (a Media3 `SimpleBasePlayer` on Android, souvlaki on desktop).
