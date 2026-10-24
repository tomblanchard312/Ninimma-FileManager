"""Conservatively classify documentation-only pull requests for CI."""
import json
import os
from pathlib import PurePosixPath
import subprocess


def docs_only(paths):
    return bool(paths) and all(
        path in ("README.md", "CHANGELOG.md", "CONTRIBUTING.md")
        or (path.startswith("docs/") and PurePosixPath(path).suffix == ".md")
        for path in paths
    )


if __name__ == "__main__":
    result = False
    if os.environ.get("GITHUB_EVENT_NAME") == "pull_request":
        with open(os.environ["GITHUB_EVENT_PATH"], encoding="utf-8") as source:
            pr = json.load(source)["pull_request"]
        # Include deletions and renames; any non-documentation path runs full CI.
        changed = subprocess.check_output([
            "git", "diff", "--no-renames", "--name-only", "-z",
            pr["base"]["sha"] + "..." + pr["head"]["sha"], "--",
        ]).decode("utf-8").rstrip("\0").split("\0")
        result = docs_only(changed)
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        output.write(f"docs_only={str(result).lower()}\n")
