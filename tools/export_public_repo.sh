#!/usr/bin/env bash
# Dry-run export of the public repository snapshot (local only; never pushes).
#
#   tools/export_public_repo.sh OUT_DIR [--forbid-email ADDR]... [--forbid-term WORD]... [--strict]
#
# Private values to check for (account e-mail, real note titles) are passed on
# the command line or via NOTES_EXPORT_FORBID / NOTES_EXPORT_FORBID_TERMS so they
# never live in this file. See tools/public_export.py for the allowlist.
set -euo pipefail
if [ "$#" -lt 1 ]; then
  sed -n '2,8p' "$0" >&2
  exit 2
fi
out=$1; shift
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
python3 "$root/tools/branding.py" sync --check
exec python3 "$root/tools/public_export.py" --out "$out" "$@"
