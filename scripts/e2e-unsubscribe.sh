#!/usr/bin/env bash
# End-to-end proof of the unsubscribe contract: builds and runs pod-server
# against a throwaway database and two locally served feeds, asserts the
# observable behaviour of subscribing, unsubscribing and re-subscribing, then
# drops everything it created.
#
# Requires a reachable Postgres, an admin connection allowed to create the
# throwaway database, jq and python3. Override ADMIN_DATABASE_URL,
# E2E_DATABASE_URL, E2E_DB, E2E_DB_OWNER, POD_PORT or FEED_PORT if the
# defaults below do not fit.
#
# Run it from anywhere: ./scripts/e2e-unsubscribe.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ADMIN_URL="${ADMIN_DATABASE_URL:-postgresql:///postgres?host=/run/postgresql&user=postgres}"
E2E_DB="${E2E_DB:-pod_e2e}"
E2E_DB_OWNER="${E2E_DB_OWNER:-pod}"
E2E_URL="${E2E_DATABASE_URL:-postgres://pod:pod@localhost:5432/$E2E_DB}"
POD_PORT="${POD_PORT:-3999}"
FEED_PORT="${FEED_PORT:-3998}"
API="http://127.0.0.1:$POD_PORT/api/v1"
FEED_ORIGIN="http://127.0.0.1:$FEED_PORT"
WORK="$(mktemp -d)"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "ok: $*"; }

cleanup() {
  [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null
  [ -n "${FEED_PID:-}" ] && kill "$FEED_PID" 2>/dev/null
  wait 2>/dev/null || true
  psql --no-psqlrc "$ADMIN_URL" -q -c "drop database if exists $E2E_DB" >/dev/null 2>&1
  rm -rf "$WORK"
}
trap cleanup EXIT

mkdir -p "$WORK/feeds"
cat >"$WORK/feeds/full.xml" <<XML
<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Full Feed</title><link>$FEED_ORIGIN/full.xml</link>
  <description>Two episodes</description>
  <image><url>$FEED_ORIGIN/cover.png</url><title>Full Feed</title><link>$FEED_ORIGIN/</link></image>
  <item>
    <guid>full-ep-1</guid><title>Episode One</title><pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
    <enclosure url="$FEED_ORIGIN/1.mp3" length="1" type="audio/mpeg"/>
  </item>
  <item>
    <guid>full-ep-2</guid><title>Episode Two</title><pubDate>Tue, 02 Jan 2024 00:00:00 GMT</pubDate>
    <enclosure url="$FEED_ORIGIN/2.mp3" length="1" type="audio/mpeg"/>
  </item>
</channel></rss>
XML
cat >"$WORK/feeds/empty.xml" <<XML
<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>Empty Feed</title><link>$FEED_ORIGIN/empty.xml</link>
  <description>No episodes yet</description>
  <image><url>$FEED_ORIGIN/cover.png</url><title>Empty Feed</title><link>$FEED_ORIGIN/</link></image>
</channel></rss>
XML

python3 -m http.server "$FEED_PORT" --bind 127.0.0.1 --directory "$WORK/feeds" >/dev/null 2>&1 &
FEED_PID=$!

psql --no-psqlrc "$ADMIN_URL" -v ON_ERROR_STOP=1 -q \
  -c "drop database if exists $E2E_DB" \
  -c "create database $E2E_DB owner $E2E_DB_OWNER" \
  || fail "create the throwaway database $E2E_DB"

SQLX_OFFLINE=true cargo build --quiet --manifest-path "$ROOT/Cargo.toml" -p pod-server --bin pod-server \
  || fail "build pod-server"

DATABASE_URL="$E2E_URL" PORT="$POD_PORT" REFRESH_INTERVAL_SECS=86400 \
  "$ROOT/target/debug/pod-server" >"$WORK/server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 100); do
  curl -s -o /dev/null "$API/auth/me" && break
  sleep 0.1
done
curl -s -o /dev/null "$API/auth/me" || fail "server never came up: $(cat "$WORK/server.log")"
pass "server booted and applied migrations against a fresh database"

TOKEN=$(curl -sf -X POST "$API/auth/register" -H 'content-type: application/json' \
  -d '{"username":"e2e","password":"e2e-password"}' | jq -r .token)
[ -n "$TOKEN" ] && [ "$TOKEN" != null ] || fail "register a user"

auth=(-H "Authorization: Bearer $TOKEN")
status() { curl -s -o "$WORK/body.json" -w '%{http_code}' "${auth[@]}" "$@"; }

subscribe() {
  curl -sf -X POST "$API/podcasts" "${auth[@]}" -H 'content-type: application/json' \
    -d "{\"feed_url\":\"$1\"}" | jq -r .id
}

FULL_ID=$(subscribe "$FEED_ORIGIN/full.xml")
EMPTY_ID=$(subscribe "$FEED_ORIGIN/empty.xml")
[ -n "$FULL_ID" ] && [ "$FULL_ID" != null ] || fail "subscribe to the full feed"
[ -n "$EMPTY_ID" ] && [ "$EMPTY_ID" != null ] || fail "subscribe to the empty feed"
[ "$(curl -sf "$API/podcasts" "${auth[@]}" | jq 'length')" = 2 ] || fail "both podcasts listed"
pass "subscribed to two podcasts"

[ "$(status "$API/podcasts/$FULL_ID/episodes")" = 200 ] || fail "episodes while subscribed"
[ "$(jq '.items | length' "$WORK/body.json")" = 2 ] || fail "expected two episodes"
EPISODE_ID=$(jq -r '.items[0].episode.id' "$WORK/body.json")
pass "subscribed podcast with episodes returns 200 with its episodes"

[ "$(status "$API/podcasts/$EMPTY_ID/episodes")" = 200 ] \
  || fail "a subscribed podcast with no episodes must be 200, got $(cat "$WORK/body.json")"
[ "$(jq '.items | length' "$WORK/body.json")" = 0 ] || fail "expected an empty item list"
[ "$(jq '.next_page_token' "$WORK/body.json")" = null ] || fail "expected no next page token"
pass "subscribed podcast with zero episodes returns an empty 200 page, not a 404"

curl -sf -X POST "$API/episodes/$EPISODE_ID/progress" "${auth[@]}" -H 'content-type: application/json' \
  -d '{"progress":321,"done":false}' >/dev/null || fail "report progress"
pass "reported progress on an episode"

preflight=$(curl -s -o /dev/null -D - -X OPTIONS "$API/podcasts/$FULL_ID" \
  -H 'Origin: http://example.test' -H 'Access-Control-Request-Method: DELETE' \
  | tr -d '\r' | grep -i '^access-control-allow-methods:' || true)
case "$preflight" in
  *DELETE*) pass "CORS preflight advertises DELETE" ;;
  *) fail "CORS preflight must advertise DELETE, got '${preflight:-no allow-methods header}'" ;;
esac

[ "$(status -X DELETE "$API/podcasts/$FULL_ID")" = 200 ] || fail "unsubscribe"
pass "unsubscribe returns 200"

[ "$(status -X DELETE "$API/podcasts/$FULL_ID")" = 200 ] || fail "repeated unsubscribe"
pass "unsubscribe is idempotent"

[ "$(curl -sf "$API/podcasts" "${auth[@]}" | jq --arg id "$FULL_ID" 'map(select(.id == $id)) | length')" = 0 ] \
  || fail "unsubscribed podcast still listed"
pass "unsubscribed podcast is absent from GET /podcasts"

[ "$(status "$API/podcasts/$FULL_ID")" = 404 ] || fail "podcast detail after unsubscribe"
pass "GET /podcasts/{id} is 404 after unsubscribe"

[ "$(status "$API/podcasts/$FULL_ID/episodes")" = 404 ] \
  || fail "episodes after unsubscribe must be 404, got $(cat "$WORK/body.json")"
pass "GET /podcasts/{id}/episodes is 404 after unsubscribe"

[ "$(status "$API/episodes/inbox?per_page=50")" = 200 ] || fail "inbox"
[ "$(jq '.items | length' "$WORK/body.json")" = 0 ] || fail "inbox still shows unsubscribed episodes"
pass "inbox drops episodes of the unsubscribed podcast"

[ "$(psql --no-psqlrc "$E2E_URL" -tAc "select count(*) from episode where podcast_id = '$FULL_ID'")" = 2 ] \
  || fail "unsubscribing must not delete the shared episode rows"
[ "$(psql --no-psqlrc "$E2E_URL" -tAc "select count(*) from podcast where id = '$FULL_ID'")" = 1 ] \
  || fail "unsubscribing must not delete the shared podcast row"
pass "shared podcast and episode rows survive the unsubscribe"

[ "$(subscribe "$FEED_ORIGIN/full.xml")" = "$FULL_ID" ] || fail "re-subscribe to the full feed"
[ "$(status "$API/podcasts/$FULL_ID/episodes")" = 200 ] || fail "episodes after re-subscribe"
[ "$(jq --arg id "$EPISODE_ID" '.items[] | select(.episode.id == $id) | .progress' "$WORK/body.json")" = 321 ] \
  || fail "progress must survive an unsubscribe and re-subscribe"
pass "listening progress survives an unsubscribe and re-subscribe"

echo "ALL PASS"
