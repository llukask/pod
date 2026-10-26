#!/usr/bin/env bash
# Throwaway Postgres cluster for local development and tests.
#
# The cluster lives in tmp/pg (gitignored) and listens only on a unix socket
# inside that directory, so it never clashes with a system Postgres or opens
# a TCP port. Delete tmp/pg to start from an empty database.
#
#   scripts/dev-db.sh start   # init on first use, then start (no-op if running)
#   scripts/dev-db.sh stop    # stop (no-op if stopped)
#   scripts/dev-db.sh url     # print the DATABASE_URL for the cluster
#
# Each checkout or worktree gets its own cluster. Concurrent test runs need
# separate clusters: `#[sqlx::test]` names each test database after the test
# path, so two runs of one suite on one cluster drop each other's databases.
#
# Unix socket paths are capped at about 100 bytes, so a very deep checkout
# makes `start` fail with a bind error.
#
# Typical use: export DATABASE_URL="$(scripts/dev-db.sh url)"
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pg_dir="$repo_root/tmp/pg"
data_dir="$pg_dir/data"

is_running() {
    [[ -d "$data_dir" ]] && pg_ctl status --pgdata="$data_dir" >/dev/null 2>&1
}

case "${1:-}" in
start)
    if is_running; then
        exit 0
    fi
    if [[ ! -f "$data_dir/PG_VERSION" ]]; then
        mkdir -p "$pg_dir"
        # Trust auth is acceptable because the socket sits in a directory only
        # the current user can reach.
        initdb --pgdata="$data_dir" --username=postgres --auth=trust \
            --encoding=UTF8 --no-locale >"$pg_dir/initdb.log"
    fi
    pg_ctl start --pgdata="$data_dir" --log="$pg_dir/postgres.log" --wait \
        --options="-k $pg_dir -c listen_addresses=''" >/dev/null
    ;;
stop)
    if is_running; then
        pg_ctl stop --pgdata="$data_dir" --mode=fast --wait >/dev/null
    fi
    ;;
url)
    echo "postgresql:///postgres?host=$pg_dir&user=postgres"
    ;;
*)
    echo "usage: $0 start|stop|url" >&2
    exit 2
    ;;
esac
