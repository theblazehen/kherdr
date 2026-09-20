#!/usr/bin/env python3
"""Build pinned Herdr in Crabbox using already installed tools and cached dependencies.

Never installs tools, downloads dependencies, runs the ARM executable, or changes
its input checkout. Each attempt requires a new, empty --work directory. A failed
attempt retains its logs and an accepted=false build-evidence.json receipt.
"""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys


COMMIT = "b99002ac99b09e00b4ca692436cb15a6b0d676f1"
RUST_VERSION = "1.92.0"
ZIG_VERSION = "0.15.2"
BINDGEN_VERSION = "0.72.1"
TARGET = "armv7-unknown-linux-gnueabihf"
TRIPLE = "arm-kindlehf-linux-gnueabihf"
INTERPRETER = "/lib/ld-linux-armhf.so.3"
ARM_FLAGS = ["-mcpu=cortex-a9", "-mfpu=neon", "-mfloat-abi=hard"]


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def progress(message, *, file=sys.stdout):
    # A native Crabbox child can outlive its forwarding connection. Losing
    # optional console output must not replace the build result or its receipt.
    try:
        print(message, file=file, flush=True)
    except BrokenPipeError:
        # Also let Python flush its stream at exit without replacing the build's
        # exit status with another BrokenPipeError.
        with open(os.devnull, "w") as sink:
            os.dup2(sink.fileno(), file.fileno())


def target_layout_checks(original, generated):
    checks = re.compile(
        r"^#\[allow\(clippy::unnecessary_operation, clippy::identity_op\)\]\n"
        r"const _: \(\) = \{\n.*?^\};\n", re.M | re.S)
    original_runtime, original_count = checks.subn("", original)
    generated_runtime, generated_count = checks.subn("", generated)
    # libclang versions can attach a repeated documentation attribute differently.
    # Keep the original documentation, declarations and implementations byte-for-byte.
    docs = re.compile(r'^#\[doc = "(?:\\.|[^"\\])*"\]\n', re.M)
    if (not original_count or original_count != generated_count
            or docs.sub("", original_runtime) != docs.sub("", generated_runtime)):
        raise RuntimeError("target binding generation changed runtime declarations; stop for discussion")
    blocks = checks.findall(generated)
    for before, after in zip(checks.findall(original), blocks):
        if re.findall(r'\["([^"]+)"\]', before) != re.findall(r'\["([^"]+)"\]', after):
            raise RuntimeError("target generation changed layout-check type or field coverage")
    replacements = iter(blocks)
    result = checks.sub(lambda _: next(replacements), original)
    if checks.sub("", result) != original_runtime:
        raise RuntimeError("runtime bindings changed outside generated layout assertions")
    return result, original_count, hashlib.sha256(original_runtime.encode()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True,
                        help="Clean, complete local Git checkout of the pinned upstream commit")
    parser.add_argument("--work", type=Path, required=True,
                        help="New or empty attempt directory outside the source checkout")
    parser.add_argument("--toolchain", type=Path, required=True,
                        help="Existing KOReader hard-float toolchain root")
    parser.add_argument("--zig-system-dir", type=Path, required=True,
                        help="Existing Zig package system directory; --system prevents package downloads")
    parser.add_argument("--bindgen", type=Path, required=True,
                        help="Existing bindgen 0.72.1 executable")
    parser.add_argument("--libclang", type=Path, required=True,
                        help="Existing host libclang shared library for target-aware generation")
    args = parser.parse_args()
    source, work, toolchain = (p.resolve() for p in (args.source, args.work, args.toolchain))
    if source == work or source in work.parents or work in source.parents:
        parser.error("source and work must be disjoint directories")
    if work.exists() and (not work.is_dir() or any(work.iterdir())):
        parser.error("--work must be new or empty; old success receipts are never reused")
    work.mkdir(parents=True, exist_ok=True)
    receipt = work / "build-evidence.json"
    report = {"accepted": False, "errors": [], "upstream_commit": COMMIT,
              "source": str(source), "work": str(work), "toolchain": str(toolchain),
              "target": TARGET, "cpu": "cortex-a9", "simd": True,
              "runtime_verified": False, "commands": [], "tools": {},
              "recipe_sha256": sha256(Path(__file__))}
    env = os.environ.copy()
    # Preserve host loader settings (including NIX_LD), but not ambient build
    # overrides. Both target-qualified spellings are set explicitly below.
    compiler_vars = ("CC", "CXX", "AR", "RANLIB", "CFLAGS", "CXXFLAGS", "CPPFLAGS", "LDFLAGS")
    for name in list(env):
        if (name in compiler_vars or name.startswith(tuple(v + "_" for v in compiler_vars))
                or name.startswith(("HOST_", "TARGET_", "CARGO_TARGET_", "CARGO_BUILD_",
                                    "PKG_CONFIG", "LIBGHOSTTY_", "HERDR_BUILD_", "BINDGEN_"))
                or name in ("CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH",
                            "RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC", "RUSTDOC",
                            "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER", "ZIG", "RUSTFMT",
                            "LIBCLANG_PATH", "CLANG_PATH")):
            env.pop(name, None)
    env.update({"LC_ALL": "C", "CARGO_NET_OFFLINE": "true", "MISE_AUTO_INSTALL": "false",
                "CARGO_TARGET_DIR": str(work / "target"),
                "ZIG_GLOBAL_CACHE_DIR": str(work / "zig-global-cache")})

    def save():
        receipt.write_text(json.dumps(report, indent=2) + "\n")

    def run(command, label, cwd=work):
        command = [str(arg) for arg in command]
        log = work / f"{len(report['commands']):03d}-{label}.log"
        report["commands"].append({"argv": command, "cwd": str(cwd), "log": str(log)})
        save()
        progress("+ " + " ".join(command))
        with log.open("w") as output:
            result = subprocess.run(command, cwd=cwd, env=env, stdout=output,
                                    stderr=subprocess.STDOUT)
        text = log.read_text()
        report["commands"][-1]["returncode"] = result.returncode
        save()
        if result.returncode:
            progress(text, file=sys.stderr)
            raise RuntimeError(f"{label} failed ({result.returncode}); see {log}; no portability fallback applied")
        return text

    def inventory(checkout):
        names = run(["git", "ls-files", "-z"], "tracked-files", checkout).split("\0")
        result = {}
        for name in filter(None, names):
            path = checkout / name
            if path.is_symlink():
                result[name] = {"symlink": os.readlink(path)}
            elif path.is_file():
                result[name] = {"sha256": sha256(path), "executable": bool(path.stat().st_mode & 0o111)}
            else:
                raise RuntimeError(f"missing tracked file or unsupported submodule: {path}")
        return result

    try:
        save()
        head = run(["git", "rev-parse", "HEAD"], "source-head", source).strip()
        if head != COMMIT:
            raise RuntimeError(f"source HEAD must be {COMMIT}, got {head}")
        status = run(["git", "status", "--porcelain=v1", "--untracked-files=all", "--ignored"],
                     "source-status", source)
        if status.strip():
            raise RuntimeError("source contains tracked, untracked or ignored changes; use a clean review checkout")
        original = inventory(source)
        report["source_files"] = original
        # A local transport clone does not fetch from the network. --no-local
        # avoids sharing mutable object files with the supplied repository.
        checkout = work / "source"
        run(["git", "clone", "--no-local", "--no-checkout", source, checkout], "clone")
        run(["git", "checkout", "--detach", COMMIT], "checkout", checkout)
        if inventory(checkout) != original:
            raise RuntimeError("copied checkout differs from the supplied source")
        build = checkout / "build.rs"
        before = build.read_text()
        edits = [
            ('        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",',
             '        "aarch64-unknown-linux-gnu" => "aarch64-linux-gnu",\n'
             '        "armv7-unknown-linux-gnueabihf" => "arm-linux-gnueabihf.2.20",'),
            ('    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {',
             '    if target == "armv7-unknown-linux-gnueabihf" {\n'
             '        command.arg("-Dcpu=cortex_a9+neon");\n'
             '    }\n'
             '    if let Ok(system_dir) = env::var("LIBGHOSTTY_VT_ZIG_SYSTEM_DIR") {'),
        ]
        after = before
        for old, new in edits:
            if after.count(old) != 1:
                raise RuntimeError("upstream build.rs does not match the reviewed build-only patch")
            after = after.replace(old, new, 1)
        build.write_text(after)
        diff = "".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                           fromfile="a/build.rs", tofile="b/build.rs"))
        patch = work / "build-only.patch"
        patch.write_text(diff)
        report["patch"] = {"path": str(patch), "sha256": sha256(patch), "diff": diff}
        expected = dict(original)
        expected["build.rs"] = {"sha256": sha256(build),
                                "executable": bool(build.stat().st_mode & 0o111)}
        if inventory(checkout) != expected:
            raise RuntimeError("unexpected source changes while applying build-only patch")

        tools = {name: toolchain / "bin" / f"{TRIPLE}-{name}"
                 for name in ("gcc", "g++", "ar", "ranlib", "ld", "readelf")}
        # mise's Rust install directory can contain rustup shims rather than
        # bin/rustc. Resolve the selected compiler's actual installed sysroot.
        rust_home = Path(run(["mise", "exec", f"rust@{RUST_VERSION}", "--",
                              "rustc", "--print", "sysroot"], "rust-home").strip())
        zig_home = Path(run(["mise", "where", f"zig@{ZIG_VERSION}"], "zig-home").strip())
        tools.update({"rustc": rust_home / "bin/rustc", "cargo": rust_home / "bin/cargo",
                      "rustfmt": rust_home / "bin/rustfmt", "zig": zig_home / "zig",
                      "bindgen": args.bindgen.resolve()})
        if not tools["zig"].is_file():
            tools["zig"] = zig_home / "bin/zig"
        for name in ("cc", "c++", "ar"):
            native = shutil.which(name, path=env.get("PATH"))
            if not native:
                raise RuntimeError(f"missing native host compiler tool: {name}")
            tools[f"host-{name}"] = Path(native)
        for name, path in tools.items():
            if not path.is_file() or not os.access(path, os.X_OK):
                raise RuntimeError(f"missing already-installed tool: {path}")
            version_arg = "version" if name == "zig" else "--version"
            report["tools"][name] = {"path": str(path), "resolved": str(path.resolve()),
                                     "sha256": sha256(path),
                                     "version": run([path, version_arg], f"{name}-version")}
        rust_info = run([tools["rustc"], "-Vv"], "rustc-verbose")
        if not re.search(rf"^release: {re.escape(RUST_VERSION)}$", rust_info, re.M):
            raise RuntimeError(f"unexpected Rust compiler: {rust_info}")
        if report["tools"]["zig"]["version"].strip() != ZIG_VERSION:
            raise RuntimeError("unexpected Zig compiler version")
        if report["tools"]["bindgen"]["version"].strip() != f"bindgen {BINDGEN_VERSION}":
            raise RuntimeError("unexpected binding generator version")
        libclang = args.libclang.resolve()
        if not libclang.is_file():
            raise RuntimeError(f"missing host libclang shared library: {libclang}")
        report["tools"]["libclang"] = {"path": str(libclang), "sha256": sha256(libclang)}
        if run([tools["gcc"], "-dumpmachine"], "gcc-target").strip() != TRIPLE:
            raise RuntimeError("unexpected KOReader GCC target")
        sysroot = Path(run([tools["gcc"], "-print-sysroot"], "gcc-sysroot").strip())
        if not sysroot.is_absolute() or not sysroot.is_dir():
            raise RuntimeError(f"invalid GCC sysroot: {sysroot}")
        report["sysroot"] = str(sysroot.resolve())
        report["rustc_verbose"] = rust_info
        env["PATH"] = os.pathsep.join([str(rust_home / "bin"), env.get("PATH", "")])
        env["RUSTC"] = str(tools["rustc"])
        env["RUSTFMT"] = str(tools["rustfmt"])
        env["ZIG"] = str(tools["zig"])
        env["LIBCLANG_PATH"] = str(libclang.parent)
        env["LIBGHOSTTY_VT_SIMD"] = "true"
        env["LIBGHOSTTY_VT_OPTIMIZE"] = "ReleaseFast"
        zig_system_dir = args.zig_system_dir.resolve()
        if not zig_system_dir.is_dir():
            raise RuntimeError(f"missing prepared Zig package system directory: {zig_system_dir}")
        env["LIBGHOSTTY_VT_ZIG_SYSTEM_DIR"] = str(zig_system_dir)
        env["CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER"] = str(tools["gcc"])
        env.update({"HOST_CC": str(tools["host-cc"]), "HOST_CXX": str(tools["host-c++"]),
                    "HOST_AR": str(tools["host-ar"]), "HOST_CFLAGS": "", "HOST_CXXFLAGS": ""})
        target_env = {"CC": str(tools["gcc"]), "CXX": str(tools["g++"]),
                      "AR": str(tools["ar"]), "RANLIB": str(tools["ranlib"]),
                      "CFLAGS": " ".join(ARM_FLAGS), "CXXFLAGS": " ".join(ARM_FLAGS),
                      "CPPFLAGS": "", "LDFLAGS": "", "PKG_CONFIG_PATH": "",
                      "PKG_CONFIG_LIBDIR": os.pathsep.join(str(sysroot / p) for p in
                          ("usr/lib/pkgconfig", "usr/share/pkgconfig", "lib/pkgconfig")),
                      "PKG_CONFIG_SYSROOT_DIR": str(sysroot), "PKG_CONFIG_ALLOW_CROSS": "1"}
        for name, value in target_env.items():
            for suffix in (TARGET, TARGET.replace("-", "_")):
                env[f"{name}_{suffix}"] = value
        rust_flags = ["-C", "target-cpu=cortex-a9", "-C", "target-feature=+neon"]
        rust_flags.extend(f"-Clink-arg={flag}" for flag in ARM_FLAGS)
        rust_flags.append(f"-Clink-arg=-Wl,--dynamic-linker={INTERPRETER}")
        env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(rust_flags)
        report["build_environment"] = {name: value for name, value in env.items()
                                       if name not in os.environ or value != os.environ[name]}
        report["rust_flags"] = rust_flags
        # Existing Cargo caches are allowed, but ambient Cargo configuration can
        # change source replacements, rustflags or wrappers without changing Git.
        cargo_home = Path(env.get("CARGO_HOME", str(Path.home() / ".cargo")))
        config_dirs = {cargo_home, *(p / ".cargo" for p in (checkout, *checkout.parents))}
        for directory in config_dirs:
            for filename in ("config", "config.toml"):
                config = directory / filename
                if not config.exists():
                    continue
                if directory == checkout / ".cargo" and f".cargo/{filename}" in original:
                    report.setdefault("upstream_cargo_config", {})[filename] = config.read_text()
                    continue
                raise RuntimeError(f"unreviewed Cargo configuration: {config}")
        # Upstream checked in 64-bit bindgen layout assertions. Generate the
        # ARMv7 equivalents from its unchanged public C headers, retaining every
        # assertion and rejecting changes to any runtime declaration or code.
        header = checkout / "vendor/libghostty-vt/include/ghostty/vt.h"
        generated = work / "bindings-armv7.raw.rs"
        gcc_include = run([tools["gcc"], "-print-file-name=include"], "gcc-include").strip()
        run([tools["bindgen"], header, "--allowlist-type", "Ghostty.*",
             "--allowlist-function", "ghostty_.*", "--allowlist-var", "GHOSTTY_.*",
             "--with-derive-default", "--output", generated, "--", f"--target={TARGET}",
             f"--sysroot={sysroot}", *ARM_FLAGS, "-isystem", gcc_include,
             "-I", header.parent.parent], "generate-armv7-bindings", checkout)
        bindings = checkout / "src/ghostty/bindings.rs"
        before_bindings = bindings.read_text()
        after_bindings, layout_count, runtime_hash = target_layout_checks(
            before_bindings, generated.read_text())
        bindings.write_text(after_bindings)
        expected["src/ghostty/bindings.rs"] = {
            "sha256": sha256(bindings), "executable": bool(bindings.stat().st_mode & 0o111)}
        diff += "".join(difflib.unified_diff(
            before_bindings.splitlines(True), after_bindings.splitlines(True),
            fromfile="a/src/ghostty/bindings.rs", tofile="b/src/ghostty/bindings.rs"))
        patch.write_text(diff)
        report["patch"] = {"path": str(patch), "sha256": sha256(patch), "diff": diff}
        report["generated_abi_checks"] = {
            "count": layout_count, "raw_bindgen_sha256": sha256(generated),
            "runtime_bindings_sha256": runtime_hash, "runtime_bindings_unchanged": True}
        if inventory(checkout) != expected:
            raise RuntimeError("source changed outside the approved build and generated ABI checks")
        # No cache repair or tool installation: missing dependencies are an
        # explicit prerequisite failure, not permission to access the network.
        run([tools["cargo"], "build", "--frozen", "--offline", "--release",
             "--target", TARGET, "--bin", "herdr", "-vv"], "cargo-build", checkout)
        binary = work / "target" / TARGET / "release/herdr"
        report["binary"] = str(binary)
        report["binary_sha256"] = sha256(binary)
        report["binary_bytes"] = binary.stat().st_size
        elf = run([tools["readelf"], "-W", "-h", "-l", "-A", "-d", "-V", binary], "binary-elf")
        versions = sorted(set(re.findall(r"\bGLIBC_(\d+\.\d+(?:\.\d+)?)\b", elf)),
                          key=lambda v: tuple(map(int, v.split("."))))
        interpreters = re.findall(r"\[Requesting program interpreter:\s*([^\]]+)\]", elf)
        needed = re.findall(r"\(NEEDED\).*?\[([^\]]+)\]", elf)
        report.update({"glibc_versions": versions, "interpreter": interpreters, "needed": needed,
                       "binary_elf": elf})
        errors = report["errors"]
        if not versions or any(tuple(map(int, v.split("."))) + (0,) * (3 - len(v.split(".")))
                               > (2, 20, 0) for v in versions):
            errors.append(f"unacceptable GLIBC requirements: {versions}")
        if re.search(r"\bGLIBC_(?:ABI_\w+|PRIVATE)\b", elf):
            errors.append("unsupported nonnumeric GLIBC requirement")
        if (not re.search(r"Machine:\s+ARM\s*$", elf, re.M)
                or not re.search(r"Class:\s+ELF32\s*$", elf, re.M)
                or not re.search(r"Data:.*little endian", elf)
                or "hard-float ABI" not in elf or not re.search(r"Flags:.*Version5 EABI", elf)
                or not re.search(r"Tag_CPU_arch:\s+v7\s*$", elf, re.M)
                or not re.search(r"Tag_ABI_VFP_args:\s+VFP registers", elf)):
            errors.append("output is not ARMv7 little-endian EABI5 hard-float ELF")
        if interpreters != [INTERPRETER]:
            errors.append(f"unexpected ELF interpreter: {interpreters}")
        if inventory(checkout) != expected or inventory(source) != original:
            errors.append("tracked source changed beyond the approved build and generated ABI checks")
        untracked = run(["git", "ls-files", "--others", "--exclude-standard"],
                        "unexpected-untracked-source", checkout)
        if untracked.strip():
            errors.append(f"unexpected untracked source files: {untracked}")
        final_diff = run(["git", "diff", "--binary", "HEAD", "--"], "final-source-diff", checkout)
        report["final_git_diff"] = final_diff
        report["runtime_and_vendor_implementation_unchanged"] = inventory(checkout) == expected
        report["accepted"] = not errors
        report["acceptance_scope"] = "Build-only source integrity and binary ELF ABI; not device/runtime feasibility"
        save()
        if errors:
            raise RuntimeError("; ".join(errors))
        progress(f"Accepted build/ABI receipt: {receipt}")
    except BaseException as error:
        report["accepted"] = False
        report["errors"].append(f"{type(error).__name__}: {error}")
        if "expected" in locals():
            try:
                actual = inventory(checkout)
                report["runtime_and_vendor_implementation_unchanged"] = actual == expected
                report["failed_attempt_source_files"] = actual
                report["input_source_unchanged"] = inventory(source) == original
                if actual != expected or not report["input_source_unchanged"]:
                    report["errors"].append("source integrity failed after the rejected build attempt")
            except Exception as integrity_error:
                report["errors"].append(f"could not establish final source integrity: {integrity_error}")
        save()
        progress(f"REJECTED: {error}\nReceipt: {receipt}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
