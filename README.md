# Pod

A self-hosted podcast server. It polls feeds, stores episodes in Postgres and
syncs subscriptions, playback positions and inbox state with desktop, Android
and web clients. This branch is the v2 rewrite; see `../docs/spec.md`.

## Running locally

```bash
nix develop                       # or install Rust and Postgres 16 yourself
scripts/dev-db.sh start
export DATABASE_URL="$(scripts/dev-db.sh url)"
POD_BASE_URL=http://localhost:3000 cargo run -p pod-server
curl -i http://localhost:3000/healthz
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
| `fetch_allowlist` | `POD_FETCH_ALLOWLIST` (comma-separated) | empty | Hosts the fetch guard allows on private networks |
| `allow_registration` | `POD_ALLOW_REGISTRATION` (`true`/`false`) | `true` | Whether new accounts can register |

Environment values win over the file. An empty env value counts as unset.
`RUST_LOG` sets the log filter (default `info`).
