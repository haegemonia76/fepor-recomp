#!/usr/bin/env bash
# Development loop: regenerate and build the game from a disc, link it with the Rust runtime, run it.
# Usage: tools/dev-run.sh <disc> [data-dir] [-- extra fepor-game args]
# Environment: FEPOR_TRACE=hw,calls (runtime tracing), TRACE_CALLS=1 (generate call hooks, default on),
#              BUILD_TYPE=Release|Debug|None (default None = -O0 for fast iteration),
#              SKIP_PREPARE=1 reuses the generated build in <data-dir> (runtime-only changes).
#              RUN_TIMEOUT=<seconds> kills a run that hangs (default 30; 0 = no limit).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
disc="${1:?usage: tools/dev-run.sh <disc> [data-dir] [-- args]}"
data="${2:-$ROOT/out/dev}"
shift 2 2>/dev/null || shift $#
[ "${1:-}" = "--" ] && shift

cargo build -q --release -p fepor-recomp --manifest-path "$ROOT/Cargo.toml"
flags=()
[ "${TRACE_CALLS:-1}" = "1" ] && flags+=(--trace-calls)
[ "${SKIP_PREPARE:-0}" = 1 ] || "$ROOT/target/release/fepor-recomp" prepare --game "$disc" --data "$data" --build-type "${BUILD_TYPE:-None}" "${flags[@]}" > /dev/null
FEPOR_GENERATED_DIR="$data/build" cargo build -q --release -p fepor-game --manifest-path "$ROOT/Cargo.toml"
t="${RUN_TIMEOUT:-30}"
if [ "$t" != 0 ]; then exec timeout "$t" "$ROOT/target/release/fepor-game" --game "$disc" --data "$data" "$@"; fi
exec "$ROOT/target/release/fepor-game" --game "$disc" --data "$data" "$@"
