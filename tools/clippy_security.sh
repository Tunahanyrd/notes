#!/usr/bin/env bash
# Security-focused clippy pass over the engine workspace (product code only:
# --lib --bins, so tests, benches and examples are not linted).
#
# Gate (fails the run):
#   * every crate: `unsafe` blocks need a `// SAFETY:` comment, no
#     mem::forget, no todo!/unimplemented!;
#   * HARDENED crates (hostile input from Samsung Cloud, downloaded files,
#     the sign-in callback or the local HTTP API reaches them): no
#     unwrap/expect/panic! in non-test code. A provably safe site uses a
#     local `#[allow(clippy::unwrap_used)] // reason` instead.
# Report only (printed, never fails): indexing/slicing, lossy casts and
# unwrap/expect/panic in the remaining crates.
#
#   tools/clippy_security.sh            # gate + report
#   tools/clippy_security.sh --gate     # gate only (CI)
set -uo pipefail
cd "$(dirname "$0")/.."

# Crates whose non-test code is clean of unwrap/expect/panic. Add a crate
# here once `cargo clippy -p <crate> --lib --bins -- -W clippy::unwrap_used
# -W clippy::expect_used -W clippy::panic` is clean.
HARDENED=(notes-platform notes-net notes-protocol ${NOTES_CLIPPY_HARDENED:-})

status=0
echo "== gate: undocumented unsafe, mem::forget, todo!/unimplemented! (workspace)"
cargo clippy --locked --workspace --lib --bins -- -A clippy::all \
  -D clippy::undocumented_unsafe_blocks -D clippy::mem_forget \
  -D clippy::todo -D clippy::unimplemented || status=1

for crate in "${HARDENED[@]}"; do
  echo "== gate: unwrap/expect/panic ($crate)"
  cargo clippy --locked -p "$crate" --lib --bins -- -A clippy::all \
    -D clippy::unwrap_used -D clippy::expect_used -D clippy::panic || status=1
done

if [ "${1:-}" != "--gate" ]; then
  echo "== report: panics, indexing and casts (workspace, warnings only)"
  cargo clippy --locked --workspace --lib --bins --message-format=short -- -A clippy::all \
    -W clippy::unwrap_used -W clippy::expect_used -W clippy::panic -W clippy::unreachable \
    -W clippy::indexing_slicing -W clippy::cast_possible_truncation -W clippy::cast_sign_loss 2>&1 \
    | grep -E '^crates/.*: warning:' | sed -E 's#^(crates/[^/]+)/.*warning: (.*)$#\1: \2#' \
    | sed -E 's/`[^`]*`/_/g' | sort | uniq -c | sort -rn || true
fi
exit "$status"
