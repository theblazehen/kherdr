#!/usr/bin/env python3
"""Build and render actual kherdr UI states inside Crabbox. Does not contact Paper or a Kindle.

Run through `mise run paper-export --work /workspace/kherdr-paper --fonts /path/to/private/fonts`.
Copy the resulting scenes directory back with Crabbox before releasing the lease.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--work", type=Path, required=True)
    parser.add_argument(
        "--fonts",
        type=Path,
        required=True,
        help="Private directory containing the eight original Kindle fonts; never committed",
    )
    parser.add_argument(
        "--state", help="Export only one of the states listed by the converter"
    )
    args = parser.parse_args()
    project = Path(__file__).resolve().parents[2]
    work = args.work.resolve()
    if not work.is_relative_to("/workspace"):
        parser.error("--work must be under the Crabbox worker /workspace mount")
    fonts = args.fonts.resolve()
    if not fonts.is_dir():
        parser.error("--fonts must name the private Kindle font directory")
    zig = shutil.which("zig")
    if not zig:
        parser.error("Zig is required; run with the project paper-export mise task")
    tools = work / "bin"
    tools.mkdir(parents=True, exist_ok=True)
    for name, command in [("cc", "cc"), ("ar", "ar")]:
        path = tools / name
        path.write_text(f'#!/bin/sh\nexec {shlex.quote(zig)} {command} "$@"\n')
        path.chmod(0o755)
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=str(work / "target"),
        CARGO_BUILD_JOBS="2",
        CC=str(tools / "cc"),
        AR=str(tools / "ar"),
        CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=str(tools / "cc"),
        ZIG_GLOBAL_CACHE_DIR=str(work / "zig-cache"),
        SLINT_EMIT_DEBUG_INFO="1",
        KHERDR_FONT_DIR=str(fonts),
    )
    sources = [
        project / name
        for name in ("src/ui.rs", "src/fonts.rs", "src/input.rs", "src/keyboard.rs")
    ]
    sources += sorted((project / "src/ui").glob("*.slint"))
    sources += sorted((project / "src/ui/icons").glob("*.svg"))
    sources += sorted((project / "tools/slint-paper/src").glob("*.rs"))
    sources += [
        project / "tools/slint-paper" / name
        for name in ("Cargo.toml", "Cargo.lock", "import.mjs")
    ]
    digests = {
        str(path.relative_to(project)): hashlib.sha256(path.read_bytes()).hexdigest()
        for path in sources
    }
    output = work / "scenes"
    output.mkdir(exist_ok=True)
    provenance = output / "provenance.json"
    provenance.unlink(missing_ok=True)
    subprocess.run(
        [
            "cargo",
            "build",
            "--locked",
            "--manifest-path",
            "tools/slint-paper/Cargo.toml",
        ],
        cwd=project,
        env=env,
        check=True,
    )
    command = [str(work / "target/debug/kherdr-slint-paper"), str(output)]
    if args.state:
        command.append(args.state)
    subprocess.run(command, cwd=project, env=env, check=True)
    manifest = json.loads((output / "manifest.json").read_text())
    unsupported = {
        item["state"]: item["unsupported"] for item in manifest if item["unsupported"]
    }
    if unsupported:
        raise RuntimeError(f"Unsupported scene constructs: {json.dumps(unsupported)}")
    for path in sources:
        if (
            hashlib.sha256(path.read_bytes()).hexdigest()
            != digests[str(path.relative_to(project))]
        ):
            raise RuntimeError(f"Source changed during export: {path}")
    provenance.write_text(
        json.dumps(
            {
                "schema": 1,
                "slint": "1.17.1",
                "source_sha256": digests,
                "font_sha256": {
                    path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in sorted(fonts.glob("*.ttf"))
                },
                "sample_data": "Synthetic; no live sessions or credentials",
                "surface": "1236×1547 application area; Amazon firmware bar is external",
                "states": [item["state"] for item in manifest],
            },
            indent=2,
        )
        + "\n"
    )
    print(
        f"Exported {len(manifest)} states with source and private-font provenance: {output}"
    )


if __name__ == "__main__":
    main()
