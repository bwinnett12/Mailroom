#!/usr/bin/env bash
# recipe-vault.sh <dir> — make a throwaway vault that accepts recipes and ingredients.
# Run from the Mailroom repo. Then:  MAILROOM_VAULT=<dir> MAILROOM_LISTEN=127.0.0.1:3000 cargo run
# Links are "replace": re-importing a fixed recipe file drops the links it lost.
set -eu
V=${1:?usage: recipe-vault.sh <dir>}
mkdir -p "$V/60_kitchen/60-A_recipes"
cat > "$V/60_kitchen/60-A_recipes/nest" <<'N'
id = "60-A"
name = "recipes"
kind = "leaf"
accepts = ["recipe", "ingredient"]
N
cat > "$V/routes.toml" <<'R'
[dodos.cook]
home_nest       = "60-A"
available_nests = ["60-A"]

[hops.link]
link = "replace"

[[routes]]
data_type = "recipe"
dodo      = "cook"
nest      = "60-A"
hops      = ["link"]

[[routes]]
data_type = "ingredient"
dodo      = "cook"
nest      = "60-A"
hops      = ["link"]
R

# The coffer: where Mailroom keeps file bytes and this machine's ledger.
MAILROOM_VAULT="$V" cargo run -q -- coffer init
echo "vault ready: $V"
