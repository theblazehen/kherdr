#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Read-only Herdr discovery and explicit session start over SSH."""
import errno
import json
import os
from pathlib import Path
import platform
import re
import selectors
import signal
import shutil
import socket
import subprocess
import sys
import threading
import time

LIMIT = 65536


def command(argv):
    env = os.environ.copy()
    env.pop("HERDR_SESSION", None)
    child = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             stdin=subprocess.DEVNULL, env=env)
    output = bytearray()
    selector = selectors.DefaultSelector()
    selector.register(child.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + 10
    try:
        while selector.get_map():
            if time.monotonic() >= deadline:
                raise RuntimeError("Herdr command timed out")
            for key, _ in selector.select(.1):
                data = os.read(key.fileobj.fileno(), 4096)
                if not data:
                    selector.unregister(key.fileobj)
                    continue
                output.extend(data)
                if len(output) > LIMIT:
                    raise RuntimeError("Herdr command output exceeds 64 KiB")
        child.wait(timeout=max(.1, deadline - time.monotonic()))
        if child.returncode:
            raise RuntimeError(output.decode(errors="replace").strip() or "Herdr command failed")
        return output.decode("utf-8").strip()
    finally:
        selector.close()
        if child.poll() is None:
            child.kill()
        child.wait()
        child.stdout.close()


def resolve(raw):
    expanded = os.path.expanduser(raw)
    if "/" in expanded:
        candidate = Path(expanded).absolute()
        return str(candidate) if candidate.is_file() and os.access(candidate, os.X_OK) else ""
    found = shutil.which(expanded)
    if not found and raw == "herdr":
        local = Path.home() / ".local/bin/herdr"
        if local.is_file() and os.access(local, os.X_OK):
            found = str(local)
    return os.path.abspath(found) if found else ""


def running(path):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(1)
        try:
            sock.connect(str(path))
            return True
        except OSError as error:
            if error.errno in (errno.ENOENT, errno.ECONNREFUSED):
                return False
            raise RuntimeError("Cannot establish server absence at %s: %s" % (path, error))


def session_directory(name):
    # Stable release config directory; custom config file does not relocate it.
    root = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))) / "herdr"
    return root if name == "default" else root / "sessions" / name


def inspect(raw, name, discover=True):
    binary = resolve(raw)
    version = command([binary, "--version"]) if binary else ""
    selected = session_directory(name)
    active = running(selected / "herdr.sock") or running(selected / "herdr-client.sock")
    compatible = version == "herdr 0.9.0"
    sessions = []
    if discover and compatible:
        listing = json.loads(command([binary, "session", "list", "--json"]))
        entries = listing.get("sessions") if isinstance(listing, dict) else None
        if not isinstance(entries, list) or len(entries) > 256:
            raise RuntimeError("Herdr session discovery requires an array of at most 256 sessions")
        for entry in entries:
            session = entry.get("name") if isinstance(entry, dict) else None
            if not isinstance(session, str) or not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", session) or session in (".", ".."):
                raise RuntimeError("Herdr session discovery returned an invalid session name")
            if type(entry.get("running")) is not bool:
                raise RuntimeError("Herdr session discovery returned an invalid running state")
            if entry["running"]:
                sessions.append(session)
        sessions = sorted(set(sessions))
    detail = ("Found %s at %s." % (version, binary)) if binary else "Herdr executable not found. Install Herdr on the host or open an SSH terminal."
    if binary and not compatible:
        detail += " This executable is incompatible; no running server will be upgraded or restarted."
    detail += " Selected session '%s' is %s." % (name, "running" if active else "absent")
    if discover and compatible:
        detail += " Found %d running Herdr session%s." % (len(sessions), "" if len(sessions) == 1 else "s")
    return dict(event="inspected", binary=binary, version=version, session_running=active, sessions=sessions,
                start_allowed=compatible and not active,
                detail=detail)




def start(raw, name):
    state = inspect(raw, name, discover=False)
    if not state["start_allowed"]:
        raise RuntimeError("Start refused: " + state["detail"])
    directory = session_directory(name)
    env = os.environ.copy()
    # Explicit session takes precedence over these inherited socket overrides.
    env.pop("HERDR_SOCKET_PATH", None)
    env.pop("HERDR_CLIENT_SOCKET_PATH", None)
    with open(os.devnull, "wb") as log:
        child = subprocess.Popen([state["binary"], "--session", name, "server"],
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                                 start_new_session=True, close_fds=True, env=env)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if child.poll() is not None:
                raise RuntimeError("Server did not start (exit %s); inspect %s/herdr-server.log" % (child.returncode, directory))
            if running(directory / "herdr.sock") and running(directory / "herdr-client.sock"):
                return dict(event="completed", binary=state["binary"], detail="Session '%s' is running. Existing sessions were not stopped or restarted." % name)
            time.sleep(.1)
        # Do not kill a server that may already own restored agents.
        raise RuntimeError("Server startup not confirmed within 20 seconds; it may still finish. Inspect before retrying. No server was killed.")


def main():
    # The native transport retains stdin until cancellation. Losing its owner
    # cancels inspection; a deliberately detached server is not killed.
    def cancelled(signum, frame):
        raise RuntimeError("SSH setup owner disconnected; inspect before retrying")
    def watch_owner():
        while os.read(0, 1):
            pass
        os.kill(os.getpid(), signal.SIGTERM)
    signal.signal(signal.SIGTERM, cancelled)
    threading.Thread(target=watch_owner, daemon=True).start()
    action, raw, name = sys.argv[1:]
    if not os.environ.get("HOME") or not Path(os.environ["HOME"]).is_absolute():
        raise RuntimeError("SSH HOME must be an absolute user home for safe setup")
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", name) or name in (".", ".."):
        raise RuntimeError("Invalid Herdr session name")
    if platform.system() != "Linux":
        raise RuntimeError("Automatic discovery/setup currently requires Linux and Python 3")
    operation = {"inspect": inspect, "start": start}.get(action)
    if operation is None:
        raise RuntimeError("Unknown setup action")
    print(json.dumps(operation(raw, name)), flush=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps(dict(event="failed", message=str(error)[:8192])), flush=True)
        sys.exit(1)
