#!/bin/bash
# run-mailroom-on-mac.sh

export MAILROOM_VAULT="$HOME/Orchard"
export MAILROOM_LIBRARY_ROOT="$HOME/Orchard"
export MAILROOM_LISTEN="0.0.0.0:3000"
export RUST_LOG="mailroom=info,tower_http=info"

cd ~/Projects/Mailrooms/rust/Mailroom || exit 1
OPENSSL_DIR=$(brew --prefix openssl) cargo run