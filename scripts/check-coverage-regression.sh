#!/usr/bin/env bash
# Soft coverage regression check for touched crates.
#
# Compares per-crate line coverage from an llvm-cov JSON summary (or LCOV)
# against coverage/baseline.json. Regressions emit ::warning:: annotations and
# (on pull_request) a sticky PR comment. Exit code is always 0 — this is not a
# hard gate yet (see CONTRIBUTING.md).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export ROOT
BASELINE="${ROOT}/coverage/baseline.json"
cd "$ROOT"

if [[ ! -f "$BASELINE" ]]; then
  echo "::error::Missing ${BASELINE}"
  exit 1
fi

python3 - <<'PY'
import json
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

root = Path(os.environ["ROOT"])
baseline_path = root / "coverage" / "baseline.json"
summary_json = root / "coverage-summary.json"
lcov_path = root / "lcov.info"
report_md = root / "coverage-report.md"

baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
crate_baselines = baseline.get("crates", {})
workspace_baseline = float(baseline.get("workspace_lines_pct", 0.0))

# Map package name -> directory prefix under the repo root.
PACKAGE_DIRS = {
    "invofi-common": "common",
    "invofi-registry": "registry",
    "invofi-financing": "financing",
    "invofi-repayment": "repayment",
    "invofi-insurance": "insurance",
    "invofi-reputation": "reputation",
    "invofi-integration": "integration",
}
DIR_TO_PACKAGE = {v: k for k, v in PACKAGE_DIRS.items()}


def coverage_from_lcov(path: Path):
    """Return ({package: line_pct}, workspace_pct) aggregated from LCOV."""
    covered: dict[str, int] = defaultdict(int)
    total: dict[str, int] = defaultdict(int)
    current_pkg = None
    if not path.is_file():
        return {}, None
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith("SF:"):
            sf = line[3:].replace("\\", "/")
            current_pkg = None
            for pkg, directory in PACKAGE_DIRS.items():
                # Match "/registry/src/..." or "registry/src/..."
                if re.search(rf"(^|/)({re.escape(directory)})/", sf):
                    current_pkg = pkg
                    break
        elif line.startswith("LF:") and current_pkg:
            total[current_pkg] += int(line[3:])
        elif line.startswith("LH:") and current_pkg:
            covered[current_pkg] += int(line[3:])
        elif line.strip() == "end_of_record":
            current_pkg = None
    out = {}
    for pkg, lf in total.items():
        if lf == 0:
            continue
        out[pkg] = round(100.0 * covered[pkg] / lf, 2)
    ws_lf = sum(total.values())
    ws_lh = sum(covered.values())
    workspace = round(100.0 * ws_lh / ws_lf, 2) if ws_lf else None
    return out, workspace


def coverage_from_json(path: Path) -> tuple[dict[str, float], float | None]:
    """Best-effort parse of cargo-llvm-cov --json-summary-path output."""
    if not path.is_file():
        return {}, None
    data = json.loads(path.read_text(encoding="utf-8"))
    per_crate: dict[str, float] = {}
    workspace = None

    # Format A: {"data":[{"totals":{"lines":{"percent":..}},"files":[...]}]}
    if isinstance(data, dict) and "data" in data:
        for entry in data["data"]:
            totals = entry.get("totals") or {}
            lines = totals.get("lines") or {}
            if "percent" in lines and workspace is None:
                workspace = float(lines["percent"])
            for f in entry.get("files") or []:
                filename = (f.get("filename") or "").replace("\\", "/")
                pkg = None
                for name, directory in PACKAGE_DIRS.items():
                    if re.search(rf"(^|/)({re.escape(directory)})/", filename):
                        pkg = name
                        break
                if not pkg:
                    continue
                flines = (f.get("summary") or f.get("totals") or {}).get("lines") or {}
                if "percent" in flines:
                    # Average later via weighted? Use running sum of covered/count if present.
                    pass
                count = int(flines.get("count") or 0)
                covered = int(flines.get("covered") or 0)
                if count:
                    # accumulate in side maps
                    per_crate.setdefault(pkg, [0, 0])
                    if isinstance(per_crate[pkg], list):
                        per_crate[pkg][0] += covered
                        per_crate[pkg][1] += count
        resolved = {}
        for pkg, val in per_crate.items():
            if isinstance(val, list) and val[1]:
                resolved[pkg] = round(100.0 * val[0] / val[1], 2)
        return resolved, workspace

    # Format B: flat {"invofi-registry": {"lines": 90.1}, "total": {...}}
    if isinstance(data, dict):
        for key, val in data.items():
            if key in PACKAGE_DIRS and isinstance(val, dict):
                pct = val.get("lines") or val.get("line_pct") or val.get("percent")
                if pct is not None:
                    per_crate[key] = round(float(pct), 2)
            if key in ("total", "totals", "workspace") and isinstance(val, dict):
                pct = val.get("lines") or val.get("line_pct") or val.get("percent")
                if pct is not None:
                    workspace = float(pct)
        if per_crate:
            return per_crate, workspace

    return {}, workspace


per_crate, workspace_pct = coverage_from_json(summary_json)
if not per_crate:
    per_crate, workspace_pct = coverage_from_lcov(lcov_path)
if workspace_pct is None and per_crate:
    # Unweighted mean as a last-resort display figure.
    workspace_pct = round(sum(per_crate.values()) / len(per_crate), 2)

# Determine touched packages from the PR/push diff.
event_name = os.environ.get("GITHUB_EVENT_NAME", "")
base_ref = os.environ.get("GITHUB_BASE_REF") or "master"
changed_files: list[str] = []
try:
    if event_name == "pull_request":
        merge_base = subprocess.check_output(
            ["git", "merge-base", f"origin/{base_ref}", "HEAD"],
            text=True,
        ).strip()
        changed_files = subprocess.check_output(
            ["git", "diff", "--name-only", merge_base, "HEAD"],
            text=True,
        ).splitlines()
    else:
        # Push to master: compare against previous commit when available.
        try:
            changed_files = subprocess.check_output(
                ["git", "diff", "--name-only", "HEAD~1", "HEAD"],
                text=True,
            ).splitlines()
        except subprocess.CalledProcessError:
            changed_files = []
except subprocess.CalledProcessError:
    changed_files = []

touched_pkgs: set[str] = set()
for path in changed_files:
    top = path.replace("\\", "/").split("/", 1)[0]
    if top in DIR_TO_PACKAGE:
        touched_pkgs.add(DIR_TO_PACKAGE[top])

lines = []
lines.append("## Coverage report")
lines.append("")
lines.append(f"Workspace line coverage: **{workspace_pct if workspace_pct is not None else 'n/a'}%** "
             f"(baseline {workspace_baseline}%)")
lines.append("")
lines.append("| Crate | Current | Baseline | Δ | Touched |")
lines.append("|---|---:|---:|---:|:---:|")

regressions: list[tuple[str, float, float]] = []
for pkg in sorted(crate_baselines.keys()):
    base = float(crate_baselines[pkg])
    cur = per_crate.get(pkg)
    touched = pkg in touched_pkgs
    if cur is None:
        delta = "n/a"
        cur_s = "n/a"
    else:
        d = round(cur - base, 2)
        delta = f"{d:+.2f}"
        cur_s = f"{cur:.2f}%"
        if touched and cur + 1e-9 < base:
            regressions.append((pkg, cur, base))
    lines.append(
        f"| `{pkg}` | {cur_s} | {base:.2f}% | {delta} | {'yes' if touched else ''} |"
    )

lines.append("")
lines.append(
    "_Soft check only: regressions on touched crates are annotated and "
    "commented, but do not fail CI yet._"
)

if regressions:
    lines.append("")
    lines.append("### Regressions on touched crates")
    for pkg, cur, base in regressions:
        msg = f"{pkg} coverage {cur:.2f}% is below baseline {base:.2f}%"
        lines.append(f"- {msg}")
        # GitHub Actions annotation (visible on the PR Checks UI).
        print(f"::warning title=Coverage regression::{msg}")
else:
    if touched_pkgs:
        lines.append("")
        lines.append(
            f"No coverage regressions on touched crates: "
            f"{', '.join(sorted(touched_pkgs))}."
        )
    else:
        lines.append("")
        lines.append("No contract crates touched in this change set.")

report = "\n".join(lines) + "\n"
report_md.write_text(report, encoding="utf-8")
summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
if summary_path:
    with open(summary_path, "a", encoding="utf-8") as fh:
        fh.write(report)

print(report)

# Optional PR comment (never fails the job).
if event_name == "pull_request" and os.environ.get("GH_TOKEN"):
    try:
        pr = os.environ.get("GITHUB_REF_NAME", "").replace("/merge", "")
        # Prefer number from event payload when available.
        event_path = os.environ.get("GITHUB_EVENT_PATH")
        pr_number = None
        if event_path and Path(event_path).is_file():
            event = json.loads(Path(event_path).read_text(encoding="utf-8"))
            pr_number = (event.get("number")
                         or (event.get("pull_request") or {}).get("number"))
        if pr_number:
            body = (
                "<!-- invofi-coverage-bot -->\n"
                + report
                + "\nSee `coverage/baseline.json` and CONTRIBUTING.md "
                "for how the baseline is maintained.\n"
            )
            # Upsert: delete prior bot comments then post fresh.
            existing = subprocess.check_output(
                [
                    "gh", "api",
                    f"repos/{os.environ['GITHUB_REPOSITORY']}/issues/{pr_number}/comments",
                    "--jq",
                    '.[] | select(.body | contains("invofi-coverage-bot")) | .id',
                ],
                text=True,
            ).splitlines()
            for cid in existing:
                cid = cid.strip()
                if cid:
                    subprocess.run(
                        [
                            "gh", "api", "-X", "DELETE",
                            f"repos/{os.environ['GITHUB_REPOSITORY']}/issues/comments/{cid}",
                        ],
                        check=False,
                    )
            subprocess.run(
                [
                    "gh", "pr", "comment", str(pr_number),
                    "--body", body,
                ],
                check=False,
            )
    except Exception as exc:  # noqa: BLE001 — soft path
        print(f"Note: could not post PR comment ({exc})", file=sys.stderr)

# Soft gate: always succeed.
sys.exit(0)
PY
