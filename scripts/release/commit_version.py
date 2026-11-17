"""Commit release versions on main and push before exposing a build SHA."""
import argparse
import os
from pathlib import Path
import subprocess

from addon_release import manifests
from prepare_version import prepare, validate_versions


def git(root, *args):
    return subprocess.check_output(["git", *args], cwd=root, text=True).strip()


def commit_version(tag, root=Path(".")):
    if git(root, "branch", "--show-current") != "main":
        raise ValueError("Release preparation requires a main checkout")
    if git(root, "status", "--porcelain"):
        raise ValueError("Release preparation requires a clean checkout")
    version = prepare(tag, root)
    validate_versions(tag, root)
    files = ["Cargo.toml", "Cargo.lock"]
    for path, _ in manifests(root):
        files.extend(str(p.relative_to(root)) for p in (
            path, path.with_name("Cargo.toml"), path.with_name("Cargo.lock")))
    git(root, "add", "--", *files)
    if git(root, "diff", "--cached", "--name-only"):
        git(root, "commit", "-m", f"chore: release {version}")
    # A normal push rejects concurrent changes and branch protection failures.
    git(root, "push", "origin", "HEAD:refs/heads/main")
    return git(root, "rev-parse", "HEAD")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    args = parser.parse_args()
    sha = commit_version(args.tag)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"sha={sha}\n")
    print(f"Release source committed to main: {sha}")
