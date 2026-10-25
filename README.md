# Pod

A self-hosted podcast server. It polls feeds, stores episodes in Postgres and
syncs subscriptions, playback positions and inbox state with desktop, Android
and web clients. This branch is the v2 rewrite; see `../docs/spec.md`.

## Running locally

```bash
nix develop                       # or install Rust and Postgres 16 yourself
scripts/dev-db.sh start
export DATABASE_URL="$(scripts/dev-db.sh url)"
sqlx migrate run --source crates/pod-server/migrations
POD_BASE_URL=http://localhost:3000 cargo run -p pod-server
curl -i http://localhost:3000/healthz
```

The server applies pending migrations on startup. The `sqlx migrate run` step
is for the build: with `DATABASE_URL` set, sqlx's `query!` macros check every
query against that database. Without it, or with `SQLX_OFFLINE=true`, they
read the committed `.sqlx/` cache instead. After changing a query or a
migration, regenerate and commit the cache (sqlx-cli 0.9, as in `nix
develop`):

```bash
cargo sqlx prepare --workspace -- --all-targets
```

## Configuration

The server reads a TOML file from `POD_CONFIG` (default `pod.toml`; a missing
default file is fine) and then applies environment overrides. See
`pod.example.toml`.

| Key | Env override | Default | Meaning |
| --- | --- | --- | --- |
| `database_url` | `POD_DATABASE_URL`, then `DATABASE_URL` | required | Postgres connection string |
| `base_url` | `POD_BASE_URL` | required | Public URL, sent in the feed poller's `User-Agent` |
| `listen_addr` | `POD_LISTEN_ADDR` | `0.0.0.0:3000` | HTTP listen address |
| `poll_interval_secs` | `POD_POLL_INTERVAL_SECS` | `900` | Seconds between polls of one feed |
| `poller_workers` | `POD_POLLER_WORKERS` | `4` | Concurrent feed poller workers |
| `fetch_allowlist` | `POD_FETCH_ALLOWLIST` (comma-separated) | empty | Host names, IPs or CIDRs the fetch guard allows on private networks |
| `allow_registration` | `POD_ALLOW_REGISTRATION` (`true`/`false`) | `true` | Whether new accounts can register |
| `trust_fly_client_ip` | `POD_TRUST_FLY_CLIENT_IP` (`true`/`false`) | `false` | Rate-limit by the `Fly-Client-IP` header; only behind Fly.io's proxy |
| `directory_base_url` | `POD_DIRECTORY_BASE_URL` | `https://itunes.apple.com` | iTunes-compatible search API for `/v1/directory/search` (`/search` is appended) |

Environment values win over the file. An empty env value counts as unset.
`RUST_LOG` sets the log filter (default `info`).

### LAN fetch allowlist

Feed fetches refuse loopback, private, link-local and other internal
addresses, so a feed URL can't be used to probe the server's network.
`fetch_allowlist` lifts that for feeds self-hosted on a LAN. Each entry is one
of:

- a host name (`nas.lan`): exact match, and every address it resolves to is
  allowed;
- an IP address (`192.168.1.20`): any host resolving to it is allowed;
- a CIDR (`10.0.0.0/8`): any host resolving inside it is allowed.

Ports stay limited to 80 and 443 either way. In the environment, separate
entries with commas: `POD_FETCH_ALLOWLIST=nas.lan,192.168.1.0/24`.

## Docker

```bash
docker build -t pod-server .
docker run --rm -p 3000:3000 \
  -e POD_DATABASE_URL=postgresql://pod:pod@db.example:5432/pod \
  -e POD_BASE_URL=http://localhost:3000 \
  pod-server
```

The build reads the committed `.sqlx/` cache (`SQLX_OFFLINE=true`), so it needs
no database. The image runs `pod-server` as an unprivileged user and listens on
`0.0.0.0:3000` unless `POD_LISTEN_ADDR` says otherwise.

## Deploy

The server deploys to Fly.io with one Postgres cluster (`fly.toml`). The feed
poller runs inside the server process, so the app runs exactly one machine
that never auto-stops. First-time setup:

```bash
fly apps create <app>                 # then set `app` and POD_BASE_URL in fly.toml
fly postgres create --name <app>-db --region fra
fly postgres attach <app>-db --app <app>   # sets the DATABASE_URL secret
fly deploy --ha=false                 # one machine: the poller and rate limits live in it
```

`fly.toml` sets `POD_ALLOW_REGISTRATION=false`. The database starts empty, so
register the first accounts with `fly deploy --ha=false --env
POD_ALLOW_REGISTRATION=true` and redeploy with plain `fly deploy` afterwards.
Set other secrets with `fly secrets set POD_KEY=value`.

Every start applies pending migrations before the server binds its port. A
failed migration makes the process exit, so the `/healthz` check never passes
and `fly deploy` reports the failure. Later deploys are just `fly deploy`.

## Administration

`pod-server admin` runs one operator command against the configured database
and exits without serving. It reads the same config as the server and applies
pending migrations first.

```bash
pod-server admin list-users                    # id, username, created_at, live sessions
pod-server admin purge-episode <episode_id>    # tombstone an episode for every client
pod-server admin reset-password <username>     # reads the new password from stdin
```

`reset-password` reads one line from stdin, so the password stays out of shell
history and the process list, and it signs the user out on every device. A
purged episode stays deleted while its feed keeps carrying it unchanged; it
comes back only if the publisher changes that episode. On Fly.io, run these
with `fly ssh console -C "pod-server admin list-users"`.

The server also runs a daily job that deletes expired sessions and
`applied_mutation` rows older than 30 days.
