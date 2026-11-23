# Semgrep dependency policy

The scanner is a CI tool, not a runtime component of Notes. Its wheel and every
transitive dependency are SHA-256 pinned in `semgrep-requirements.txt`. CI installs
only wheels with `--require-hashes --no-deps`; it does not resolve versions at run
time. The rules remain pinned separately in `rules.lock.json`.

## Security updates and upstream version bounds

Semgrep 1.182.1 declares `wcmatch~=8.3` and OpenTelemetry 1.37.x upper bounds.
Those bounds exclude the fixes for GHSA-53pm-wgxf-c6qf (wcmatch 11.2) and
GHSA-mf5v-w3w3-f6cg (OpenTelemetry API 1.45.2). We intentionally override these
bounds for this repository's scanner environment. This is a locally tested
combination, not an upstream-supported dependency set.

The OpenTelemetry SDK, exporters, protocol, instrumentation and semantic
conventions move together to 1.45.2 / 0.66b2, including the new HTTP transport
and OTLP common packages. Updating only the API leaves an inconsistent SDK.
The existing mcp 1.30.0 pin also overrides Semgrep's exact 1.29.0 requirement;
that pre-existing exception is now explicitly validated.

`semgrep-compatibility.json` records the exact upstream constraints permitted to
be overridden and the minimum patched versions. `validate_semgrep.py` checks:

- Installed locked versions and dependency requirements, including activated extras.
- Only the named Semgrep constraints may disagree with installed versions. All
  other missing/incompatible dependencies fail, as do obsolete exceptions.
- Minimum security versions, Python telemetry imports and trace propagation,
  Semgrep's Python glob matching, and actual clean/finding scans with path filters.

Run after the hash-pinned installation, using the same virtual environment:

```sh
python security/validate_semgrep.py
tools/semgrep.sh gate
```

Metrics and version checks are disabled. Local validation additionally runs
without network, with a read-only checkout and an isolated writable scratch
space. The ordinary upstream `pip check` reports the documented metadata
conflicts; we do not edit installed wheel metadata to disguise them. The stricter
repository-specific validator replaces that check for this locked environment.

For future updates, review the whole dependency closure and upstream metadata,
regenerate hashes from the published files, run the validator and the real gate,
and remove overrides when upstream permits the patched versions. Do not raise a
single OpenTelemetry package independently or suppress the security alerts.
