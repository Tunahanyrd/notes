#!/usr/bin/env python3
"""Validate the exact locked environment and narrowly scoped upstream overrides."""
import importlib.metadata as metadata
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile

from packaging.requirements import Requirement
from packaging.utils import canonicalize_name
from packaging.version import Version

ROOT = Path(__file__).resolve().parent


def validate_dependencies():
    pins = dict(re.findall(r"^([a-zA-Z0-9_-]+)==([^ ;\\\n]+)", (ROOT / "semgrep-requirements.txt").read_text(), re.M))
    expected = json.loads((ROOT / "rules.lock.json").read_text())["semgrep"]["version"]
    if pins.get("semgrep") != expected:
        raise RuntimeError("Semgrep version differs between manifests")
    compatibility = json.loads((ROOT / "semgrep-compatibility.json").read_text())
    overrides = compatibility["semgrepRequirementOverrides"]
    installed = {canonicalize_name(d.metadata["Name"]): d for d in metadata.distributions()}
    errors = []
    for name, minimum in compatibility["minimumSecurityVersions"].items():
        if name not in installed or Version(installed[name].version) < Version(minimum["version"]):
            errors.append(f"{name}: vulnerable version ({minimum['advisory']})")
    used = set()
    # Follow activated extras as well as unconditional requirements.
    active = {n: {""} for n in pins if n in installed}
    changed = True
    while changed:
        changed = False
        for name, extras in list(active.items()):
            for raw in installed[name].requires or []:
                req = Requirement(raw)
                if req.marker and not any(req.marker.evaluate({"extra": e}) for e in extras):
                    continue
                target = canonicalize_name(req.name)
                if target in installed:
                    previous = active.setdefault(target, {""})
                    extra = req.extras - previous
                    if extra:
                        previous.update(extra)
                        changed = True
    for name, extras in active.items():
        dist = installed[name]
        if name in pins and dist.version != pins[name]:
            errors.append(f"{name}: installed version differs from hash-pinned lock")
        for raw in dist.requires or []:
            req = Requirement(raw)
            if req.marker and not any(req.marker.evaluate({"extra": e}) for e in extras):
                continue
            target = canonicalize_name(req.name)
            dep = installed.get(target)
            if dep is None:
                errors.append(f"{name}: missing {target}")
            elif dep.version not in req.specifier:
                if (name == "semgrep" and dist.version == expected
                        and overrides.get(target) == str(req.specifier)
                        and dep.version == pins.get(target)):
                    used.add(target)
                else:
                    errors.append(f"{name}: incompatible {target} {dep.version} ({req.specifier})")
    if used != set(overrides):
        errors.append("Override list changed or contains unused entries; review it")
    if errors:
        raise RuntimeError("\n".join(errors))
    print(f"Dependency closure verified; {len(used)} explicit Semgrep metadata overrides")


def smoke_scan():
    # Exercise Python imports too: native CLI startup alone does not cover these.
    from semgrep.target_manager import TargetManager
    for path, pattern, expected in [
        ("src/nested/a.py", "**/*.py", True),
        ("src/.hidden.py", "**/*.py", True),
        ("src/a.rs", "**/*.py", False),
    ]:
        if TargetManager._globmatch(path, pattern) != expected:
            raise RuntimeError("Semgrep wcmatch compatibility failed")
    import semgrep.telemetry  # noqa: F401
    from opentelemetry.trace.propagation.tracecontext import TraceContextTextMapPropagator
    carrier = {"traceparent": "00-" + "1" * 32 + "-" + "2" * 16 + "-01"}
    from opentelemetry import trace
    ctx = TraceContextTextMapPropagator().extract(carrier)
    if not trace.get_current_span(ctx).get_span_context().is_valid:
        raise RuntimeError("Trace context propagation failed")
    with tempfile.TemporaryDirectory(prefix="notes-semgrep-smoke-") as tmp:
        root = Path(tmp)
        (root / "rule.yaml").write_text("rules:\n- id: smoke-eval\n  languages: [python]\n  severity: ERROR\n  message: synthetic fixture\n  pattern: eval($X)\n")
        (root / "src").mkdir()
        (root / "ignored").mkdir()
        (root / "src/bad.py").write_text("eval(user_input)\n")
        (root / "src/clean.py").write_text("print('ok')\n")
        (root / "ignored/bad.py").write_text("eval(user_input)\n")
        env = dict(os.environ, HOME=tmp, SEMGREP_SEND_METRICS="off", SEMGREP_ENABLE_VERSION_CHECK="0")
        command = [str(Path(sys.executable).parent / "semgrep"), "scan", "--config", "rule.yaml", "--metrics=off", "--disable-version-check", "--no-git-ignore", "--include", "**/*.py", "--exclude", "ignored", "--json", "--error", "-j", "1"]
        for target, expected_count in [(".", 1), ("src/clean.py", 0)]:
            run = subprocess.run(command + [target], cwd=tmp, env=env, capture_output=True, text=True, timeout=90)
            result = json.loads(run.stdout)
            if run.returncode != (1 if expected_count else 0) or result.get("errors"):
                raise RuntimeError(f"Scanner failed: {run.returncode}; {run.stderr}")
            paths = [r["path"] for r in result["results"]]
            if paths != (["src/bad.py"] if expected_count else []):
                raise RuntimeError(f"Unexpected scan results: {paths}")
    print("Telemetry imports/propagation, include/exclude, finding and clean scans passed")


if __name__ == "__main__":
    validate_dependencies()
    smoke_scan()
