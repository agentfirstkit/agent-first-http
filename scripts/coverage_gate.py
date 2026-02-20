#!/usr/bin/env python3
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Optional


def load_json(path: Path) -> dict:
    with path.open("r", encoding="utf-8") as f:
        return json.load(f)


def run_coverage(cwd: Path, report_path: Path) -> None:
    profile_dir = cwd / "target" / "llvm-cov-target" / "profiles"
    profile_dir.mkdir(parents=True, exist_ok=True)

    cmd = [
        "cargo",
        "llvm-cov",
        "--all-targets",
        "--json",
        "--output-path",
        str(report_path),
    ]
    env = os.environ.copy()
    env["LLVM_PROFILE_FILE"] = str(profile_dir / "afhttp-%p-%m.profraw")
    subprocess.run(cmd, cwd=cwd, env=env, check=True)


def pct(summary: dict, key: str) -> float:
    return float(summary[key]["percent"])


def find_file_summary(report: dict, rel_path: str) -> Optional[dict]:
    suffix = "/" + rel_path
    for entry in report["data"][0]["files"]:
        if entry["filename"].endswith(suffix):
            return entry["summary"]
    return None


def main() -> int:
    parser = argparse.ArgumentParser(description="Coverage gate for agent-first-http")
    parser.add_argument("--root", default=".", help="Project root (default: current dir)")
    parser.add_argument(
        "--policy",
        default="coverage-policy.json",
        help="Coverage policy JSON path, relative to root",
    )
    parser.add_argument(
        "--report",
        default="target/llvm-cov-report.json",
        help="Coverage report output path, relative to root",
    )
    parser.add_argument(
        "--no-run",
        action="store_true",
        help="Do not run cargo llvm-cov; only evaluate existing report",
    )
    args = parser.parse_args()

    root = Path(args.root).resolve()
    policy_path = (root / args.policy).resolve()
    report_path = (root / args.report).resolve()

    policy = load_json(policy_path)

    if not args.no_run:
        run_coverage(root, report_path)

    report = load_json(report_path)
    totals = report["data"][0]["totals"]

    global_regions = pct(totals, "regions")
    global_lines = pct(totals, "lines")
    req_global_regions = float(policy["global"]["regions"])
    req_global_lines = float(policy["global"]["lines"])

    print(
        f"Global coverage: regions={global_regions:.2f}% (min {req_global_regions:.2f}%), "
        f"lines={global_lines:.2f}% (min {req_global_lines:.2f}%)"
    )

    failures: list[str] = []
    if global_regions < req_global_regions:
        failures.append(
            f"global regions below threshold: {global_regions:.2f}% < {req_global_regions:.2f}%"
        )
    if global_lines < req_global_lines:
        failures.append(
            f"global lines below threshold: {global_lines:.2f}% < {req_global_lines:.2f}%"
        )

    for rel_path, min_cfg in policy.get("core_files", {}).items():
        summary = find_file_summary(report, rel_path)
        if summary is None:
            failures.append(f"core file missing in coverage report: {rel_path}")
            continue
        file_regions = pct(summary, "regions")
        file_lines = pct(summary, "lines")
        min_regions = float(min_cfg["regions"])
        min_lines = float(min_cfg["lines"])
        print(
            f"Core {rel_path}: regions={file_regions:.2f}% (min {min_regions:.2f}%), "
            f"lines={file_lines:.2f}% (min {min_lines:.2f}%)"
        )
        if file_regions < min_regions:
            failures.append(
                f"{rel_path} regions below threshold: {file_regions:.2f}% < {min_regions:.2f}%"
            )
        if file_lines < min_lines:
            failures.append(
                f"{rel_path} lines below threshold: {file_lines:.2f}% < {min_lines:.2f}%"
            )

    exempt = policy.get("exempt_files", [])
    if exempt:
        print("Exempt files (tracked, not gated):")
        for rel_path in exempt:
            summary = find_file_summary(report, rel_path)
            if summary is None:
                print(f"  - {rel_path}: missing in report")
                continue
            print(
                f"  - {rel_path}: regions={pct(summary, 'regions'):.2f}%, "
                f"lines={pct(summary, 'lines'):.2f}%"
            )

    targets = policy.get("targets", {}).get("global")
    if targets:
        print(
            "Long-term target: "
            f"regions={float(targets['regions']):.2f}% lines={float(targets['lines']):.2f}%"
        )

    if failures:
        print("\nCoverage gate FAILED:")
        for msg in failures:
            print(f"  - {msg}")
        return 1

    print("\nCoverage gate PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
