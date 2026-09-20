#!/usr/bin/env python3
"""Build Kindle releases from pinned public inputs on x86-64 Linux."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib

HERDR = "b99002ac99b09e00b4ca692436cb15a6b0d676f1"
GHOSTTY = "c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3"
TOOLCHAIN_URL = "https://github.com/koreader/koxtoolchain/releases/download/2026.08/kindlehf.tar.zst"
TOOLCHAIN_SHA256 = "8cc7dfbd71abd78f9e947d6b2e20670288a4402edc7b07176bca791f7eaf87d0"
TARGET = "armv7-unknown-linux-gnueabihf"
TRIPLE = "arm-kindlehf-linux-gnueabihf"
PROJECT = Path(__file__).resolve().parent.parent


def run(args, *, cwd=PROJECT, env=None, capture=False):
    args = list(map(str, args))
    print("+ " + " ".join(args), flush=True)
    return subprocess.run(args, cwd=cwd, env=env, check=True, text=True,
                          stdout=subprocess.PIPE if capture else None).stdout


def download(url, destination):
    temporary = destination.with_suffix(destination.suffix + ".part")
    run(["curl", "--fail", "--location", "--retry", "3", "--output", temporary, url])
    temporary.replace(destination)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def ghostty_build(source, prefix, cache, env, *, native=False):
    """Fetch only selected Zig dependencies, verifying each with Zig's lock hash."""
    zig = ["mise", "exec", "zig@0.15.2", "--", "zig"]
    packages = cache / "p"
    packages.mkdir(parents=True, exist_ok=True)
    archives = cache / "archives"
    archives.mkdir(exist_ok=True)
    target = "x86_64-linux-gnu" if native else "arm-linux-gnueabihf.2.20"
    cpu = "baseline" if native else "cortex_a9"
    build = zig + ["build", "--system", str(packages), "-Demit-lib-vt=true",
                   "-Dversion-string=1.3.2-dev+c5a21edfc", f"-Dtarget={target}",
                   f"-Dcpu={cpu}", "-Doptimize=ReleaseSmall", "--prefix", str(prefix)]
    fetched = set()
    while True:
        result = subprocess.run(build, cwd=source, env=env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(result.stdout, flush=True)
        if result.returncode == 0:
            return
        missing = set(re.findall(r"(?:package not found at '|lazy dependency package not found: )"
                                 + re.escape(str(packages)) + r"/([^/'\s]+)", result.stdout))
        if not missing or missing & fetched:
            raise RuntimeError("Ghostty build failed; see compiler output above")
        records = {}
        for root in (source, packages):
            for manifest in root.rglob("build.zig.zon"):
                for url, expected in re.findall(r'\.url\s*=\s*"([^"]+)"\s*,\s*\.hash\s*=\s*"([^"]+)"', manifest.read_text()):
                    records.setdefault(expected, url)
        for expected in sorted(missing):
            url = records[expected]
            archive_url = re.fullmatch(r"https://github.com/([^/]+/[^/]+)/archive/(.+)\.tar\.gz", url)
            git_url = re.fullmatch(r"git\+https://github.com/([^/]+/[^#]+)#([0-9a-f]+)", url)
            if archive_url or git_url:
                match = archive_url or git_url
                url = f"https://codeload.github.com/{match[1]}/tar.gz/{match[2]}"
            suffix = next((s for s in (".tar.zst", ".tar.xz", ".tar.bz2", ".zip") if url.endswith(s)), ".tar.gz")
            archive = archives / (expected + suffix)
            download(url, archive)
            actual = run(zig + ["fetch", archive], cwd=source, env=env, capture=True).strip()
            if actual != expected:
                raise RuntimeError(f"Zig package hash mismatch: {expected}")
            fetched.add(expected)


def collect_notices(vendor, destination):
    for package in sorted(vendor.iterdir()):
        for source in package.rglob("*"):
            if source.is_file() and source.name.lower().startswith(("license", "licence", "copying", "notice", "copyright", "authors")):
                target = destination / package.name / source.relative_to(package)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, default=PROJECT / "target/release-work")
    parser.add_argument("--output", type=Path, default=PROJECT / "dist")
    parser.add_argument("--cache", type=Path, default=Path.home() / ".cache/kherdr-release")
    args = parser.parse_args()
    work, output, cache = (p.resolve() for p in (args.work, args.output, args.cache))
    for path in (work, output, cache):
        path.mkdir(parents=True, exist_ok=True)
    version = tomllib.loads((PROJECT / "Cargo.toml").read_text())["package"]["version"]
    ref = os.environ.get("GITHUB_REF", "")
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{version}":
        raise RuntimeError("Release tag must match Cargo.toml version")
    run(["git", "diff", "--exit-code", "HEAD", "--"])
    untracked = run(["git", "ls-files", "--others", "--exclude-standard"], capture=True)
    if untracked.strip():
        raise RuntimeError("Commit release inputs before building the source archive")
    run(["mise", "install", "rust@1.92.0", "zig@0.15.2"])
    rust = ["mise", "exec", "rust@1.92.0", "--"]
    run(rust + ["rustup", "target", "add", TARGET])
    run(rust + ["rustup", "component", "add", "rustfmt", "--toolchain", "1.92.0"])
    archive = cache / "kindlehf-2026.08.tar.zst"
    if not archive.exists() or digest(archive) != TOOLCHAIN_SHA256:
        download(TOOLCHAIN_URL, archive)
    if digest(archive) != TOOLCHAIN_SHA256:
        raise RuntimeError("KOReader toolchain archive differs from pinned release")
    toolchain_root = work / "toolchain"
    toolchain_root.mkdir(exist_ok=True)
    run(["tar", "--zstd", "-xf", archive, "-C", toolchain_root])
    toolchain = toolchain_root / "x-tools" / TRIPLE
    gcc = toolchain / "bin" / (TRIPLE + "-gcc")
    sysroot = Path(run([gcc, "-print-sysroot"], capture=True).strip())
    clang = shutil.which("clang")
    if not clang:
        raise RuntimeError("Install clang and libclang development libraries")
    resource = Path(run([clang, "-print-resource-dir"], capture=True).strip())
    candidates = []
    if os.environ.get("LIBCLANG_PATH"):
        candidates.extend(Path(os.environ["LIBCLANG_PATH"]).glob("libclang.so*"))
    candidates.extend(Path("/usr/lib").glob("llvm-*/lib/libclang.so"))
    candidates.extend(Path("/nix/store").glob("*clang*-lib/lib/libclang.so"))
    if not candidates:
        raise RuntimeError("libclang.so not found; set LIBCLANG_PATH")
    libclang = sorted(candidates)[-1].resolve()
    env = dict(os.environ, LIBCLANG_PATH=str(libclang.parent),
               CARGO_BUILD_JOBS=os.environ.get("CARGO_BUILD_JOBS", "4"),
               ZIG_GLOBAL_CACHE_DIR=str(cache / "zig"))
    bindgen = cache / "bindgen/bin/bindgen"
    if not bindgen.exists():
        run(rust + ["cargo", "install", "bindgen-cli", "--version", "0.72.1", "--locked",
                    "--root", cache / "bindgen"], env=env)
    # Each invocation builds in a fresh directory; only downloadable inputs are cached.
    attempt = Path(tempfile.mkdtemp(prefix="build-", dir=work))
    try:
        source = attempt / "herdr"
        run(["git", "init", source])
        run(["git", "-C", source, "remote", "add", "origin", "https://github.com/herdrdev/herdr.git"])
        run(["git", "-C", source, "fetch", "--depth=1", "origin", HERDR])
        run(["git", "-C", source, "checkout", "--detach", "FETCH_HEAD"])
        metadata = json.loads((source / "vendor/libghostty-vt.vendor.json").read_text())
        if metadata["source_commit"] != GHOSTTY:
            raise RuntimeError("Unexpected Ghostty source commit")
        ghostty = attempt / "ghostty"
        shutil.copytree(source / "vendor/libghostty-vt", ghostty)
        prefix = attempt / "ghostty-install"
        ghostty_build(ghostty, prefix, cache / "zig", env)
        native_prefix = attempt / "ghostty-native"
        ghostty_build(ghostty, native_prefix, cache / "zig", env, native=True)
        native_env = dict(env, GHOSTTY_SOURCE=str(ghostty), GHOSTTY_PREFIX=str(native_prefix),
                          CARGO_TARGET_DIR=str(attempt / "native-target"))
        run(rust + ["cargo", "test", "--locked", "--all-targets", "--features", "terminal"], env=native_env)
        run(rust + ["cargo", "fetch", "--locked", "--target", TARGET], cwd=source, env=env)
        server = attempt / "server"
        run([sys.executable, PROJECT / "tools/build-local-herdr.py", "--source", source,
             "--work", server, "--toolchain", toolchain, "--zig-system-dir", cache / "zig/p",
             "--bindgen", bindgen, "--libclang", libclang], env=env)
        client = attempt / "client"
        env.update(GHOSTTY_SOURCE=str(ghostty), GHOSTTY_PREFIX=str(prefix))
        env["BINDGEN_EXTRA_CLANG_ARGS_armv7_unknown_linux_gnueabihf"] = (
            f"--target={TARGET} --sysroot={sysroot} -nostdinc "
            f"-isystem {resource / 'include'} -isystem {sysroot / 'usr/include'}")
        run([sys.executable, PROJECT / "tools/build-rust.py", "--bin", "kherdr", "--features", "terminal",
             "--toolchain", toolchain, "--work", client, "--library-dir", prefix / "lib"], env=env)
        sources = attempt / f"kherdr-{version}-source"
        sources.mkdir()
        project_tar = attempt / "project.tar"
        run(["git", "archive", "--format=tar", "--output", project_tar, "HEAD"])
        with tarfile.open(project_tar) as tar:
            tar.extractall(sources, filter="data")
        upstream = sources / "upstream/herdr"
        shutil.copytree(source, upstream, ignore=shutil.ignore_patterns(".git"))
        shutil.copyfile(server / "build-only.patch", sources / "upstream/herdr-kindle.patch")
        shutil.copytree(cache / "zig/p", sources / "dependencies/zig")
        licenses = attempt / "licenses"
        for name, checkout in (("kherdr", PROJECT), ("herdr", source)):
            vendor = sources / "dependencies" / (name + "-cargo")
            run(rust + ["cargo", "vendor", "--locked", "--versioned-dirs", vendor], cwd=checkout, env=env, capture=True)
            notice = licenses / name
            (notice / "source").mkdir(parents=True)
            for filename in ("Cargo.toml", "Cargo.lock", "COPYING" if name == "kherdr" else "LICENSE"):
                shutil.copyfile(checkout / filename, notice / "source" / filename)
            collect_notices(vendor, notice / "dependencies")
            collect_notices(sources / "dependencies/zig", notice / "dependencies/zig")
            shutil.copytree(ghostty / "include", notice / "source/ghostty-headers")
            shutil.copyfile(ghostty / "LICENSE", notice / "source/GHOSTTY-LICENSE")
        package = output / f"kherdr-{version}.kpkg"
        run([sys.executable, PROJECT / "tools/verify-rust-package.py",
             "--binary", client / "target" / TARGET / "release/kherdr",
             "--build-evidence", client / "build-evidence.json", "--license-dir", licenses / "kherdr",
             "--local-herdr", server / "target" / TARGET / "release/herdr",
             "--local-herdr-evidence", server / "build-evidence.json",
             "--local-herdr-patch", server / "build-only.patch", "--local-herdr-license-dir", licenses / "herdr",
             "--output", package], env=env)
        source_archive = output / f"kherdr-{version}-source.tar.gz"
        with tarfile.open(source_archive, "w:gz") as tar:
            tar.add(sources, arcname=sources.name)
        print(f"Release ready: {package}\nCorresponding source: {source_archive}")
    except BaseException:
        print(f"Build logs and intermediate files retained at {attempt}", file=sys.stderr)
        raise
    else:
        shutil.rmtree(attempt)


if __name__ == "__main__":
    main()
