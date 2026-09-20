#!/usr/bin/env python3
"""Package an exact Rust kherdr build as a deterministic, credential-free KPM package."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import tarfile
import tempfile
import tomllib


PACKAGE_ID = "kherdr"
LOCAL_HERDR_COMMIT = "b99002ac99b09e00b4ca692436cb15a6b0d676f1"
LOCAL_HERDR_PATCH_SHA256 = "e939777701ff50f67b98ba68f009bc7585566bd98c8075220a51cd68fa8671a1"
EXAMPLE = b"""# Example only: no host or credentials are supplied.
# Add saved hosts in Hosts; native SSH needs no external client or remote helper.
# Local Herdr owns local shells and SSH panes; remote Herdr connects independently.
# Prefer Herdr falls back to SSH in Local only on explicit initial unavailability.
# Opening a host discovers all running remote Herdr sessions and attaches the preferred one.
# Connect all running opens every discovered session; closing one forgets it for restart restore.
# Discovery never starts or stops servers; remote stock Herdr setup stays explicit.
# Use the native manager for key generation/import, password auth, and host trust.
# Verify the server fingerprint independently before accepting a new host key.
# A valid legacy etc/connection.ini is imported only when connections.json is absent.
# The KPM launcher imports a manual installation once into /mnt/us/kherdr/etc.
# Public packages never contain live settings, keys, or private provisioning seeds.
[connection]
# Uncomment and replace these with your actual SSH destination:
# host=your-server.example
# user=your-user
port=22
keepalive=30
compression=false
# Legacy INI imports retain Herdr mode and key authentication.
# Choose SSH in Local or password authentication in Hosts when needed.
# identity=/mnt/us/kherdr/etc/.ssh/identity
"""


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def local_herdr_receipt(binary, evidence, patch, license_dir):
    """Bind the supplied runtime to the approved stock source/build-only patch."""
    for path in (binary, evidence, patch, license_dir / "source/Cargo.toml", license_dir / "source/LICENSE"):
        if path.is_symlink() or not path.is_file() or not path.stat().st_size:
            raise ValueError(f"local Herdr input must be a nonempty regular file: {path}")
    if evidence.stat().st_size > 8 * 1024 * 1024 or patch.stat().st_size > 1024 * 1024:
        raise ValueError("local Herdr provenance exceeds its bounded document size")
    receipt = json.loads(evidence.read_bytes())
    if not isinstance(receipt, dict) or receipt.get("accepted") is not True or receipt.get("errors") != []:
        raise ValueError("local Herdr build receipt is not accepted")
    if (receipt.get("upstream_commit") != LOCAL_HERDR_COMMIT
            or receipt.get("target") != "armv7-unknown-linux-gnueabihf"
            or receipt.get("runtime_and_vendor_implementation_unchanged") is not True):
        raise ValueError("local Herdr must use the approved stock v0.9.0 ARM source")
    if receipt.get("binary_sha256") != sha256(binary) or receipt.get("binary_bytes") != binary.stat().st_size:
        raise ValueError("local Herdr binary does not match its build receipt")
    with binary.open("rb") as stream:
        header = stream.read(20)
    if header[:7] != b"\x7fELF\x01\x01\x01" or header[18:20] != b"\x28\x00":
        raise ValueError("local Herdr must be a real little-endian ARM ELF executable")
    patch_digest = sha256(patch)
    if patch_digest != LOCAL_HERDR_PATCH_SHA256 or receipt.get("patch", {}).get("sha256") != patch_digest:
        raise ValueError("local Herdr patch differs from the approved build-only changes")
    for name in ("Cargo.toml", "LICENSE"):
        if receipt.get("source_files", {}).get(name, {}).get("sha256") != sha256(license_dir / "source" / name):
            raise ValueError(f"local Herdr source/{name} does not match the receipt")
    package = tomllib.loads((license_dir / "source/Cargo.toml").read_text())["package"]
    if package.get("name") != "herdr" or package.get("version") != "0.9.0":
        raise ValueError("local Herdr source manifest is not herdr v0.9.0")
    dependencies = license_dir / "dependencies"
    if not dependencies.is_dir() or not any(path.is_file() for path in dependencies.rglob("*")):
        raise ValueError("local Herdr licenses require a nonempty dependencies/ notice tree")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True,
                        help="Actual cross-built kherdr executable, not a device probe")
    parser.add_argument("--build-evidence", type=Path, required=True,
                        help="Accepted tools/build-rust.py receipt for this exact kherdr binary")
    parser.add_argument("--output", type=Path, required=True, help="Output .kpkg archive")
    parser.add_argument("--license-dir", type=Path, required=True,
                        help="Prepared nonempty source/dependency license and notice tree for this exact build; copied verbatim")
    parser.add_argument("--local-herdr", type=Path, required=True)
    parser.add_argument("--local-herdr-evidence", type=Path, required=True)
    parser.add_argument("--local-herdr-patch", type=Path, required=True)
    parser.add_argument("--local-herdr-license-dir", type=Path, required=True,
                        help="Exact runtime notice tree: source/Cargo.toml, source/LICENSE, and dependencies/ including vendored libraries")
    args = parser.parse_args()
    project = Path(__file__).resolve().parent.parent
    metadata = tomllib.loads((project / "Cargo.toml").read_text())["package"]
    try:
        version = [int(part) for part in metadata["version"].split(".")]
        if len(version) != 3 or any(part < 0 for part in version):
            raise ValueError("KPM requires three nonnegative version components")
        receipt = json.loads(args.build_evidence.read_bytes())
        if (receipt.get("accepted") is not True or receipt.get("errors") != []
                or receipt.get("bin") != "kherdr"
                or receipt.get("target") != "armv7-unknown-linux-gnueabihf"
                or receipt.get("binary_sha256") != sha256(args.binary)
                or receipt.get("binary_bytes") != args.binary.stat().st_size
                or receipt.get("cargo_manifest_sha256") != sha256(project / "Cargo.toml")
                or receipt.get("cargo_lock_sha256") != sha256(project / "Cargo.lock")):
            raise ValueError("kherdr binary/build receipt does not match the release inputs")
    except (OSError, ValueError, TypeError, KeyError) as error:
        parser.error(str(error))
    # Values are (source Path or generated bytes, archive mode). Runtime files
    # are intentionally absent: extracting an update cannot overwrite them.
    files = {}

    def add(name, path, mode):
        if path.is_symlink() or not path.is_file():
            parser.error(f"input must be a regular file, not a symlink: {path}")
        if path.stat().st_size == 0:
            parser.error(f"input is empty: {path}")
        if any(ord(c) < 32 or ord(c) == 127 or c == "\\" for c in name):
            parser.error(f"unsupported archive filename: {name!r}")
        if name in files:
            parser.error(f"duplicate archive filename: {name!r}")
        files[name] = (path, mode)

    add("bin/kherdr", args.binary, 0o755)
    add("bin/herdr", args.local_herdr, 0o755)
    add("provenance/kherdr/build-evidence.json", args.build_evidence, 0o644)
    add("provenance/local-herdr/build-evidence.json", args.local_herdr_evidence, 0o644)
    add("provenance/local-herdr/build-only.patch", args.local_herdr_patch, 0o644)
    try:
        local_receipt = local_herdr_receipt(args.local_herdr, args.local_herdr_evidence,
                                           args.local_herdr_patch, args.local_herdr_license_dir)
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.error(str(error))
    files["BINARY_SHA256SUMS"] = ((f"{receipt['binary_sha256']}  kherdr\n"
                                  f"{local_receipt['binary_sha256']}  herdr\n").encode(), 0o644)
    add("kherdr-icon.png", project / "kindle.pkg" / "kherdr-icon.png", 0o644)
    add("kherdr-cover.svg", project / "kindle.pkg" / "kherdr-cover.svg", 0o644)
    add("HERDR-ARTWORK-LICENSE", project / "kindle.pkg" / "HERDR-ARTWORK-LICENSE", 0o644)
    for name in ("install.sh", "launch.sh", "uninstall.sh", "scriptlet.sh", "bin/kherdr.sh"):
        add(name, project / "kindle.pkg" / name, 0o755)
    manifest = {"manifest_version": 2, "id": PACKAGE_ID, "name": "kherdr", "author": "theblazehen",
                "description": "Native Kindle terminal client for local and remote Herdr sessions.",
                "version": version, "dependencies": [], "supported_platforms": ["kindlehf"]}
    files["manifest.json"] = ((json.dumps(manifest, indent=2) + "\n").encode(), 0o644)
    files["defaults/connection.ini.example"] = (EXAMPLE, 0o644)
    if args.license_dir.is_symlink() or not args.license_dir.is_dir():
        parser.error("--license-dir must be an existing directory, not a symlink")
    license_count = 0
    for path in sorted(args.license_dir.rglob("*")):
        if path.is_symlink():
            parser.error(f"license tree must not contain symlinks: {path}")
        if path.relative_to(args.license_dir).parts[0] == "local-herdr":
            parser.error("licenses/local-herdr is reserved for --local-herdr-license-dir")
        if path.is_dir():
            continue
        add("licenses/" + path.relative_to(args.license_dir).as_posix(), path, 0o644)
        license_count += 1
    if not license_count:
        parser.error("--license-dir must contain the build's actual license/notice material")
    if args.local_herdr_license_dir.is_symlink() or not args.local_herdr_license_dir.is_dir():
        parser.error("--local-herdr-license-dir must be an existing directory, not a symlink")
    for path in sorted(args.local_herdr_license_dir.rglob("*")):
        if path.is_symlink():
            parser.error(f"local Herdr license tree must not contain symlinks: {path}")
        if not path.is_dir():
            add("licenses/local-herdr/" + path.relative_to(args.local_herdr_license_dir).as_posix(), path, 0o644)

    output = args.output.resolve()
    if any(output.is_relative_to(path.resolve()) for path in (args.license_dir, args.local_herdr_license_dir)):
        parser.error("--output must be outside both license trees to avoid packaging previous archives")
    for source, _mode in files.values():
        if isinstance(source, Path) and source.resolve() == output:
            parser.error("--output must not overwrite an input")
    manifest = []
    for name, (source, _mode) in sorted(files.items()):
        digest = sha256(source) if isinstance(source, Path) else hashlib.sha256(source).hexdigest()
        manifest.append(f"{digest}  {name}\n")
    # Standard sha256sum format, relative to the extension root; excludes itself.
    files["SHA256SUMS"] = ("".join(manifest).encode("utf-8"), 0o644)
    directories = set()
    for name in files:
        parent = Path(name).parent
        while parent != Path("."):
            directories.add(parent.as_posix())
            parent = parent.parent

    output.parent.mkdir(parents=True, exist_ok=True)
    # A failed package must not truncate a previous release artifact.
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(dir=output.parent, prefix=f".{output.name}.", delete=False) as raw:
            temporary = Path(raw.name)
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0, compresslevel=9) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                    for name in sorted(directories):
                        info = tarfile.TarInfo(name + "/")
                        info.type = tarfile.DIRTYPE
                        info.mode = 0o755
                        archive.addfile(info)
                    for name, (source, mode) in sorted(files.items()):
                        info = tarfile.TarInfo(name)
                        info.mode = mode
                        # TarInfo defaults provide fixed uid/gid/mtime=0 and
                        # empty user/group names, independent of the build host.
                        if isinstance(source, Path):
                            with source.open("rb") as stream:
                                info.size = source.stat().st_size
                                archive.addfile(info, stream)
                        else:
                            info.size = len(source)
                            archive.addfile(info, io.BytesIO(source))
        temporary.replace(output)
        temporary = None
    finally:
        if temporary is not None:
            temporary.unlink(missing_ok=True)
    print(f"{sha256(output)}  {output}")


if __name__ == "__main__":
    main()
