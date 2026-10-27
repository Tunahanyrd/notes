#!/usr/bin/env bash
# Semgrep with pinned community rules (security/rules.lock.json).
#
#   tools/semgrep.sh [gate]          CI gate: ERROR-severity findings in
#                                    product code; exit 1 when any exist
#   tools/semgrep.sh report [OUT]    full report (all severities, whole tree,
#                                    never fails on findings); JSON to OUT
#
# Environment:
#   SEMGREP           semgrep executable (default: `semgrep` on PATH); its
#                     version must equal the one in security/rules.lock.json
#   SEMGREP_CACHE     archive/rule cache (default: $XDG_CACHE_HOME/notes-semgrep)
#   SEMGREP_ARCHIVES  directory that already holds <name>.tar.gz archives
#                     (checked against the lock like a download)
#   SEMGREP_JSON      gate mode: also write JSON results to this file
#   SEMGREP_JOBS      parallel jobs (default 4)
#
# Archives are downloaded only when missing from both directories, and every
# archive is verified by SHA-256 before it is extracted. Semgrep runs with a
# throwaway HOME, metrics and version checks off: it sends nothing anywhere.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
lock="$root/security/rules.lock.json"
rule_list="$root/security/semgrep-rules.txt"
mode=${1:-gate}
case "$mode" in
    gate | report) ;;
    -h | --help)
        sed -n '2,20p' "${BASH_SOURCE[0]}"
        exit 0
        ;;
    *)
        echo "usage: tools/semgrep.sh [gate | report [OUT.json]]" >&2
        exit 2
        ;;
esac

die() {
    echo "semgrep.sh: $*" >&2
    exit 2
}

command -v python3 >/dev/null || die "python3 is required to read the lock file"
semgrep=${SEMGREP:-semgrep}
command -v "$semgrep" >/dev/null || die "semgrep not found; set SEMGREP or install it from security/semgrep-requirements.txt"

want_version=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["semgrep"]["version"])' "$lock")
have_version=$("$semgrep" --version 2>/dev/null | tail -n1 | tr -d '[:space:]') || true
[ "$have_version" = "$want_version" ] || die "semgrep $want_version required, found '${have_version:-unknown}'"

cache=${SEMGREP_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/notes-semgrep}
mkdir -p "$cache/archives" "$cache/rules"

sha256_of() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# name, url, sha256 per line.
entries=$(python3 -c '
import json, re, sys
for r in json.load(open(sys.argv[1]))["repositories"]:
    assert re.fullmatch(r"[a-z0-9_-]+", r["name"]) and re.fullmatch(r"[0-9a-f]{64}", r["sha256"])
    assert r["url"].startswith("https://codeload.github.com/") and r["commit"] in r["url"]
    print(r["name"], r["url"], r["sha256"])
' "$lock")

while read -r name url sum || [ -n "${name:-}" ]; do
    [ -n "$name" ] || continue
    dest="$cache/rules/$name"
    if [ -f "$dest/.sha256" ] && [ "$(cat "$dest/.sha256")" = "$sum" ]; then
        continue
    fi
    archive="$cache/archives/$name.tar.gz"
    if [ -n "${SEMGREP_ARCHIVES:-}" ] && [ -f "$SEMGREP_ARCHIVES/$name.tar.gz" ]; then
        cp "$SEMGREP_ARCHIVES/$name.tar.gz" "$archive.part"
        mv "$archive.part" "$archive"
    fi
    if [ ! -f "$archive" ] || [ "$(sha256_of "$archive")" != "$sum" ]; then
        echo "semgrep.sh: downloading $name rules" >&2
        curl -fsSL --proto '=https' --tlsv1.2 --retry 3 -o "$archive.part" "$url"
        mv "$archive.part" "$archive"
    fi
    got=$(sha256_of "$archive")
    if [ "$got" != "$sum" ]; then
        rm -f "$archive"
        die "$name archive checksum mismatch (expected $sum, got $got)"
    fi
    rm -rf "$dest.tmp" "$dest"
    mkdir -p "$dest.tmp"
    tar -xzf "$archive" -C "$dest.tmp" --strip-components=1 --no-same-owner --no-same-permissions
    echo "$sum" >"$dest.tmp/.sha256"
    mv "$dest.tmp" "$dest"
done <<<"$entries"

configs=()
while read -r kind path || [ -n "${kind:-}" ]; do
    case "${kind:-}" in
        '' | '#'*) continue ;;
        both) ;;
        gate | report) [ "$kind" = "$mode" ] || continue ;;
        *) die "bad line in security/semgrep-rules.txt: $kind $path" ;;
    esac
    [ -d "$cache/rules/$path" ] || die "rule directory $path is missing from the pinned archives"
    configs+=(--config "$cache/rules/$path")
done <"$rule_list"

scratch_home=$(mktemp -d "${TMPDIR:-/tmp}/notes-semgrep-home.XXXXXX")
trap 'rm -rf "$scratch_home"' EXIT

common=(
    scan "${configs[@]}"
    --metrics=off --disable-version-check
    --max-target-bytes 2000000
    -j "${SEMGREP_JOBS:-4}"
)

cd "$root"
run() {
    HOME="$scratch_home" XDG_CONFIG_HOME="$scratch_home/.config" \
        SEMGREP_SEND_METRICS=off SEMGREP_ENABLE_VERSION_CHECK=0 \
        "$semgrep" "$@"
}

if [ "$mode" = report ]; then
    out=${2:-$root/target/semgrep/report.json}
    mkdir -p "$(dirname "$out")"
    # Whole tree minus .semgrepignore; never fails on findings.
    status=0
    run "${common[@]}" --json -o "$out" . || status=$?
    # 0 = clean, 1 = findings; anything else is an operational error.
    [ "$status" -le 1 ] || die "semgrep failed (exit $status)"
    python3 -c '
import collections, json, sys
d = json.load(open(sys.argv[1]))
c = collections.Counter(r["extra"]["severity"] for r in d["results"])
print("findings:", len(d["results"]), dict(sorted(c.items())), "errors:", len(d["errors"]))
' "$out"
    exit 0
fi

# Gate: shipped product code only. Tests, examples, the conformance harness
# and Python helper scripts are excluded on purpose: they are never shipped,
# and test code deliberately spawns processes, writes temp files and embeds
# sample tokens. The full report still covers them.
targets=(crates desktop/src desktop/src-tauri desktop/index.html Dockerfile .github/workflows)
exists=()
for t in "${targets[@]}"; do
    [ -e "$t" ] && exists+=("$t")
done
json_args=()
[ -n "${SEMGREP_JSON:-}" ] && json_args=(--json-output "$SEMGREP_JSON")
run "${common[@]}" \
    --severity ERROR --error \
    --exclude 'tests.rs' --exclude 'test_*.rs' --exclude '*_tests.rs' \
    --exclude 'examples' --exclude 'benches' --exclude 'conformance' \
    --exclude '*.py' \
    "${json_args[@]}" \
    "${exists[@]}"
