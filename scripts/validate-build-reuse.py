"""Permit packaging-only recovery from trusted main builds with unchanged binary inputs."""
import json
import os
import subprocess
import sys


def api(path):
    return json.loads(subprocess.check_output(["gh", "api", path]))


def main():
    run_id = sys.argv[1]
    if not run_id.isdecimal():
        raise ValueError("Build run must be a numeric GitHub run ID")
    repository = os.environ["GITHUB_REPOSITORY"]
    run = api(f"repos/{repository}/actions/runs/{run_id}")
    if (run["head_repository"]["full_name"] != repository or run["head_branch"] != "main"
            or run["event"] not in ("push", "workflow_dispatch") or run["status"] != "completed"
            or run["path"] != ".github/workflows/windows.yml"):
        raise ValueError("Reuse requires a completed, trusted main workflow run")
    jobs = api(f"repos/{repository}/actions/runs/{run_id}/jobs?per_page=100")["jobs"]
    for name in ("rust", "native (integration)", "native (experimental)"):
        if not any(job["name"] == name and job["conclusion"] == "success" for job in jobs):
            raise ValueError(f"Missing successful build: {name}")
    difference = api(f"repos/{repository}/compare/{run['head_sha']}...{os.environ['GITHUB_SHA']}")
    if difference["status"] not in ("ahead", "identical"):
        raise ValueError("Build source must be an ancestor of the packaging revision")
    allowed = {"tests/test_portable.py", ".github/workflows/windows.yml", "SHA256SUMS.txt",
               "scripts/package-windows.py", "scripts/archive-source.py", "scripts/validate-build-reuse.py"}
    files = difference["files"]
    if len(files) >= 300:
        raise ValueError("Comparison may be truncated; rebuild instead")
    for changed in files:
        for name in (changed["filename"], changed.get("previous_filename", changed["filename"])):
            if name not in allowed and not name.startswith(("packaging/", "docs/")):
                raise ValueError(f"Binary build input changed: {name}; rebuild instead")
    print(f"Reusing tested binaries from {run['head_sha']} (run {run_id}); binary inputs unchanged")


if __name__ == "__main__":
    main()
