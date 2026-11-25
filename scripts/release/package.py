#!/usr/bin/env python3
"""Package a native release build; never include workspace data or credentials."""
import argparse
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile
import tempfile
from prepare_version import validate_versions
from addon_release import TARGETS, RELEASE_TARGETS, safe_tag

MACOS_TARGETS = ("x86_64-apple-darwin", "aarch64-apple-darwin")


def asset_name(tag, target):
    # Git tags may contain slashes; asset names must be single safe filenames.
    tag = safe_tag(tag)
    platform, extension = TARGETS[target]
    return f"focus-editor-{tag}-{platform}{extension}"


def run(*args, **kwargs):
    return subprocess.check_output(args, text=True, **kwargs).strip()


def verify_binary(binary, target, name="focus-editor", expected_version=None):
    description = run("file", "-b", str(binary))
    print(description)
    architecture = {
        "x86_64-unknown-linux-gnu": "x86-64",
        "aarch64-unknown-linux-gnu": "aarch64",
        "x86_64-apple-darwin": "x86_64",
        "aarch64-apple-darwin": "arm64",
    }[target]
    if architecture not in description:
        raise RuntimeError(f"Wrong executable architecture: expected {architecture}")
    if target.endswith("linux-gnu"):
        libraries = run("ldd", str(binary))
        print(libraries)
        if "libghostty-vt.so" in libraries:
            raise RuntimeError("Ghostty must be statically linked in release binaries")
        if "not found" in libraries:
            raise RuntimeError("Unresolved Linux shared libraries")
    else:
        if run("lipo", "-archs", str(binary)) != architecture:
            raise RuntimeError(f"Expected a native {architecture} Mach-O executable")
        libraries = run("otool", "-L", str(binary))
        print(libraries)
        # No runner-local Homebrew or build-directory dependencies may ship.
        for line in libraries.splitlines()[1:]:
            dependency = line.strip().split(" (", 1)[0]
            if not dependency.startswith(("/usr/lib/", "/System/Library/")):
                raise RuntimeError(f"Unbundled macOS dependency: {dependency}")
    version = run(str(binary), "--version")
    if not version.startswith(name + " ") or (expected_version is not None and version != f"{name} {expected_version}"):
        raise RuntimeError("Executable version smoke test failed")
    print(version)


def package_linux(binary, output, stage):
    shutil.copy2(binary, stage / "focus-editor")
    shutil.copy2(binary.with_name("focus-editor-server"), stage / "focus-editor-server")
    shutil.copy2("scripts/release/INSTALL-linux.txt", stage / "INSTALL.txt")
    shutil.copy2("LICENSE", stage / "LICENSE")
    shutil.copy2("THIRD_PARTY_NOTICES.txt", stage / "THIRD_PARTY_NOTICES.txt")
    assets = ("focus-editor", "focus-editor-server", "INSTALL.txt", "LICENSE", "THIRD_PARTY_NOTICES.txt")
    with tarfile.open(output, "w:gz") as archive:
        for name in assets:
            archive.add(stage / name, arcname=name)
    with tarfile.open(output) as archive:
        if set(archive.getnames()) != set(assets):
            raise RuntimeError("Unexpected Linux archive contents")
        for name in ("focus-editor", "focus-editor-server"):
            if not archive.getmember(name).mode & 0o111:
                raise RuntimeError("Packaged executable lost its execute permission")


def package_macos(binary, output, stage):
    metadata = json.loads(run("cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"))
    version = next(p["version"] for p in metadata["packages"] if p["name"] == "focus-editor")
    app = stage / "Focus Editor.app"
    contents = app / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    (contents / "Resources").mkdir()
    executable = contents / "MacOS/focus-editor"
    shutil.copy2(binary, executable)
    shutil.copy2(binary.with_name("focus-editor-server"), contents / "MacOS/focus-editor-server")
    shutil.copy2("LICENSE", contents / "Resources/LICENSE")
    shutil.copy2("THIRD_PARTY_NOTICES.txt", contents / "Resources/THIRD_PARTY_NOTICES.txt")
    # Keep the bundle's minimum OS aligned with the release build environment.
    with (contents / "Info.plist").open("wb") as stream:
        plistlib.dump({
            "CFBundleIdentifier": "io.github.nfranchet.focus-editor",
            "CFBundleName": "Focus Editor",
            "CFBundleDisplayName": "Focus Editor",
            "CFBundleExecutable": "focus-editor",
            "CFBundlePackageType": "APPL",
            "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleShortVersionString": version.split("-", 1)[0].split("+", 1)[0],
            "CFBundleVersion": version.split("-", 1)[0].split("+", 1)[0],
            "LSMinimumSystemVersion": os.environ.get("MACOSX_DEPLOYMENT_TARGET", "11.0"),
            "NSHighResolutionCapable": True,
        }, stream)
    run("plutil", "-lint", str(contents / "Info.plist"))
    run("codesign", "--force", "--sign", "-", str(contents / "MacOS/focus-editor-server"))
    run("codesign", "--force", "--sign", "-", str(app))
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app))
    os.symlink("/Applications", stage / "Applications")
    shutil.copy2(Path(__file__).with_name("INSTALL-macos.txt"), stage / "INSTALL.txt")
    shutil.copy2("LICENSE", stage / "LICENSE")
    shutil.copy2("THIRD_PARTY_NOTICES.txt", stage / "THIRD_PARTY_NOTICES.txt")
    run("hdiutil", "create", "-volname", "Focus Editor", "-srcfolder", str(stage),
        "-format", "UDZO", "-ov", str(output))
    run("hdiutil", "verify", str(output))
    # Verify the mounted image, including its app signature and drag-and-drop link.
    mount = stage.parent / "mounted"
    mount.mkdir()
    run("hdiutil", "attach", "-readonly", "-nobrowse", "-mountpoint", str(mount), str(output))
    try:
        assert (mount / "Applications").is_symlink()
        assert os.readlink(mount / "Applications") == "/Applications"
        run("codesign", "--verify", "--deep", "--strict", str(mount / "Focus Editor.app"))
        assert run(str(mount / "Focus Editor.app/Contents/MacOS/focus-editor"), "--version").startswith("focus-editor ")
        assert run(str(mount / "Focus Editor.app/Contents/MacOS/focus-editor-server"), "--version").startswith("focus-editor-server ")
    finally:
        run("hdiutil", "detach", str(mount))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("tag")
    args = parser.parse_args()
    binary = Path("target", args.target, "release/focus-editor").resolve(strict=True)
    version = validate_versions(args.tag)
    verify_binary(binary, args.target, expected_version=version)
    verify_binary(binary.with_name("focus-editor-server"), args.target, "focus-editor-server", version)
    destination = Path("dist").resolve()
    destination.mkdir(exist_ok=True)
    output = destination / asset_name(args.tag, args.target)
    with tempfile.TemporaryDirectory(prefix="focus-release-") as temporary:
        stage = Path(temporary) / "payload"
        stage.mkdir()
        if args.target.endswith("apple-darwin"):
            package_macos(binary, output, stage)
        else:
            package_linux(binary, output, stage)
    print(f"Verified release asset: {output.name}")


if __name__ == "__main__":
    main()
