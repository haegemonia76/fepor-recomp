#!/usr/bin/env bash
# Validate a RecompLauncher profile against the launcher's published JSON schema.
# Usage: tools/validate-profile.sh [profile.json]   (default: packaging/recomplauncher.json)
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/venv.sh"
profile="${1:-$REPO_ROOT/packaging/recomplauncher.json}"
schema="$REPO_ROOT/.cache/recomplauncher.schema.json"
if [ ! -f "$schema" ]; then
    mkdir -p "$(dirname "$schema")"
    curl -sSfL https://raw.githubusercontent.com/elripalda/RecompLauncher-SDK/main/schema/recomplauncher.json -o "$schema"
fi
"$TOOLS_PY" - "$profile" "$schema" <<'PY'
import json, sys
import jsonschema
profile, schema = (json.load(open(p)) for p in sys.argv[1:3])
errors = sorted(jsonschema.Draft7Validator(schema).iter_errors(profile), key=lambda e: list(e.path))
for e in errors:
    print(f"{'/'.join(map(str, e.path)) or '<root>'}: {e.message}")
sys.exit(1 if errors else 0)
PY
echo "ok: $profile"
