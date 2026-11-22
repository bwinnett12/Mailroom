#!/usr/bin/env bash
# relink-check.sh — the declarative link mode (link = "replace"), end to
# end on a throwaway vault: re-sending a recipe makes its file's links
# the whole truth, and an unchanged re-send writes nothing.
#
# Run from the Mailroom repo root, inside the dev shell:
#     bash relink-check.sh
# Your real vault and the systemd service are never touched.

set -u
REPO=$(pwd); WORK=$(mktemp -d); V=$WORK/vault; PORT=3995; URL=http://127.0.0.1:$PORT
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

[hops.relink]
link = "replace"

[hops.store]
store = true

[[routes]]
data_type = "recipe"
dodo = "cook"
nest = "60-A"
hops = ["relink"]

[[routes]]
data_type = "recipe/additive"
dodo = "cook"
nest = "60-A"
hops = ["link"]
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
    "{\"source\":\"manual\",\"data_type\":\"${DT:-recipe}\",\"payload\":{\"kind\":\"text\",\"data\":\"$key\"},\"meta\":{\"key\":\"recipe/$key\"},\"links\":[$links]}"
}
post() { curl -s -w '\n%{http_code}' -X POST "$URL$1" -H 'content-type: application/json' -d "$2"; }
code() { echo "$1" | tail -1; }
has_edge() { curl -s "$URL/graph/node?id=$1" | grep -q "\"label\":\"$2\",\"seq\":[0-9]*,\"to\":\"$3\""; }
ok() { curl -s "$URL/graph/check" | grep -q '"ok":true'; }

kinds() { grep -c "\"kind\":\"$1\"" "$V/.mailroom/ledger/events.jsonl"; }

echo "1. first import"
start run1
recipe soup onion salt butter-1
recipe stew onion beef
check "soup has 3 ingredients"                       'has_edge recipe/soup USES ingredient/butter-1 && has_edge recipe/soup USES ingredient/salt'
check "check passes"                                 'ok'

echo "2. re-import after fixing the file (butter-1 -> butter, salt removed)"
recipe soup onion butter
check "butter-1 edge gone"                           '! has_edge recipe/soup USES ingredient/butter-1'
check "salt edge gone"                               '! has_edge recipe/soup USES ingredient/salt'
check "butter edge added"                            'has_edge recipe/soup USES ingredient/butter'
check "onion kept"                                   'has_edge recipe/soup USES ingredient/onion'
check "exactly 2 Unlinked events"                    '[ "$(kinds unlinked)" = 2 ]'
check "stew untouched"                               'has_edge recipe/stew USES ingredient/beef'
check "soup still VISITED its nest"                  'has_edge recipe/soup VISITED nest:60-A'
check "check passes (live graph = replay)"           'ok'

echo "3. unchanged re-import writes no link events"
L=$(kinds linked); U=$(kinds unlinked)
recipe soup onion butter
check "no new Linked"                                '[ "$(kinds linked)" = "$L" ]'
check "no new Unlinked"                              '[ "$(kinds unlinked)" = "$U" ]'

echo "4. additive route still only adds"
DT=recipe/additive recipe stew carrot
check "stew keeps beef and gains carrot"             'has_edge recipe/stew USES ingredient/beef && has_edge recipe/stew USES ingredient/carrot'

echo "5. restart"
stop; start run2
check "butter-1 still gone after restart"            '! has_edge recipe/soup USES ingredient/butter-1'
check "check passes after restart"                   'ok'

echo
if [ "$FAILS" -eq 0 ]; then echo "all checks passed"; else echo "$FAILS check(s) failed (KEEP=1 keeps the vault and logs)"; fi
exit "$FAILS"
