## What and why

<!-- Short description; link issues. -->

## Evidence level

<!-- Keep these separate; one does not imply another. -->
- [ ] Implemented in source
- [ ] Synthetic / offline tests pass
- [ ] Cloud readback verified (controlled test note only; say which step)
- [ ] Seen on a Samsung device / official app

## Checklist

- [ ] `cargo test --workspace` passes (no PyO3 in the default build)
- [ ] Intended response/state changes re-blessed in `crates/notes-engine/conformance/golden` and reviewed
- [ ] notes-engine HTTP conformance replay is 100% (`run.py --replay`)
- [ ] `npx tsc --noEmit` and Playwright pass (UI changes)
- [ ] `python3 tools/branding.py sync --check` (no hardcoded product name / app id)
- [ ] Cloud writes keep the safety rules: immutable operation IDs, compare guard,
      a single POST, full readback, no automatic retry of an unknown outcome
- [ ] No private data: tokens, signed URLs, e-mails, note titles/content, SQLite files,
      personal screenshots, absolute home paths
- [ ] No Samsung material: binaries, APKs, icons, fonts, decompiled code or copied text
