#!/usr/bin/env python3
"""Cross-build a Rust binary inside Crabbox; never runs the target executable."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess


RUST_VERSION = "1.92.0"
TARGET = "armv7-unknown-linux-gnueabihf"
TRIPLE = "arm-kindlehf-linux-gnueabihf"
INTERPRETER = "/lib/ld-linux-armhf.so.3"
ARM_FLAGS = ["-mcpu=cortex-a9", "-mfpu=neon", "-mfloat-abi=hard"]


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--toolchain", type=Path, required=True)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument("--bin", default="slint-device-probe")
    parser.add_argument("--features", default="", help="Explicit Cargo features")
    parser.add_argument("--library-dir", type=Path,
                        help="Explicit additional target library directory; default: GCC sysroot only")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_.-]*", args.bin):
        parser.error("--bin must be a Cargo binary name, not a path")
    project = Path(__file__).resolve().parent.parent
    toolchain = args.toolchain.resolve()
    work = args.work.resolve()
    tools = {name: toolchain / "bin" / f"{TRIPLE}-{name}"
             for name in ("gcc", "g++", "ar", "ranlib", "ld", "readelf")}
    for path in tools.values():
        if not path.is_file() or not os.access(path, os.X_OK):
            parser.error(f"missing executable toolchain tool: {path}")
    library_dir = args.library_dir.resolve() if args.library_dir else None
    if library_dir and not library_dir.is_dir():
        parser.error(f"missing target library directory: {library_dir}")
    work.mkdir(parents=True, exist_ok=True)
    target_dir = work / "target"
    evidence = work / "build-evidence.json"
    # Never leave an earlier success report beside a failed rebuild.
    evidence.unlink(missing_ok=True)

    env = os.environ.copy()
    # Keep NIX_LD and NIX_LD_LIBRARY_PATH for host tools. Do not let ambient
    # workstation include/library paths or generic compiler flags reach ARM.
    for name in ("CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH",
                 "PKG_CONFIG_PATH", "PKG_CONFIG_LIBDIR", "PKG_CONFIG_SYSROOT_DIR",
                 "PKG_CONFIG_ALLOW_CROSS", "CC", "CXX", "AR", "RANLIB",
                 "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS", "RUSTFLAGS",
                 "CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_RUSTFLAGS"):
        env.pop(name, None)
    env["LC_ALL"] = "C"
    env["CARGO_TARGET_DIR"] = str(target_dir)
    env["CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER"] = str(tools["gcc"])
    commands = []

    def run(command, log):
        command = [str(arg) for arg in command]
        commands.append(command)
        (work / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        print("+ " + " ".join(command), flush=True)
        with (work / log).open("w") as output:
            result = subprocess.run(command, cwd=project, env=env,
                                    stdout=output, stderr=subprocess.STDOUT)
        text = (work / log).read_text()
        if result.returncode:
            print(text, flush=True)
            raise subprocess.CalledProcessError(result.returncode, command)
        return text

    versions = {}
    for name in tools:
        versions[name] = run([tools[name], "--version"], f"{name}-version.txt")
    versions["gcc_verbose"] = run([tools["gcc"], "-v"], "gcc-verbose.txt")
    machine = run([tools["gcc"], "-dumpmachine"], "gcc-target.txt").strip()
    if machine != TRIPLE:
        raise RuntimeError(f"unexpected GCC target: {machine}")
    sysroot = Path(run([tools["gcc"], "-print-sysroot"], "gcc-sysroot.txt").strip())
    if not sysroot.is_absolute() or not sysroot.is_dir():
        raise RuntimeError(f"GCC did not report an existing absolute sysroot: {sysroot}")
    sysroot = sysroot.resolve()

    target_env = {
        "CC": str(tools["gcc"]), "CXX": str(tools["g++"]),
        "AR": str(tools["ar"]), "RANLIB": str(tools["ranlib"]),
        "CFLAGS": " ".join(ARM_FLAGS), "CXXFLAGS": " ".join(ARM_FLAGS),
        "CPPFLAGS": "", "LDFLAGS": "",
        "PKG_CONFIG_PATH": "",
        "PKG_CONFIG_LIBDIR": os.pathsep.join(str(sysroot / directory) for directory in
                                           ("usr/lib/pkgconfig", "usr/share/pkgconfig", "lib/pkgconfig")),
        "PKG_CONFIG_SYSROOT_DIR": str(sysroot),
        "PKG_CONFIG_ALLOW_CROSS": "1",
    }
    # cc-rs/pkg-config-rs accept target-qualified variables. Set both spellings
    # so an inherited, higher-priority hyphenated variable cannot override us.
    # Host build scripts and proc macros keep their own native compiler/flags.
    for name, value in target_env.items():
        for suffix in (TARGET, TARGET.replace("-", "_")):
            env[f"{name}_{suffix}"] = value
    rust_flags = ["-C", "target-cpu=cortex-a9"]
    rust_flags.extend(f"-Clink-arg={flag}" for flag in ARM_FLAGS)
    if library_dir:
        rust_flags.append(f"-Lnative={library_dir}")
        rust_flags.extend(["-Clink-arg=-Wl,-rpath-link", f"-Clink-arg={library_dir}"])
    # --target is mandatory below: Cargo then applies these only to target
    # crates, never host build scripts/proc macros. Encoding preserves spaces.
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(rust_flags)
    mise = ["mise", "exec", f"rust@{RUST_VERSION}", "--"]
    versions["rustc"] = run(mise + ["rustc", "-Vv"], "rustc-version.txt")
    if not re.search(rf"^release: {re.escape(RUST_VERSION)}$", versions["rustc"], re.M):
        raise RuntimeError(f"unexpected Rust compiler: {versions['rustc']}")
    versions["cargo"] = run(mise + ["cargo", "--version"], "cargo-version.txt")
    lockfile = project / "Cargo.lock"
    locked = lockfile.is_file()
    command = mise + ["cargo", "build", "--release", "--target", TARGET, "--bin", args.bin]
    if args.features:
        command.extend(["--features", args.features])
    # Bootstrap a missing lockfile once; subsequent builds must honor it.
    if locked:
        command.append("--locked")
    run(command, "cargo-build.log")
    binary = target_dir / TARGET / "release" / args.bin
    if not binary.is_file():
        raise RuntimeError(f"Cargo succeeded without the requested executable: {binary}")
    elf = run([tools["readelf"], "-W", "-h", "-l", "-A", "-d", "-V", binary], "binary-elf.txt")
    glibc_versions = sorted(set(re.findall(r"\bGLIBC_(\d+\.\d+(?:\.\d+)?)\b", elf)),
                            key=lambda value: tuple(map(int, value.split("."))))
    interpreters = re.findall(r"\[Requesting program interpreter:\s*([^\]]+)\]", elf)
    needed = re.findall(r"\(NEEDED\).*?\[([^\]]+)\]", elf)
    errors = []
    if not glibc_versions or any(tuple(map(int, version.split("."))) + (0,) * (3 - len(version.split(".")))
                                 > (2, 20, 0) for version in glibc_versions):
        errors.append(f"unacceptable GLIBC requirements: {glibc_versions}")
    if re.search(r"\bGLIBC_(?:ABI_\w+|PRIVATE)\b", elf):
        errors.append("unsupported nonnumeric GLIBC symbol requirement")
    if (not re.search(r"Machine:\s+ARM\s*$", elf, re.M)
            or not re.search(r"Class:\s+ELF32\s*$", elf, re.M)
            or not re.search(r"Data:.*little endian", elf)
            or "hard-float ABI" not in elf
            or not re.search(r"Flags:.*Version5 EABI", elf)):
        errors.append("output is not a little-endian ARM32 EABI5 hard-float ELF")
    if interpreters != [INTERPRETER]:
        errors.append(f"unexpected ELF interpreter: {interpreters}")
    report = {
        "accepted": not errors, "errors": errors, "bin": args.bin,
        "binary": str(binary), "binary_sha256": sha256(binary),
        "binary_bytes": binary.stat().st_size, "target": TARGET, "cpu": "cortex-a9",
        "toolchain": str(toolchain), "sysroot": str(sysroot),
        "library_dir": str(library_dir) if library_dir else None,
        "cargo_target_dir": str(target_dir), "rust_version": RUST_VERSION,
        "compiler_versions": versions, "commands": commands,
        "target_environment": {name: env[name] for name in sorted(env)
                               if name.endswith((TARGET, TARGET.replace("-", "_")))},
        "rust_flags": rust_flags, "interpreter": interpreters,
        "needed": needed, "glibc_versions": glibc_versions,
        "cargo_locked": locked, "cargo_lock_sha256": sha256(lockfile),
        "cargo_manifest_sha256": sha256(project / "Cargo.toml"),
    }
    evidence.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    if errors:
        raise RuntimeError("; ".join(errors))


if __name__ == "__main__":
    main()
