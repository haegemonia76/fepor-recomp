#!/usr/bin/env bash
# Source this file: it creates .venv-tools on first use and exports TOOLS_PY and REPO_ROOT.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENV="$REPO_ROOT/.venv-tools"
[ -x "$VENV/bin/python" ] || python3 -m venv "$VENV"
if ! "$VENV/bin/python" -c 'import unicorn, jsonschema' 2>/dev/null; then
    "$VENV/bin/pip" install -q -r "$REPO_ROOT/tools/requirements.txt"
fi
TOOLS_PY="$VENV/bin/python"
export REPO_ROOT TOOLS_PY
