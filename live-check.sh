#!/usr/bin/env bash
# live-check.sh — end-to-end check of the phase 1 ledger and blob store.
#
# Run from the Mailroom repo root, inside the dev shell:
#     bash live-check.sh
#
# Builds the server, then runs it against a throwaway vault in a temp
# directory. Your real vault and the systemd service are never touched.
# Prints PASS/FAIL per check and exits non-zero if anything failed.

set -u
REPO=$(pwd)
WORK=$(mktemp -d)
V=$WORK/vault
PORT=3999
URL=http://127.0.0.1:$PORT
FAILS=0
PID=

pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
check() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }

cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; pkill -f "sleep 31" 2>/dev/null; rm -rf "$WORK"; }
trap cleanup EXIT

echo "building…"
cargo build -q 2>/dev/null || { echo "cargo build failed — run 'cargo build' to see why"; exit 1; }
BIN=$REPO/target/debug/mailroom

# ── Throwaway vault: one nested nest (like Orchard), two routes ─────────────
mkdir -p "$V/52_media/52-H_books" "$WORK/intake"
cat > "$V/52_media/52-H_books/nest" <<'EOF'
id = "52-H"
name = "books"
kind = "leaf"
accepts = ["file/book", "test/slow"]
EOF
cat > "$V/routes.toml" <<'EOF'
[dodos.librarian]
home_nest       = "52-H"
available_nests = ["52-H"]

[hops.store]
store = true

[hops.slow]
run  = "sh"
args = ["-c", "sleep 31"]

[[routes]]
data_type = "file/book"
dodo      = "librarian"
nest      = "52-H"
hops      = ["store"]

[[routes]]
data_type = "test/slow"
dodo      = "librarian"
nest      = "52-H"
hops      = ["slow"]
EOF
head -c 2000000 /dev/urandom > "$WORK/intake/calc3.pdf"
cp "$WORK/intake/calc3.pdf" "$WORK/intake/calc3-renamed-copy.pdf"
EXPECTED=$(sha256sum "$WORK/intake/calc3.pdf" | cut -d' ' -f1)

export MAILROOM_VAULT=$V MAILROOM_LISTEN=127.0.0.1:$PORT MAILROOM_CONFIG=/nonexistent RUST_LOG=mailroom=info

start() {
  "$BIN" > "$WORK/$1.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 40); do curl -s "$URL/health" >/dev/null 2>&1 && return 0; sleep 0.25; done
  return 1
}
stop() { kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null; PID=; }
dispatch() {
  curl -s -m "${3:-10}" -X POST "$URL/dispatch" -H 'content-type: application/json' \
    -d "{\"source\":\"manual\",\"data_type\":\"$1\",\"payload\":$2}"
}
ledger() { cat "$V/.mailroom/ledger/events.jsonl" 2>/dev/null; }
logged() { sed 's/\x1b\[[0-9;]*m//g' "$WORK/$1.log" | grep -q "$2"; }  # log text without colour codes

echo "1. server and ledger"
check "server starts and /health answers" 'start run1'
check "ledger file exists"                '[ -f "$V/.mailroom/ledger/events.jsonl" ]'

echo "2. blob store (same file sent twice, under two names)"
R1=$(dispatch file/book "{\"kind\":\"file_path\",\"data\":\"$WORK/intake/calc3.pdf\"}")
R2=$(dispatch file/book "{\"kind\":\"file_path\",\"data\":\"$WORK/intake/calc3-renamed-copy.pdf\"}")
BLOB="$V/by-hash/${EXPECTED:0:2}/$EXPECTED"
check "both dispatches completed"           '[ "$(echo "$R1$R2" | grep -o "\"Completed\"" | wc -l)" -eq 2 ]'
check "response hash matches sha256sum"     'echo "$R1" | grep -q "$EXPECTED"'
check "exactly one blob, named by its hash" '[ "$(find "$V/by-hash" -type f | wc -l)" -eq 1 ] && [ -f "$BLOB" ]'
check "blob is read-only (444)"             '[ "$(stat -c %a "$BLOB")" = 444 ]'
check "blob bytes match the original"       'cmp -s "$BLOB" "$WORK/intake/calc3.pdf"'
check "two .ref files in the nest's real folder" '[ "$(ls "$V/52_media/52-H_books/entries/"*.ref 2>/dev/null | wc -l)" -eq 2 ]'
check "no stray 52-H_books/ at the vault root"   '[ ! -e "$V/52-H_books" ]'
check "originals left in place"             '[ -f "$WORK/intake/calc3.pdf" ]'

echo "3. one Mailroom per vault"
OUT=$(MAILROOM_LISTEN=127.0.0.1:$((PORT + 1)) timeout 10 "$BIN" 2>&1)
check "a second process on the same vault is refused" 'echo "$OUT" | grep -q "in use by another Mailroom"'

echo "4. crash in the middle of a stop"
dispatch test/slow '{"kind":"text","data":"slow"}' 2 >/dev/null
sleep 0.5
kill -9 "$PID"; wait "$PID" 2>/dev/null; PID=; pkill -f "sleep 31" 2>/dev/null
check "ledger ends with an arrival that never closed" 'ledger | tail -1 | grep -q "\"kind\":\"transition\""'
check "restart succeeds"                    'start run2'
stop
check "restart recorded 1 interrupted stop" 'logged run2 "interrupted=1"'
check "Interrupted event is in the ledger"  'ledger | tail -1 | grep -q "\"kind\":\"interrupted\""'
start run3 >/dev/null; stop
check "next start reports 0 interrupted"    'logged run3 "interrupted=0"'

echo "5. ledger shape"
N=$(ledger | wc -l)
LAST=$(ledger | tail -1 | grep -o '"seq":[0-9]*' | cut -d: -f2)
check "seq runs 1..N with no gaps ($N records)" '[ "$LAST" = "$N" ]'

echo
if [ "$FAILS" -eq 0 ]; then echo "all checks passed"; else echo "$FAILS check(s) failed — logs were in $WORK (deleted on exit; rerun with 'trap - EXIT' to keep them)"; fi
exit "$FAILS"
