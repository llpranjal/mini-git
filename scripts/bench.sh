#!/usr/bin/env bash
#
# The single command that produces every number in the README.
#
#     ./scripts/bench.sh
#
# It builds in release mode, runs the benchmark, and leaves the output in
# results/benchmark.txt. The benchmark itself runs `cargo test` at the end to
# report the test count, which is why this script must build first and then
# invoke the binary directly: running it through `cargo run` would hold the
# build lock and the nested `cargo test` would deadlock.

set -euo pipefail

cd "$(dirname "$0")/.."

echo "Building in release mode..."
cargo build --release --quiet

echo "Running the benchmark (this takes a minute)..."
echo
exec ./target/release/mgit-bench
