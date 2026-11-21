#!/usr/bin/env bash
# Regenerate the instruction test vectors (see tools/gen_vectors.py and ADR 0008).
# Usage: tools/vectors.sh [output]   (default: crates/ppc-lift/tests/vectors.bin)
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/venv.sh"
exec "$TOOLS_PY" "$REPO_ROOT/tools/gen_vectors.py" "${1:-$REPO_ROOT/crates/ppc-lift/tests/vectors.bin}"
