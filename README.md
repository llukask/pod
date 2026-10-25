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
