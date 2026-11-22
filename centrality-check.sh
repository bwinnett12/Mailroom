#!/usr/bin/env bash
# centrality-check.sh — M2 eigenvector centrality, end to end.
#
# Run from the Mailroom repo root, inside the dev shell:
#     bash centrality-check.sh
# For the NetworkX comparison (recommended):
#     nix-shell -p 'python3.withPackages (p: [p.networkx p.numpy p.scipy])' --run 'bash centrality-check.sh'
#
# Starts the server on a throwaway vault, sends a small graph with two
# separate pieces through /dispatch, and checks /graph/centrality against
# values worked out by hand and, if available, against NetworkX.
# Your real vault and the systemd service are never touched.

set -u
REPO=$(pwd); WORK=$(mktemp -d); V=$WORK/vault; PORT=3996; URL=http://127.0.0.1:$PORT
FAILS=0; PID=
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; FAILS=$((FAILS + 1)); }
check() { if eval "$2"; then pass "$1"; else fail "$1"; fi; }
cleanup() { [ -n "$PID" ] && kill "$PID" 2>/dev/null; [ "${KEEP:-0}" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"; }
trap cleanup EXIT

echo "building…"
cargo build -q 2>/dev/null || { echo "cargo build failed — run 'cargo build' to see why"; exit 1; }

mkdir -p "$V/40_learning/40-A"
export MAILROOM_DEVICE=check   # fixed device name, so the ledger folder is known
MAILROOM_VAULT=$V "$REPO/target/debug/mailroom" coffer init >/dev/null || { echo "coffer init failed"; exit 1; }
LEDGER=$V/85_coffer/85-C_ledger/check; CONTENTS=$V/85_coffer/85-B_contents
printf 'id = "40"\nname = "learning"\nkind = "leaf"\naccepts = ["thing"]\n' > "$V/40_learning/40-A/nest"
printf '[dodos.librarian]\nhome_nest = "40"\navailable_nests = ["40"]\n\n[hops.link]\nlink = true\n\n[[routes]]\ndata_type = "thing"\ndodo = "librarian"\nnest = "40"\nhops = ["link"]\n' > "$V/routes.toml"

export MAILROOM_VAULT=$V MAILROOM_LISTEN=127.0.0.1:$PORT MAILROOM_CONFIG=/nonexistent RUST_LOG=mailroom=warn
"$REPO/target/debug/mailroom" > "$WORK/server.log" 2>&1 & PID=$!
for _ in $(seq 1 40); do curl -s "$URL/health" >/dev/null 2>&1 && break; sleep 0.25; done

# send <key> [<LABEL> <target>]...
send() {
  local key=$1; shift; local links="" sep=""
  while [ $# -ge 2 ]; do links="$links$sep{\"label\":\"$1\",\"to\":\"$2\"}"; sep=","; shift 2; done
  curl -s -o /dev/null -X POST "$URL/dispatch" -H 'content-type: application/json' -d \
    "{\"source\":\"manual\",\"data_type\":\"thing\",\"payload\":{\"kind\":\"text\",\"data\":\"$key\"},\"meta\":{\"key\":\"$key\"},\"links\":[$links]}"
}

# Piece 1, a star: hub with three spokes (one pointing in, two out).
#   by hand: λ = √3, hub = 1/√2 ≈ 0.7071, each spoke = 1/√6 ≈ 0.4082
# Piece 2, a path p1 – p2 – p3.
#   by hand: λ = √2, p2 = √2/2 ≈ 0.7071, ends = 1/2
# Every envelope also VISITED nest:40; centrality must ignore that by default,
# or nest:40 would join both pieces into one and top the list.
send hub ABOUT s1 ABOUT s2
send s3 ABOUT hub
send p1 NEXT p2
send p2 NEXT p3

C=$(curl -s "$URL/graph/centrality?top=10")
J() { python3 -c "import json,sys; d=json.loads(sys.argv[1]); print($1)" "$C"; }

echo "1. hand-calculated values"
check "two components (the nest is not a bridge)" '[ "$(J "d[\"components_total\"]")" = 2 ]'
check "star: λ = √3"                               '[ "$(J "round(d[\"components\"][0][\"lambda\"],6)")" = 1.732051 ]'
check "star: hub first, 1/√2"                      '[ "$(J "(lambda t: t[\"id\"]+\" \"+str(round(t[\"score\"],6)))(d[\"components\"][0][\"top\"][0])")" = "hub 0.707107" ]'
check "star: spokes 1/√6"                          '[ "$(J "sorted({round(t[\"score\"],6) for t in d[\"components\"][0][\"top\"][1:]})")" = "[0.408248]" ]'
check "path: λ = √2"                               '[ "$(J "round(d[\"components\"][1][\"lambda\"],6)")" = 1.414214 ]'
check "path: middle first, √2/2"                   '[ "$(J "(lambda t: t[\"id\"]+\" \"+str(round(t[\"score\"],6)))(d[\"components\"][1][\"top\"][0])")" = "p2 0.707107" ]'
check "nest:40 not scored by default"              '! echo "$C" | grep -q "nest:40"'
V2=$(curl -s "$URL/graph/centrality?labels=VISITED")
check "with labels=VISITED, nest:40 is the hub"    'echo "$V2" | python3 -c "import json,sys; print(json.load(sys.stdin)[\"components\"][0][\"top\"][0][\"id\"])" | grep -qx nest:40'

echo "2. NetworkX"
if python3 -c 'import networkx' 2>/dev/null; then
  curl -s "$URL/graph/edges.csv" > "$WORK/edges.csv"
  curl -s "$URL/graph/centrality?top=100000&components=100000" > "$WORK/ours.json"
  OUT=$(python3 - "$WORK/edges.csv" "$WORK/ours.json" <<'PY'
import csv, json, sys, networkx as nx
SYSTEM = {"VISITED", "HAS_CONTENT", "EMITTED_FROM"}
g = nx.Graph()
for r in csv.DictReader(open(sys.argv[1])):
    if r["label"] in SYSTEM: continue
    a, b = r["source"], r["target"]
    w = g[a][b]["weight"] + 1 if g.has_edge(a, b) else 1
    g.add_edge(a, b, weight=w)
ours = {n["id"]: n["score"] for c in json.load(open(sys.argv[2]))["components"] for n in c["top"]}
worst = 0.0
for comp in nx.connected_components(g):
    sub = g.subgraph(comp)
    theirs = nx.eigenvector_centrality(sub, weight="weight", tol=1e-12, max_iter=10000) if len(sub) > 1 else {n: 1.0 for n in sub}
    for n, v in theirs.items():
        worst = max(worst, abs(ours.get(n, float("nan")) - v))
print(f"max difference {worst:.2e}" if worst < 1e-6 else f"DIFFER max {worst:.2e}")
PY
)
  check "per-component scores match NetworkX ($OUT)" 'case "$OUT" in max*) true;; *) false;; esac'
else
  echo "  skip  NetworkX not installed (see the header for the nix-shell line)"
fi

echo
if [ "$FAILS" -eq 0 ]; then echo "all checks passed"; else echo "$FAILS check(s) failed (KEEP=1 keeps the vault and log)"; fi
exit "$FAILS"
