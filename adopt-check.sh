#!/usr/bin/env bash
# adopt-check.sh — drop files into intake nests and take them in: through
# the running server (endpoint, panel, CLI) and directly (CLI, no server).
# Also checks that the older /envelope handler now writes to the ledger.
#
# Run from the Mailroom repo root, inside the dev shell:
#     bash adopt-check.sh
# Your real vault and the systemd service are never touched.

set -u
REPO=$(pwd); WORK=$(mktemp -d); V=$WORK/vault; PORT=3993; URL=http://127.0.0.1:$PORT
FAILS=0; PID=
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
check() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }
stop() { [ -n "$PID" ] && kill "$PID" 2>/dev/null && wait "$PID" 2>/dev/null; PID=; }
cleanup() { stop; [ "${KEEP:-0}" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"; }
trap cleanup EXIT

echo "building…"
cargo build -q 2>/dev/null || { echo "cargo build failed — run 'cargo build' to see why"; exit 1; }
MR=$REPO/target/debug/mailroom

BOOKS=$V/52_media/52-B_books; MOVIES=$V/52_media/52-E_movies; NOTES=$V/34_lore/34-B_journal
mkdir -p "$BOOKS" "$MOVIES" "$NOTES"
export MAILROOM_DEVICE=check MAILROOM_VAULT=$V MAILROOM_LISTEN=127.0.0.1:$PORT MAILROOM_CONFIG=/nonexistent RUST_LOG=mailroom=warn
"$MR" coffer init >/dev/null || { echo "coffer init failed"; exit 1; }
LEDGER=$V/85_coffer/85-C_ledger/check; CONTENTS=$V/85_coffer/85-B_contents

cat > "$BOOKS/nest" <<'N'
id = "52-B"
name = "books"
kind = "collection"
intake = "media/book"

[mailroom]
active = true
requires_auth = false
notify_on_write = false
ai_classify = false
mints_subnests = true
N
printf 'id = "52-E"\nname = "movies"\nkind = "leaf"\nintake = "media/movie"\n' > "$MOVIES/nest"
printf 'id = "34-B"\nname = "journal"\nkind = "leaf"\naccepts = ["text/note"]\n' > "$NOTES/nest"
cat > "$V/routes.toml" <<'R'
[dodos.librarian]
home_nest       = "52-B"
available_nests = ["52-B", "52-E"]

[hops.store]
store = true

[hops.link]
link = "replace"

[[routes]]
data_type = "media/book"
dodo      = "librarian"
nest      = "52-B"
hops      = ["store", "link"]

[[routes]]
data_type = "media/movie"
dodo      = "librarian"
nest      = "52-E"
hops      = ["store"]
R

start() {
  "$MR" > "$WORK/$1.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 40); do curl -s "$URL/health" >/dev/null 2>&1 && return 0; sleep 0.25; done
  echo "server did not start:"; tail -5 "$WORK/$1.log"; exit 1
}
age() { touch -d '-1 minute' "$@"; }   # past the "still copying" window
ok() { curl -s "$URL/graph/check" | grep -q '"ok":true'; }
received() { grep -c '"kind":"received"' "$LEDGER/events.jsonl" 2>/dev/null || echo 0; }

echo "1. drop files, list them"
start run1
echo "pretend this is a whole book" > "$BOOKS/Designing Data-Intensive Applications.pdf"
echo "pretend this is a movie"      > "$MOVIES/Dune.mkv"
echo "still copying"                > "$MOVIES/Arrival.mkv"
echo "partial"                      > "$MOVIES/Big.mkv.part"
age "$BOOKS/Designing Data-Intensive Applications.pdf" "$MOVIES/Dune.mkv"
L=$(curl -s "$URL/adopt")
check "GET /adopt lists 3 files (not the .part)"       '[ "$(echo "$L" | grep -o "\"path\"" | wc -l)" -eq 3 ]'
check "the fresh file is blocked as still copying"     'echo "$L" | grep -q "still be copying"'
check "/graph/check reports 3 waiting, still ok"       'curl -s "$URL/graph/check" | grep -q "\"intake_waiting\":3" && ok'
check "the panel offers a button for 2"                'curl -s "$URL/adopt/panel" | grep -q "Take in 2 file(s)"'
check "listing changed nothing"                        '[ -f "$MOVIES/Dune.mkv" ] && [ ! -d "$CONTENTS/$(sha256sum "$MOVIES/Dune.mkv" | cut -c1-2)" ]'

echo "2. take them in through the server (CLI talks to it)"
BOOKHASH=$(sha256sum "$BOOKS/Designing Data-Intensive Applications.pdf" | cut -d' ' -f1)
OUT=$("$MR" adopt)
check "CLI used the running server"                    'echo "$OUT" | grep -q "via the running Mailroom"'
check "2 taken in"                                     'echo "$OUT" | grep -q "2 taken in, 0 failed"'
CHILD="52-B-${BOOKHASH:0:16}"
check "the book got its own nest"                      '[ -f "$BOOKS/${CHILD}_Designing-Data-Intensive-Applications/nest" ]'
check "its pointer names the hash"                     'grep -q "ref: sha256:$BOOKHASH" "$BOOKS/${CHILD}_Designing-Data-Intensive-Applications/entries/"*.ref'
check "the book's bytes are in the coffer, read-only"  '[ -f "$CONTENTS/${BOOKHASH:0:2}/$BOOKHASH" ] && [ "$(stat -c %a "$CONTENTS/${BOOKHASH:0:2}/$BOOKHASH")" = 444 ]'
check "dropped files are gone from the nests"          '[ ! -e "$BOOKS/Designing Data-Intensive Applications.pdf" ] && [ ! -e "$MOVIES/Dune.mkv" ]'
check "the movie's pointer is in 52-E/entries"         'ls "$MOVIES/entries/"*.ref >/dev/null 2>&1'
check "the book is a graph node (its nest)"            'curl -s "$URL/graph/node?id=$CHILD" | grep -q "\"id\":\"$CHILD\""'
check "the movie is a graph node 52-E/Dune"            '[ "$(curl -s -o /dev/null -w "%{http_code}" "$URL/graph/node?id=52-E/Dune")" = 200 ]'
check "intake nests got a .gitignore"                  'grep -q "^/\*$" "$BOOKS/.gitignore" && grep -q "^!/nest$" "$MOVIES/.gitignore"'
check "still waiting: Arrival + the .part ignored"     'curl -s "$URL/graph/check" | grep -q "\"intake_waiting\":1"'
check "check ok (live graph = replay, blobs verified)" 'ok && curl -s "$URL/graph/check" | grep -q "\"verified\":2"'

echo "3. the older /envelope handler is ledgered now"
R0=$(received)
curl -s -o /dev/null -X POST "$URL/envelope" -H 'content-type: application/json' \
  -d '{"source":"manual","data_type":"text/note","payload":{"kind":"text","data":"hello"},"jd_address":"34-B"}'
check "/envelope wrote a received event"               '[ "$(received)" -eq $((R0 + 1)) ]'
check "and its entry is in 34-B/entries"               'ls "$NOTES/entries/"*.md >/dev/null 2>&1'

echo "4. directly, with no server running"
stop
age "$MOVIES/Arrival.mkv"
echo "pretend this is a whole book" > "$BOOKS/ddia copy.pdf"; age "$BOOKS/ddia copy.pdf"
OUT=$("$MR" adopt)
check "CLI adopted directly"                           'echo "$OUT" | grep -q "adopting directly"'
check "the duplicate book reuses its nest and bytes"   'echo "$OUT" | grep -q "bytes already in the coffer" && [ "$(find "$BOOKS" -maxdepth 1 -type d -name "52-B-*" | wc -l)" -eq 1 ]'
check "Arrival taken in"                               '[ ! -e "$MOVIES/Arrival.mkv" ]'
check "the .part file is left alone"                   '[ -f "$MOVIES/Big.mkv.part" ]'
start run2
check "check ok after restart"                         'ok && curl -s "$URL/graph/check" | grep -q "\"intake_waiting\":0"'

echo
if [ "$FAILS" -eq 0 ]; then echo "all checks passed"; else echo "$FAILS check(s) failed (KEEP=1 keeps the vault and logs)"; fi
exit "$FAILS"
