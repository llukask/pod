-- Initial v2 schema, spec section 2.
--
-- Timestamps are stored truncated to whole milliseconds so they round-trip
-- through clients' epoch-ms integers. The server binds every timestamp
-- through a millisecond type; only the `default now()` columns below hold
-- microseconds, and clients never receive or compare those.
--
-- Every syncable row has `version bigint not null`, allocated from its scope
-- owner's `version_counter` (`podcast` or `users`) under a row lock, never
-- from a sequence (spec 2.1).

-- ==============================================================================
-- Catalog (spec 2.2)
-- ==============================================================================

create table podcast (
    id                 uuid primary key,
    feed_url           text not null,
    status             text not null default 'pending'
                       check (status in ('pending', 'ok', 'failing', 'gone')),
    status_message     text,

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
    last_polled_at     timestamptz
);
create index on feed_poll (next_poll_at);

-- Every normalized URL a podcast has been known by, so a subscribe to an old
-- feed URL resolves to the existing podcast (spec 1 and 4.4).
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
-- Sync pull reads a podcast scope by version, the web client lists episodes
-- newest first.
create index on episode (podcast_id, version);
create index on episode (podcast_id, published_at desc);

-- ==============================================================================
-- Users and sessions (spec 2.3)
-- ==============================================================================

create table users (
    id              uuid primary key,
    username        text not null unique,
    password_hash   text not null,
    version_counter bigint not null default 0,
    created_at      timestamptz not null default now()
);

-- One session per logged-in device. The device id lives only as long as its
-- session, so there is no separate device table.
create table session (
    token_hash   bytea primary key,  -- sha256 of the bearer token
    user_id      uuid not null references users(id) on delete cascade,
    device_id    uuid not null unique,
    device_name  text not null,
    created_at   timestamptz not null default now(),
    last_seen    timestamptz not null default now(),
    expires_at   timestamptz not null
);

-- ==============================================================================
-- User state (spec 2.4)
-- ==============================================================================

-- Each mutable field carries an `*_at` (the client action time) and a `*_dev`
-- (device id, tiebreaker) for the last-writer-wins merge (spec 4.4). Every
-- table here is in user scope and indexed by `(user_id, version)` for sync
-- pull.

create table subscription (
    user_id          uuid not null references users(id) on delete cascade,
    podcast_id       uuid not null,  -- no FK: alias rows point at ids with no podcast row
    feed_url         text not null,
    subscribed       boolean not null,
    subscribed_at    timestamptz not null,
    subscribed_dev   uuid not null,
    replaced_by      uuid,           -- server-authoritative, see spec 4.4
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

-- Unknown keys are stored and synced unchanged, so older clients don't drop
-- settings that newer ones write.
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

-- Stored results of applied mutations, so a retried request gets the same
-- answer. Rows older than 30 days are pruned daily; a replay after that is
-- evaluated afresh, which is safe because every mutation is LWW-guarded.
create table applied_mutation (
    user_id      uuid not null,
    mutation_id  uuid not null,
    result       jsonb not null,
    applied_at   timestamptz not null default now(),
    primary key (user_id, mutation_id)
);
