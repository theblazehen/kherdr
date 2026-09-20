#!/usr/bin/env python3
"""Cross-build the headless feasibility probe inside Crabbox; never runs it."""

import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile


HERDR_COMMIT = "9eb521456ac0d19d3ab3d9d7cea3cca10baa8a4c"
GHOSTTY_COMMIT = "c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3"
ZIG_VERSION = "0.15.2"
TRIPLE = "arm-kindlehf-linux-gnueabihf"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--herdr-source", type=Path, required=True)
    parser.add_argument("--toolchain", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    args = parser.parse_args()
    work = args.work.resolve()
    work.mkdir(parents=True, exist_ok=True)
    probe = Path(__file__).resolve().parent.parent / "probe/vt-probe.c"
    if not probe.is_file():
        parser.error(f"missing probe: {probe}")

    # Extract committed vendor bytes, not a possibly modified working tree.
    archive = subprocess.check_output([
        "git", "-C", str(args.herdr_source.resolve()), "archive", HERDR_COMMIT,
        "vendor/libghostty-vt", "vendor/libghostty-vt.vendor.json",
    ])
    digest = hashlib.sha256(archive).hexdigest()
    source_root = work / ("source-" + digest[:16])
    source_root.mkdir(exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(archive)) as tar:
        tar.extractall(source_root, filter="data")
    source = source_root / "vendor/libghostty-vt"
    metadata = json.loads((source_root / "vendor/libghostty-vt.vendor.json").read_text())
    if metadata["source_commit"] != GHOSTTY_COMMIT:
        raise RuntimeError("unexpected Ghostty source pin")

    env = os.environ.copy()
    # Preserve nix-ld for host tool binaries, but prevent host headers/libs
    # from contaminating the ARM target. GCC uses its own pinned sysroot.
    for name in ("CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH",
                 "PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_SYSROOT_DIR",
                 "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS"):
        env.pop(name, None)
    commands = []

    def run(command, log, cwd=work):
        command = [str(arg) for arg in command]
        commands.append(command)
        print("+ " + " ".join(command), flush=True)
        with (work / log).open("w") as output:
            result = subprocess.run(command, cwd=cwd, env=env,
                                    stdout=output, stderr=subprocess.STDOUT)
        (work / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        if result.returncode:
            print((work / log).read_text(), flush=True)
            raise subprocess.CalledProcessError(result.returncode, command)
        return (work / log).read_text()

    zig = ["mise", "exec", f"zig@{ZIG_VERSION}", "--", "zig"]
    actual_zig = run(zig + ["version"], "zig-version.txt").strip()
    if actual_zig != ZIG_VERSION:
        raise RuntimeError(f"unexpected Zig version: {actual_zig}")
    install = work / "install"
    build = zig + [
        "build", "-Demit-lib-vt=true", "-Dversion-string=1.3.2-dev+c5a21edfc",
        "-Dtarget=arm-linux-gnueabihf.2.20", "-Dcpu=cortex_a9",
        "-Doptimize=ReleaseSmall", "--prefix", str(install), "--summary", "all",
    ]
    run(build, "build.log", source)
    prefix = args.toolchain.resolve() / "bin" / TRIPLE
    gcc = str(prefix) + "-gcc"
    compiler = run([gcc, "-v"], "gcc-version.txt")
    binary = work / "kherdr-vt-probe"
    run([
        gcc, "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror",
        "-mcpu=cortex-a9", "-mfpu=neon", "-mfloat-abi=hard",
        "-I", source / "include", probe, install / "lib/libghostty-vt.a",
        "-o", binary, "-lm", "-ldl", "-lpthread", "-lrt",
    ], "probe-link.log")
    elf = run([str(prefix) + "-readelf", "-W", "-h", "-l", "-A", "-d", "-V", binary],
              "probe-elf.txt")
    versions = sorted(set(re.findall(r"GLIBC_(\d+\.\d+(?:\.\d+)?)", elf)),
                      key=lambda value: tuple(map(int, value.split("."))))
    if not versions or any(tuple(map(int, version.split("."))) > (2, 20)
                           for version in versions):
        raise RuntimeError(f"unacceptable GLIBC symbol versions: {versions}")
    if "hard-float ABI" not in elf or "Machine:                           ARM" not in elf:
        raise RuntimeError("probe is not an ARM hard-float ELF")
    report = {
        "herdr_commit": HERDR_COMMIT, "ghostty_commit": GHOSTTY_COMMIT,
        "vendor_archive_sha256": digest, "zig_version": actual_zig,
        "gcc_version_output": compiler, "commands": commands,
        "target": "arm-linux-gnueabihf.2.20", "cpu": "cortex_a9",
        "simd": "upstream default enabled; not overridden",
        "ghostty_linkage": "static archive; executable uses dynamic device libc",
        "probe_source_sha256": hashlib.sha256(probe.read_bytes()).hexdigest(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "binary_bytes": binary.stat().st_size, "glibc_versions": versions,
        "device_execution": "NOT RUN: cross-build is not a feasibility gate pass",
    }
    (work / "build-evidence.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
