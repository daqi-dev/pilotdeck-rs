#!/usr/bin/env python3
"""Real source install/update acceptance; no builder replacement.

Run on the build host with its existing cargo shim first on PATH.
--source must be a clean repository. It is never modified.
Only the exclusively created --out directory is written.
Registry source/archive caches are copied without Cargo configuration or keys.
The approved Git commit is imported from a supplied public local object store.
"""
import argparse
import hashlib
import json
import os
import re
import stat
from pathlib import Path
import shutil
import signal
import subprocess
import threading
import time

BINARIES = (
    "pilotdeck", "pilotdeck-control", "pilotdeck-gateway",
    "pilotdeck-agent-loop-sidecar",
)
REF = "refs/heads/native-source-acceptance"
COMMAND_SECONDS = 3900
LOG_LIMIT = 16 * 1024 * 1024


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def write_json(path, value):
    with path.open("x") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


def tree_bytes(root):
    """Fingerprint regular bytes, symlink text and modes; ignore access times."""
    rows = {}
    for path in sorted(root.rglob("*")):
        relative = str(path.relative_to(root))
        mode = path.lstat().st_mode
        if path.is_symlink():
            rows[relative] = [mode, "link", os.readlink(path)]
        elif path.is_file():
            rows[relative] = [mode, "file", digest(path)]
        elif path.is_dir():
            rows[relative] = [mode, "directory"]
        else:
            raise RuntimeError(f"Unexpected generation object: {path}")
    return rows


def generation_libraries(directory, manifest):
    """Independently verify installed native libraries, including legacy sources."""
    bins = directory / "bin"
    profile, hashes = manifest.get("libraryProfile"), manifest.get("libraryHashes", {})
    image = directory / "source/src/pilotdeck-image-native"
    require(not image.is_symlink(), "Linked source image runtime")
    needs_image = image.exists()
    if needs_image:
        require(image.is_dir() and stat.S_ISREG((image / "Cargo.toml").lstat().st_mode),
                "Invalid source image runtime")
    if not needs_image:
        require(profile is None and hashes == {}, "Unexpected legacy library inventory")
        require({p.name for p in bins.iterdir()} == set(BINARIES),
                "Unexpected legacy binary directory entries")
        return {}
    require(isinstance(profile, str) and re.fullmatch(r"pilotdeck-libs-[0-9a-f]{64}", profile),
            "Missing or invalid runtime profile")
    require(isinstance(hashes, dict) and 0 < len(hashes) <= 64,
            "Missing or oversized runtime library inventory")
    root = bins / profile
    require(stat.S_ISDIR(root.lstat().st_mode), "Runtime profile is not a regular directory")
    require({p.name for p in bins.iterdir()} == set(BINARIES) | {profile},
            "Unregistered binary directory entries")
    require({p.name for p in root.iterdir()} == set(hashes), "Runtime library set mismatch")
    actual = {}
    for name, expected in hashes.items():
        require(isinstance(name, str) and len(name) <= 200 and
                re.fullmatch(r"lib[A-Za-z0-9._+-]+", name) and
                (".so" in name or name.endswith(".dylib")), "Invalid library name")
        path = root / name
        metadata = path.lstat()
        require(stat.S_ISREG(metadata.st_mode) and 0 < metadata.st_size <= 512 * 1024 * 1024,
                "Invalid library file")
        require(isinstance(expected, str) and re.fullmatch(r"[0-9a-f]{64}", expected),
                "Invalid library hash")
        actual[name] = digest(path)
        require(actual[name] == expected, "Runtime library hash mismatch: " + name)
    return {"profile": profile, "sha256": actual}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("control", "source", "out", "cargo-shim", "regress-db", "registry-cache", "rustup-home"):
        parser.add_argument("--" + name, type=Path, required=True)
    for name in ("old-ref", "new-ref"):
        parser.add_argument("--" + name, required=True)
    args = parser.parse_args()
    control = args.control.resolve(strict=True)
    source = args.source.resolve(strict=True)
    out = args.out.absolute()
    require(not out.is_symlink(), "--out must not be a symlink")
    require(out.parent.is_dir() and not out.exists(), "out must be exclusively new")
    out.mkdir(mode=0o700, parents=False, exist_ok=False)
    out = out.resolve()
    os.umask(0o077)
    def overall_timeout(signum, frame):
        raise TimeoutError("driver overall deadline 3900 seconds exceeded")
    signal.signal(signal.SIGALRM, overall_timeout)
    signal.alarm(3900)
    write_json(out / "driver-pid.json", {"pid": os.getpid(), "deadlineSeconds": 3900})
    original_home = Path.home()
    home, temporary, cwd = (out / name for name in ("home", "tmp", "empty-cwd"))
    for path in (home, temporary, cwd):
        path.mkdir(mode=0o700)
    cargo_home = home / ".cargo"
    cargo_home.mkdir()
    registry = args.registry_cache.resolve(strict=True)
    # Never read credentials/config, or Git caches containing private remotes.
    if registry.is_dir():
        shutil.copytree(registry, cargo_home / "registry", symlinks=False)
    env = {
        "PATH": str(args.cargo_shim.resolve(strict=True).parent) + ":" + os.environ["PATH"],
        "HOME": str(home),
        "PILOT_HOME": str(home / "pilot"),
        "TMPDIR": str(temporary),
        "TMP": str(temporary),
        "TEMP": str(temporary),
        "CARGO_HOME": str(cargo_home),
        "RUSTUP_HOME": str(args.rustup_home.resolve(strict=True)),
        "CARGO_NET_OFFLINE": "true",
        "CARGO_INCREMENTAL": "0",
        "CARGO_TERM_COLOR": "never",
        "PDRS_CARGO_SLOT_N": "2",
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_TERMINAL_PROMPT": "0",
        "GIT_NO_REPLACE_OBJECTS": "1",
        "GIT_ALLOW_PROTOCOL": "file",
        "LANG": "en_US.UTF-8",
    }
    write_json(out / "executables.json", {
        "control": str(control), "controlSha256": digest(control),
        "cargo": shutil.which("cargo", path=env["PATH"]),
        "git": shutil.which("git", path=env["PATH"]),
        "scope": "install/update only; skills and default sidecar NOT_RUN",
    })
    counter = 0

    def process_table():
        rows = {}
        raw = subprocess.check_output(
            ["/bin/ps", "-axo", "pid=,ppid=,lstart=,comm="],
            text=True, timeout=5,
        )
        for line in raw.splitlines():
            fields = line.split(None, 7)
            if len(fields) == 8:
                rows[int(fields[0])] = (int(fields[1]), tuple(fields[2:7]), fields[7])
        return rows

    def descendants(root_pid, known):
        table = process_table()
        active = {pid for pid, identity in known.items()
                  if pid in table and table[pid][1] == identity[0]}
        while True:
            found = {pid for pid, row in table.items() if row[0] in active} - active
            additions = set()
            for pid in found:
                if pid not in known:
                    known[pid] = (table[pid][1], table[pid][2])
                if table[pid][1] == known[pid][0]:
                    additions.add(pid)
            if not additions:
                break
            active.update(additions)
        return table

    def stop_owned(root_pid, known):
        # Revalidate PID plus kernel start time before signaling; children may
        # use their own process groups or be reparented when the parent exits.
        for sig, grace in ((signal.SIGTERM, 15), (signal.SIGKILL, 10)):
            deadline = time.monotonic() + grace
            while True:
                table = descendants(root_pid, known)
                live = [pid for pid, identity in known.items()
                        if pid in table and table[pid][1] == identity[0]]
                if not live:
                    return []
                for pid in live:
                    try:
                        os.kill(pid, sig)
                    except ProcessLookupError:
                        pass
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.05)
        table = process_table()
        return [pid for pid, identity in known.items()
                if pid in table and table[pid][1] == identity[0]]

    def run(label, argv, directory=cwd, timeout=COMMAND_SECONDS):
        nonlocal counter
        counter += 1
        stem = out / f"{counter:02d}-{label}"
        write_json(stem.with_suffix(".command.json"), {
            "argv": [str(x) for x in argv], "cwd": str(directory),
            "timeoutSeconds": timeout,
        })
        streams = [stem.with_suffix(".stdout"), stem.with_suffix(".stderr")]
        start = time.monotonic()
        with streams[0].open("xb") as stdout, streams[1].open("xb") as stderr:
            child = subprocess.Popen(
                [str(x) for x in argv], cwd=directory, env=env,
                stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                start_new_session=True,
            )
            failure = None
            table = process_table()
            known = {}
            if child.pid in table:
                known[child.pid] = (table[child.pid][1], table[child.pid][2])
            write_json(stem.with_suffix(".pid.json"), {"pid": child.pid, "birth": list(table[child.pid][1]) if child.pid in table else None})
            try:
                while child.poll() is None:
                    descendants(child.pid, known)
                    if time.monotonic() - start > timeout:
                        raise RuntimeError(f"{label}: deadline exceeded")
                    if any(p.stat().st_size > LOG_LIMIT for p in streams):
                        raise RuntimeError(f"{label}: output limit exceeded")
                    time.sleep(0.05)
            except BaseException as error:
                failure = error
                if child.poll() is None:
                    child.send_signal(signal.SIGTERM)
                    try:
                        child.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=10)
            finally:
                remaining = stop_owned(child.pid, known)
                write_json(stem.with_suffix(".processes.json"), {
                    "samples": [{"pid": pid, "started": list(identity[0]),
                                 "executable": identity[1]}
                                for pid, identity in sorted(known.items())],
                    "remaining": remaining,
                    "meaning": "sampled descendants; not a syscall trace",
                })
                if remaining and failure is None:
                    failure = RuntimeError(f"{label}: owned processes survived: {remaining}")
                write_json(stem.with_suffix(".exit.json"), {
                    "returncode": child.returncode,
                    "elapsedSeconds": time.monotonic() - start,
                    "error": None if failure is None else str(failure),
                })
            if failure:
                raise failure
        require(child.returncode == 0, f"{label}: exit {child.returncode}")
        return streams[0].read_text().strip()

    def git(label, *arguments, directory=source):
        return run(label, ["git", "-C", directory, *arguments], timeout=60)

    root = out / "installation"
    observations = []
    stop = threading.Event()

    def observe():
        last = None
        while not stop.is_set():
            try:
                current = os.readlink(root / "current")
            except OSError as error:
                current = f"ERROR:{error}"
            targets = []
            state = (current, tuple(targets))
            if state != last:
                observations.append({
                    "monotonic": time.monotonic(),
                    "current": current, "buildTargets": targets,
                })
                last = state
            stop.wait(0.02)

    def generation(expected_sha):
        link = os.readlink(root / "current")
        parts = Path(link).parts
        require(len(parts) == 2 and parts[0] == "generations",
                f"Invalid current: {link}")
        directory = root / link
        manifest = json.loads((directory / "manifest.json").read_text())
        hashes = {name: digest(directory / "bin" / name) for name in BINARIES}
        require(manifest["sourceSha"] == expected_sha, "Wrong source SHA")
        require(manifest["generation"] == parts[1], "Wrong generation ID")
        require(manifest["sourceCheckout"] == str(directory / "source"),
                "Wrong manifest source path")
        require(manifest["binHashes"] == hashes, "Four-bin hash mismatch")
        for name in BINARIES:
            require(os.readlink(root / "bin" / name) == "../current/bin/" + name,
                    f"Wrong stable entrypoint: {name}")
            require(os.access(directory / "bin" / name, os.X_OK),
                    f"Not executable: {name}")
        libraries = generation_libraries(directory, manifest)
        return directory, {"link": link, "manifest": manifest, "sha256": hashes,
                           "libraries": libraries}

    monitor = None
    result = {"pass": False, "skills": "NOT_RUN", "defaultSidecar": "NOT_RUN"}
    try:
        require(shutil.which("cargo", path=env["PATH"]) == str(args.cargo_shim.resolve(strict=True)), "Wrong cargo shim")
        commit = "95c04041cd9b30249961eb80d950102c29c9586c"
        dependency = args.regress_db.resolve(strict=True)
        require(run("regress-source-commit", ["git", "--git-dir", dependency, "rev-parse", commit + "^{commit}"], timeout=60) == commit, "local dependency object identity")
        require(run("regress-source-url", ["git", "--git-dir", dependency, "config", "--get", "remote.origin.url"], timeout=60) == "https://github.com/ridiculousfish/regress", "local cache must identify exact public dependency")
        dep = cargo_home / "git/db/regress-fec4701c388136fd"
        dep.parent.mkdir(parents=True)
        run("regress-init", ["git", "init", "--bare", dep], timeout=60)
        commit = "95c04041cd9b30249961eb80d950102c29c9586c"
        run("regress-local-objects", ["git", "--git-dir", dep, "fetch", "--no-tags", args.regress_db.resolve(strict=True), commit], timeout=120)
        run("regress-public-url", ["git", "--git-dir", dep, "config", "remote.origin.url", "https://github.com/ridiculousfish/regress"], timeout=60)
        require(run("regress-verify", ["git", "--git-dir", dep, "rev-parse", "FETCH_HEAD"], timeout=60) == commit, "regress fixed commit mismatch")
        require(not git("source-clean", "status", "--porcelain",
                        "--untracked-files=all"), "Source is not clean")
        old = git("resolve-old", "rev-parse", "--verify", args.old_ref + "^{commit}")
        new = git("resolve-new", "rev-parse", "--verify", args.new_ref + "^{commit}")
        require(old != new, "Old and new must differ")
        git("ancestor", "merge-base", "--is-ancestor", old, new)
        remote, seed = out / "remote.git", out / "seed"
        run("local-remote", ["git", "clone", "--bare", "--no-hardlinks",
                             "--", source, remote], timeout=120)
        git("remote-old", "update-ref", REF, old, directory=remote)
        run("seed", ["git", "clone", "--no-hardlinks", "--", remote, seed],
            timeout=120)
        git("seed-old", "checkout", "--detach", old, directory=seed)
        run("install", [control, "install", "--root", root, "--source", seed,
                        "--remote", remote, "--ref", REF])
        old_dir, old_record = generation(old)
        write_json(out / "old.json", old_record)
        policy = (root / "install.json").read_bytes()
        git("remote-new", "update-ref", REF, new, old, directory=remote)
        # Update's fetch necessarily changes old source/.git metadata.
        # Snapshot payload bytes separately from Git administrative state.
        before = {k: v for k, v in tree_bytes(old_dir).items()
                  if k != "source/.git" and not k.startswith("source/.git/")}
        monitor = threading.Thread(target=observe, daemon=True)
        monitor.start()
        run("update", [root / "bin/pilotdeck", "update"])
        new_dir, new_record = generation(new)
        stop.set()
        monitor.join(timeout=2)
        require(new_dir != old_dir, "Current did not change generations")
        after = {k: v for k, v in tree_bytes(old_dir).items()
                 if k != "source/.git" and not k.startswith("source/.git/")}
        require(before == after, "Old generation payload changed")
        require((root / "install.json").read_bytes() == policy, "Policy changed")
        require(all(row["current"] in (old_record["link"], new_record["link"])
                    for row in observations), "Observed missing/invalid current")
        git("new-clean", "status", "--porcelain", "--untracked-files=all",
            directory=new_dir / "source")
        require(not git("verify-new-clean", "status", "--porcelain",
                        "--untracked-files=all", directory=new_dir / "source"),
                "New source is dirty")
        write_json(out / "new.json", new_record)
        write_json(out / "old-payload.json", before)
        result.update(pass_install_update=True, pass_=True,
                      old=old, new=new, atomicity="sampled, not syscall-traced")
        result["pass"] = True
    except Exception as error:
        result["error"] = str(error)
    finally:
        stop.set()
        if monitor:
            monitor.join(timeout=2)
        write_json(out / "observations.json", observations)
        write_json(out / "result.json", result)
    return 0 if result["pass"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
