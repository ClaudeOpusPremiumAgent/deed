#!/bin/sh
# Runs a command in the isolated build image. Only this directory is mounted.
# Usage: ./dev.sh build | ./dev.sh test | ./dev.sh sh -c '...'
set -e
cd "$(dirname "$0")"
mkdir -p target/home
# Runs as the host user; caches live under target/, which git ignores.
RUN="docker run --rm --user $(id -u):$(id -g) -v $PWD:/app -e HOME=/app/target/home -e CARGO_HOME=/app/target/cargo-home -e CARGO_TARGET_DIR=/app/target deed-dev"
build() {
  # The vault has no near-sdk for cargo-near to read a protocol version from, and needs nothing from it.
  $RUN cargo build -q -p deed-vault --target wasm32-unknown-unknown --release
  mkdir -p target/near/deed_vault
  cp target/wasm32-unknown-unknown/release/deed_vault.wasm target/near/deed_vault/deed_vault.wasm
  for c in slow deed; do
    $RUN cargo near build non-reproducible-wasm --manifest-path contracts/$c/Cargo.toml --no-abi >target/build-$c.log 2>&1 \
      || { sed 's/\x1b\[[0-9;]*m//g' target/build-$c.log | tail -25; exit 1; }
  done
  wc -c target/near/deed_vault/deed_vault.wasm target/near/deed_slow/deed_slow.wasm target/near/deed_contract/deed_contract.wasm
}
case "$1" in
  build) build ;;
  test) build; shift; $RUN cargo test -p deed-tests "$@" -- --nocapture --test-threads=1 ;;
  *) $RUN "$@" ;;
esac
