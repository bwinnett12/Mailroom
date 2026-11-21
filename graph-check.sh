#!/usr/bin/env bash
# graph-check.sh — end-to-end check of the phase 2 graph.
#
# Run from the Mailroom repo root, inside the dev shell:
#     bash graph-check.sh
#
# Starts the server on a throwaway vault, sends the Calc 3 scenario through
# POST /dispatch (course, two sessions, a topic, three notes, a paper), then
# asks the graph questions whose answers were worked out by hand.
# Your real vault and the systemd service are never touched.
#
# Set KEEP=1 to leave the vault and server log behind for poking at.

set -u
REPO=$(pwd)
WORK=$(mktemp -d)
V=$WORK/vault
PORT=3998
URL=http://127.0.0.1:$PORT
FAILS=0
PID=

pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
check() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; [ "${KEEP:-0}" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"; }
trap cleanup EXIT

echo "building…"
cargo build -q 2>/dev/null || { echo "cargo build failed — run 'cargo build' to see why"; exit 1; }

# ── Vault: one nest, one route per kind, all ending in the link hop ─────────
mkdir -p "$V/40_learning/40-A_calc"
cat > "$V/40_learning/40-A_calc/nest" <<'EOF'
id = "40"
name = "calc"
kind = "leaf"
accepts = ["course", "session", "topic", "note/class", "note/rewrite", "paper"]
EOF
{
  printf '[dodos.librarian]\nhome_nest = "40"\navailable_nests = ["40"]\n\n'
  printf '[hops.link]\nlink = true\n\n'
  for t in course session topic note/class note/rewrite paper; do
    printf '[[routes]]\ndata_type = "%s"\ndodo = "librarian"\nnest = "40"\nhops = ["link"]\n\n' "$t"
  done
} > "$V/routes.toml"

export MAILROOM_VAULT=$V MAILROOM_LISTEN=127.0.0.1:$PORT MAILROOM_CONFIG=/nonexistent RUST_LOG=mailroom=info
"$REPO/target/debug/mailroom" > "$WORK/server.log" 2>&1 &
PID=$!
for _ in $(seq 1 40); do curl -s "$URL/health" >/dev/null 2>&1 && break; sleep 0.25; done

# send <data_type> <key or -> <title> [<LABEL> <target>]...   prints the envelope id
send() {
  local dt=$1 key=$2 title=$3; shift 3
  local meta="\"title\":\"$title\""
  [ "$key" != - ] && meta="$meta,\"key\":\"$key\""
  local links="" sep=""
  while [ $# -ge 2 ]; do links="$links$sep{\"label\":\"$1\",\"to\":\"$2\"}"; sep=","; shift 2; done
  curl -s -X POST "$URL/dispatch" -H 'content-type: application/json' -d \
    "{\"source\":\"manual\",\"data_type\":\"$dt\",\"payload\":{\"kind\":\"text\",\"data\":\"$title\"},\"meta\":{$meta},\"links\":[$links]}" \
    | grep -o '"envelope_id":"[^"]*"' | head -1 | cut -d'"' -f4
}
get() { curl -s "$URL$1"; }
has() { echo "$1" | grep -q -- "$2"; }

echo "1. sending the Calc 3 scenario through /dispatch"
send course   course/calc-3              "Calc 3"                          >/dev/null
send session  session/calc-3/4           "Calc 3 class 4"   PART_OF course/calc-3 >/dev/null
send session  session/calc-3/6           "Calc 3 class 6"   "part of" course/calc-3 >/dev/null
send topic    topic/partial-derivatives  "Partial derivatives"             >/dev/null
N4=$(send note/class - "Class 4 notes" TAKEN_AT session/calc-3/4 ABOUT topic/partial-derivatives ABOUT topic/trigonometry)
RW=$(send note/rewrite - "Partial derivatives in my words" REVISES "$N4" ABOUT topic/partial-derivatives)
N6=$(send note/class - "Class 6 notes" TAKEN_AT session/calc-3/6 ABOUT topic/trigonometry)
PP=$(send paper - "A paper on gradients" ABOUT topic/partial-derivatives)
check "all eight envelopes accepted" '[ -n "$N4" ] && [ -n "$RW" ] && [ -n "$N6" ] && [ -n "$PP" ]'

echo "2. the Calc 3 query: class notes relevant to the paper"
R=$(get "/graph/related?from=env:$PP&via=ABOUT&label=note")
check "returns the class 4 note"            'has "$R" "env:$N4"'
check "returns the rewrite"                 'has "$R" "env:$RW"'
check "does not return the trig-only note"  '! has "$R" "env:$N6"'
check "does not return the paper itself"    '! has "$R" "\"id\":\"env:$PP\""'

echo "3. everything in Calc 3 (walk inward over meaning edges)"
W=$(get "/graph/walk?from=course/calc-3&depth=5&dir=in&labels=PART_OF,TAKEN_AT,REVISES,ABOUT")
check "both sessions at depth 1"            'has "$W" "\"depth\":1" && has "$W" "session/calc-3/4" && has "$W" "session/calc-3/6"'
check "the rewrite is reached (depth 3)"    'echo "$W" | grep -o "\"depth\":3[^}]*\"id\":\"env:$RW\"" >/dev/null'
check "the paper is not part of the course" '! has "$W" "env:$PP"'

echo "4. how the paper connects to the course"
P1=$(get "/graph/path?from=env:$PP&to=course/calc-3&labels=ABOUT,TAKEN_AT,PART_OF,REVISES")
P2=$(get "/graph/path?from=env:$PP&to=course/calc-3")
check "by meaning: 4 steps through the class 4 note" 'has "$P1" "\"length\":4" && has "$P1" "env:$N4"'
check "by any edge: 2 steps through the nest"        'has "$P2" "\"length\":2" && has "$P2" "nest:40"'

echo "5. nodes, keys and links"
C=$(get "/graph/node?id=course/calc-3")
T=$(get "/graph/node?id=topic/trigonometry")
check "the course node has its title"           'has "$C" "\"title\":\"Calc 3\""'
check "a linked-but-never-sent topic is a node" 'has "$T" "\"labels\":\[\]"'
check "a UUID link resolved to the note"        'get "/graph/node?id=env:$RW" | grep -q "\"label\":\"REVISES\",\"seq\":[0-9]*,\"to\":\"env:$N4\""'

echo "6. M1: degree table agrees with the edge list"
get /graph/edges.csv > "$WORK/edges.csv"
get /graph/degrees > "$WORK/degrees.json"
E=$(($(wc -l < "$WORK/edges.csv") - 1))
check "edges.csv has a row per edge ($E)"        'has "$(get /graph/stats)" "\"edges\":$E"'
if command -v python3 >/dev/null && python3 -c 'import networkx' 2>/dev/null; then
  OUT=$(python3 - "$WORK/edges.csv" "$WORK/degrees.json" <<'PY'
import csv, json, sys, networkx as nx
g = nx.DiGraph()
with open(sys.argv[1]) as f:
    for row in csv.DictReader(f):
        g.add_edge(row["source"], row["target"])
ours = {d["id"]: (d["in_degree"], d["out_degree"]) for d in json.load(open(sys.argv[2]))["degrees"]}
theirs = {n: (g.in_degree(n), g.out_degree(n)) for n in g.nodes}
diff = {n for n in set(ours) | set(theirs) if ours.get(n, (0, 0)) != theirs.get(n, (0, 0))}
print("agree" if not diff else f"differ on {sorted(diff)}")
PY
)
  check "NetworkX in/out degrees match ours ($OUT)" '[ "$OUT" = agree ]'
else
  echo "  skip  NetworkX not installed (nix-shell -p 'python3.withPackages (p: [p.networkx])')"
fi

echo
if [ "$FAILS" -eq 0 ]; then echo "all checks passed"; else echo "$FAILS check(s) failed (KEEP=1 bash graph-check.sh keeps the vault and log)"; fi
exit "$FAILS"
