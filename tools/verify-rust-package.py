#!/usr/bin/env python3
"""Package the exact Rust build twice and verify the credential-free KPM archive."""

import argparse
import configparser
import hashlib
import json
from pathlib import Path, PurePosixPath
import subprocess
import sys
import tarfile
import tempfile
import tomllib


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--build-evidence", type=Path, required=True)
    parser.add_argument("--license-dir", type=Path, required=True)
    parser.add_argument("--local-herdr", type=Path, required=True)
    parser.add_argument("--local-herdr-evidence", type=Path, required=True)
    parser.add_argument("--local-herdr-patch", type=Path, required=True)
    parser.add_argument("--local-herdr-license-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    project = Path(__file__).resolve().parent.parent
    command = [sys.executable, str(project / "tools/package-rust.py"),
               "--binary", str(args.binary), "--license-dir", str(args.license_dir)]
    for flag in ("build-evidence", "local-herdr", "local-herdr-evidence", "local-herdr-patch", "local-herdr-license-dir"):
        command.extend(["--" + flag, str(getattr(args, flag.replace("-", "_")))])
    subprocess.run(command + ["--output", str(args.output)], check=True)
    with tempfile.TemporaryDirectory(prefix="kherdr-package-", dir=args.output.resolve().parent) as work:
        second = Path(work) / "repeat.kpkg"
        subprocess.run(command + ["--output", str(second)], check=True)
        digest = sha256(args.output)
        require(digest == sha256(second), "Repeated packaging produced different archive bytes")

    expected = {name: project / "kindle.pkg" / name for name in
                ("install.sh", "launch.sh", "uninstall.sh", "scriptlet.sh", "kherdr-icon.png",
                 "kherdr-cover.svg", "HERDR-ARTWORK-LICENSE", "bin/kherdr.sh")}
    expected["bin/kherdr"] = args.binary
    expected["bin/herdr"] = args.local_herdr
    expected["provenance/kherdr/build-evidence.json"] = args.build_evidence
    expected["provenance/local-herdr/build-evidence.json"] = args.local_herdr_evidence
    expected["provenance/local-herdr/build-only.patch"] = args.local_herdr_patch
    for path in args.license_dir.rglob("*"):
        if path.is_file():
            expected["licenses/" + path.relative_to(args.license_dir).as_posix()] = path
    for path in args.local_herdr_license_dir.rglob("*"):
        if path.is_file():
            expected["licenses/local-herdr/" + path.relative_to(args.local_herdr_license_dir).as_posix()] = path
    expected_names = set(expected) | {"defaults/connection.ini.example", "manifest.json", "BINARY_SHA256SUMS", "SHA256SUMS"}
    expected_directories = set()
    for name in expected_names:
        expected_directories.update(str(parent) for parent in PurePosixPath(name).parents
                                    if str(parent) != ".")
    with args.binary.open("rb") as binary:
        require(binary.read(4) == b"\x7fELF", "Supplied binary is not an ELF executable")
    with tarfile.open(args.output, "r:gz") as archive:
        files = {}
        directories = set()
        seen = set()
        for member in archive.getmembers():
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and ".." not in path.parts and path.parts,
                    f"Unsafe archive path: {member.name!r}")
            require(member.name not in seen, f"Duplicate archive member: {member.name!r}")
            seen.add(member.name)
            require(member.uid == member.gid == member.mtime == 0 and
                    member.uname == member.gname == "", f"Noncanonical metadata: {member.name!r}")
            require(member.isfile() or member.isdir(), f"Unexpected member type: {member.name!r}")
            if member.isdir():
                directories.add(member.name.rstrip("/"))
                mode = 0o755
            else:
                name = path.as_posix()
                require(path.parts[0] not in {"etc", "var", ".ssh"}, "Archive must not contain live user state")
                files[name] = member
                mode = 0o755 if name.startswith("bin/") or name in {"install.sh", "launch.sh", "uninstall.sh", "scriptlet.sh"} else 0o644
            require(member.mode == mode, f"Unexpected mode: {member.name!r}")
        require(set(files) == expected_names, "Archive file set differs from exact packaging inputs")
        require(directories == expected_directories, "Archive directory set differs from expected paths")
        example = configparser.ConfigParser(interpolation=None)
        example.read_string(archive.extractfile(files["defaults/connection.ini.example"]).read().decode("utf-8"))
        require(example.sections() == ["connection"] and not example.defaults() and
                not {"host", "user", "identity", "backend", "program", "command"}.intersection(example["connection"]),
                "Example must not supply a destination, credentials, external client, or obsolete helper command")
        manifest = archive.extractfile(files["SHA256SUMS"]).read().decode("utf-8")
        actual_manifest = []
        digests = {}
        for name in sorted(expected_names - {"SHA256SUMS"}):
            with archive.extractfile(files[name]) as stream:
                file_digest = hashlib.file_digest(stream, "sha256").hexdigest()
            digests[name] = file_digest
            actual_manifest.append(f"{file_digest}  {name}\n")
            if name in expected:
                require(file_digest == sha256(expected[name]), f"Input/archive mismatch: {name!r}")
        require(manifest == "".join(actual_manifest), "SHA256SUMS does not exactly cover archive payload")
        binary_manifest = archive.extractfile(files["BINARY_SHA256SUMS"]).read().decode("utf-8")
        require(binary_manifest == f"{digests['bin/kherdr']}  kherdr\n{digests['bin/herdr']}  herdr\n",
                "Retained runtime manifest does not bind the exact executable pair")
        receipt_name = "provenance/local-herdr/build-evidence.json"
        require(files[receipt_name].size <= 8 * 1024 * 1024, "Local Herdr receipt exceeds size limit")
        receipt = json.load(archive.extractfile(files[receipt_name]))
        require(receipt.get("accepted") is True and receipt.get("errors") == []
                and receipt.get("upstream_commit") == "b99002ac99b09e00b4ca692436cb15a6b0d676f1"
                and receipt.get("target") == "armv7-unknown-linux-gnueabihf"
                and receipt.get("runtime_and_vendor_implementation_unchanged") is True,
                "Packaged local Herdr receipt does not identify the approved stock build")
        require(receipt.get("binary_sha256") == digests["bin/herdr"]
                and receipt.get("binary_bytes") == files["bin/herdr"].size,
                "Packaged local Herdr does not match its receipt")
        header = archive.extractfile(files["bin/herdr"]).read(20)
        require(header[:7] == b"\x7fELF\x01\x01\x01" and header[18:20] == b"\x28\x00",
                "Packaged local Herdr is not a real ARM ELF executable")
        patch_digest = digests["provenance/local-herdr/build-only.patch"]
        require(patch_digest == "e939777701ff50f67b98ba68f009bc7585566bd98c8075220a51cd68fa8671a1"
                and receipt.get("patch", {}).get("sha256") == patch_digest,
                "Packaged local Herdr patch is not the approved build-only patch")
        for name in ("Cargo.toml", "LICENSE"):
            require(receipt.get("source_files", {}).get(name, {}).get("sha256")
                    == digests["licenses/local-herdr/source/" + name],
                    f"Packaged local Herdr source/{name} differs from receipt")
        package = tomllib.loads(archive.extractfile(files["licenses/local-herdr/source/Cargo.toml"]).read().decode())["package"]
        require(package.get("name") == "herdr" and package.get("version") == "0.9.0",
                "Packaged local Herdr source version is not v0.9.0")
        require(any(name.startswith("licenses/local-herdr/dependencies/") for name in files),
                "Packaged local Herdr dependency notices are absent")
        package_manifest = json.load(archive.extractfile(files["manifest.json"]))
        project_version = tomllib.loads((project / "Cargo.toml").read_text())["package"]["version"]
        require(package_manifest.get("manifest_version") == 2
                and package_manifest.get("id") == "kherdr"
                and package_manifest.get("version") == [int(part) for part in project_version.split(".")]
                and package_manifest.get("supported_platforms") == ["kindlehf"]
                and package_manifest.get("dependencies") == [], "Invalid KPM package manifest")
        client_receipt = json.load(archive.extractfile(files["provenance/kherdr/build-evidence.json"]))
        require(client_receipt.get("binary_sha256") == digests["bin/kherdr"]
                and client_receipt.get("binary_bytes") == files["bin/kherdr"].size,
                "Packaged kherdr binary does not match its build receipt")
    print(f"Verified deterministic KPM archive, exact input digests, manifest, modes, and provenance: {digest}")
    print("No live settings or private provisioning seeds are present. KPM lifecycle verification is separate.")


if __name__ == "__main__":
    main()
