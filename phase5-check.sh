#!/usr/bin/env bash
# phase5-check.sh — corrections (unlink, retract), the label index, and
# /graph/check, end to end on a throwaway vault.
#
# Run from the Mailroom repo root, inside the dev shell:
#     bash phase5-check.sh
# Your real vault and the systemd service are never touched.

set -u
REPO=$(pwd); WORK=$(mktemp -d); V=$WORK/vault; PORT=3994; URL=http://127.0.0.1:$PORT
FAILS=0; PID=
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
check() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }
stop() { [ -n "$PID" ] && kill "$PID" 2>/dev/null && wait "$PID" 2>/dev/null; PID=; }
cleanup() { stop; [ "${KEEP:-0}" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"; }
trap cleanup EXIT

echo "building…"
cargo build -q 2>/dev/null || { echo "cargo build failed — run 'cargo build' to see why"; exit 1; }

mkdir -p "$V/60_kitchen/60-A_recipes" "$WORK/in"
printf 'id = "60-A"\nname = "recipes"\nkind = "leaf"\naccepts = ["recipe", "doc"]\n' > "$V/60_kitchen/60-A_recipes/nest"
cat > "$V/routes.toml" <<'EOF'
[dodos.cook]
home_nest = "60-A"
available_nests = ["60-A"]

[hops.link]
link = true

[hops.store]
store = true

[[routes]]
data_type = "recipe"
dodo = "cook"
nest = "60-A"
hops = ["link"]

[[routes]]
data_type = "doc"
dodo = "cook"
nest = "60-A"
hops = ["store"]
EOF

start() {
  MAILROOM_VAULT=$V MAILROOM_LISTEN=127.0.0.1:$PORT MAILROOM_CONFIG=/nonexistent MAILROOM_SNAPSHOT_EVERY=1 \
    RUST_LOG=mailroom=info NO_COLOR=1 "$REPO/target/debug/mailroom" > "$WORK/$1.log" 2>&1 &
  PID=$!
  for _ in $(seq 1 40); do curl -s "$URL/health" >/dev/null 2>&1 && return 0; sleep 0.25; done
  return 1
}
recipe() {  # recipe <key> <ingredient>...
  local key=$1; shift; local links="" sep=""
  for i in "$@"; do links="$links$sep{\"label\":\"USES\",\"to\":\"ingredient/$i\"}"; sep=","; done
  curl -s -o /dev/null -X POST "$URL/dispatch" -H 'content-type: application/json' -d \
    "{\"source\":\"manual\",\"data_type\":\"recipe\",\"payload\":{\"kind\":\"text\",\"data\":\"$key\"},\"meta\":{\"key\":\"recipe/$key\"},\"links\":[$links]}"
}
post() { curl -s -w '\n%{http_code}' -X POST "$URL$1" -H 'content-type: application/json' -d "$2"; }
code() { echo "$1" | tail -1; }
has_edge() { curl -s "$URL/graph/node?id=$1" | grep -q "\"label\":\"$2\",\"seq\":[0-9]*,\"to\":\"$3\""; }
ok() { curl -s "$URL/graph/check" | grep -q '"ok":true'; }

echo "1. healthy vault"
start run1
recipe enchiladas chipotle garlic salt
recipe ranch buttermilk garlic salt
recipe cookies butter salt
echo "a stored document" > "$WORK/in/doc.txt"
curl -s -o /dev/null -X POST "$URL/dispatch" -H 'content-type: application/json' \
  -d "{\"source\":\"manual\",\"data_type\":\"doc\",\"payload\":{\"kind\":\"file_path\",\"data\":\"$WORK/in/doc.txt\"}}"
check "check passes"                                 'ok'
check "check re-hashed the stored blob"              'curl -s "$URL/graph/check" | grep -q "\"verified\":1"'
check "label index: 3 recipes"                       'curl -s "$URL/graph/nodes?label=recipe" | grep -q "\"count\":3"'

echo "2. unlink"
R=$(post /graph/unlink '{"from":"recipe/cookies","label":"uses","to":"ingredient/salt","reason":"test"}')
check "unlink accepted (label normalised)"           '[ "$(code "$R")" = 200 ]'
check "edge is gone"                                 '! has_edge recipe/cookies USES ingredient/salt'
check "unlinking it again: 404"                      '[ "$(code "$(post /graph/unlink "{\"from\":\"recipe/cookies\",\"label\":\"USES\",\"to\":\"ingredient/salt\"}")")" = 404 ]'
check "system edge refused: 400"                     '[ "$(code "$(post /graph/unlink "{\"from\":\"recipe/cookies\",\"label\":\"VISITED\",\"to\":\"nest:60-A\"}")")" = 400 ]'
recipe cookies butter salt
check "re-sending the recipe links it again"         'has_edge recipe/cookies USES ingredient/salt'

echo "3. retract"
R=$(post /graph/retract '{"id":"ingredient/garlic"}')
check "retract accepted, 2 edges removed"            '[ "$(code "$R")" = 200 ] && echo "$R" | grep -q "\"edges_removed\":2"'
check "node is gone"                                 '[ "$(curl -s -o /dev/null -w "%{http_code}" "$URL/graph/node?id=ingredient/garlic")" = 404 ]'
check "enchiladas keeps its other links"             'has_edge recipe/enchiladas USES ingredient/chipotle'
check "system node refused: 400"                     '[ "$(code "$(post /graph/retract "{\"id\":\"nest:60-A\"}")")" = 400 ]'
check "check still passes after corrections"         'ok'

echo "4. restart (snapshot after every record)"
stop; start run2
check "garlic still retracted"                       '[ "$(curl -s -o /dev/null -w "%{http_code}" "$URL/graph/node?id=ingredient/garlic")" = 404 ]'
check "graph came from the snapshot"                 'grep -q "from_snapshot=Some" "$WORK/run2.log"'
check "check passes after restart"                   'ok'

echo "5. damage the check must catch"
B=$(find "$V/by-hash" -type f | head -1); chmod u+w "$B"; echo tampered > "$B"
R=$(curl -s -w '\n%{http_code}' "$URL/graph/check")
check "tampered blob: 409 + named"                   '[ "$(code "$R")" = 409 ] && echo "$R" | grep -q "no longer match"'
rm -f "$B"
check "blob removed: not a problem, just absent"     'ok'
sed -i '2s/"seq":2/"seq":99/' "$V/.mailroom/ledger/events.jsonl"
R=$(curl -s -w '\n%{http_code}' "$URL/graph/check")
check "edited ledger line: 409, does not read cleanly" '[ "$(code "$R")" = 409 ] && echo "$R" | grep -q "does not read cleanly"'

echo
if [ "$FAILS" -eq 0 ]; then echo "all checks passed"; else echo "$FAILS check(s) failed (KEEP=1 keeps the vault and logs)"; fi
exit "$FAILS"
