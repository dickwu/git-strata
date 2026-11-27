"""C1 release-binary harness, invoked by cargo test on a hosted macOS runner."""
from contextlib import contextmanager
import itertools
import json
import os
from pathlib import Path
import re
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import tomllib

REPO = Path(__file__).resolve().parents[2]
CONFIG = Path("Library/Application Support/git-strata")
CORPUS = REPO / "crates/strata-daemon/tests/fixtures/wire_contract.json"
SEQUENCE = itertools.count()


def run(args, **kwargs):
    result = subprocess.run(args, text=True, stdin=subprocess.DEVNULL,
                            capture_output=True, timeout=90, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args[0]} exited {result.returncode}: {result.stderr}")
    return result.stdout


def contained(root, *paths):
    for path in paths:
        if path.resolve() != path or not path.is_relative_to(root):
            raise ValueError(f"out-of-fixture path {path}")


def environment(home):
    return {"HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config"),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_ALLOW_PROTOCOL": "file", "GIT_TERMINAL_PROMPT": "0"}


def exchange(path, frame):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(30)
        sock.connect(str(path))
        sock.sendall(json.dumps(frame).encode() + b"\n")
        with sock.makefile("rb") as stream:
            reply = json.loads(stream.readline())
    assert reply["id"] == frame["id"], (frame, reply)
    return reply


def rpc(path, name, params, client=None):
    frame = {"id": str(next(SEQUENCE)), "method": name, "params": params}
    if client:
        frame["client"] = client
    return exchange(path, frame)


def data(reply):
    assert reply["ok"] is True and isinstance(reply["data"], dict), reply
    return reply["data"]


def error(reply, code, reason=None):
    assert reply["ok"] is False and reply["error"]["code"] == code, reply
    if reason:
        assert reply["error"]["detail"]["reason"] == reason, reply


@contextmanager
def volume(root, scratch):
    # The image is private to this build. The mounted data root and checkouts
    # live beneath the independently verified short fixture, on the same APFS.
    mount = root / "vol"
    mount.mkdir()
    with tempfile.TemporaryDirectory(dir=scratch) as tmp:
        image = Path(tmp) / "contract.sparseimage"
        run(["hdiutil", "create", "-size", "1g", "-type", "SPARSE",
             "-fs", "APFS", "-volname", "StrataContract", str(image)])
        run(["hdiutil", "attach", "-nobrowse", "-noautoopen", "-owners", "on", "-mountpoint", str(mount), str(image)])
        try:
            assert mount.stat().st_dev != root.stat().st_dev
            yield mount
        finally:
            detach(mount)


def detach(mount):
    # A process that exited moments ago (a worker, git or Spotlight) can keep
    # the volume busy briefly. Retry, then force: the image is disposable.
    for attempt in range(6):
        try:
            run(["hdiutil", "detach", str(mount)])
            return
        except RuntimeError as err:
            if "Resource busy" not in str(err):
                raise
            time.sleep(2 + attempt)
    run(["hdiutil", "detach", "-force", str(mount)])


def configure(root, vol):
    main = vol / "tool"
    origin = vol / "origin.git"
    home = root / "worker"
    front = root / "cli"
    for h in [home, front]:
        (h / CONFIG).mkdir(parents=True)
        (h / ".gitconfig").write_text("[user]\n name = Strata Contract\n email = test@example.invalid\n")
    env = environment(home)
    run(["git", "init", "-q", "--bare", "-b", "main", str(origin)], env=env)
    run(["git", "init", "-q", "-b", "main", str(main)], env=env)
    (main / "note.txt").write_text("base\n")
    for args in [["add", "note.txt"], ["commit", "-qm", "fixture"],
                 ["remote", "add", "origin", origin.as_uri()], ["push", "-q", "origin", "main"]]:
        run(["git", "-C", str(main), *args], env=env)
    sibling = vol / "front"
    run(["git", "clone", "-q", origin.as_uri(), str(sibling)], env=env)
    store = vol / "data"
    for h in [home, front]:
        cfg = h / CONFIG
        text = CORPUS.with_suffix(".toml").read_text().replace("$VOLUME", str(vol))
        (cfg / "config.toml").write_text(text)
        repos = tomllib.loads(text)["repo"]
        pins = {name: {"push": repo["publish"]["push"], "confirm": repo["publish"]["confirm"],
                       "deploy_on_push": False} for name, repo in repos.items()}
        (cfg / "pins.json").write_text(json.dumps({"repos": pins}))
        # A release worker predates WorkerFixture. Independently resolve the
        # production paths from the exact HOME and config passed to that child.
        cfg_data = tomllib.loads((cfg / "config.toml").read_text())
        data_root = Path(cfg_data["paths"]["root"])
        contained(root, h, cfg, cfg / "config.toml", cfg / "pins.json", cfg / "daemon.sock",
                  cfg / "daemon.lock", data_root, data_root / "store/LOCK",
                  *(Path(r["path"]) for r in cfg_data["repo"].values()), origin)
        assert data_root == store and not (cfg / "daemon.sock").exists()
    return home, front, store


@contextmanager
def worker(root, home, binary, is_fixture):
    cfg = home / CONFIG
    path = cfg / "daemon.sock"
    args = [binary, root, cfg] if is_fixture else [binary]
    with (root / "worker.log").open("w") as log:
        child = subprocess.Popen(args, cwd=home, env=environment(home),
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            end = time.monotonic() + 75
            while time.monotonic() < end and child.poll() is None:
                try:
                    reply = rpc(path, "daemon_status", {"verify": False})
                    if reply["ok"]:
                        status = data(reply)
                        assert status["pid"] == child.pid, status
                        break
                    assert reply["error"]["detail"]["reason"] in ("starting", "store_unavailable"), reply
                except (FileNotFoundError, ConnectionRefusedError):
                    pass
                time.sleep(.05)
            else:
                raise RuntimeError("fixture failed to serve: " + (root / "worker.log").read_text()[-2000:])
            yield status
        finally:
            if child.poll() is None:
                child.terminate()
            child.wait(timeout=45)


class Proxy:
    """Separate CLI HOME lets us observe dispatch and simulate a probe/revert race."""
    def __init__(self, root, home, backend, legacy=False):
        self.front = home / CONFIG / "daemon.sock"
        contained(root, self.front, backend)
        self.backend = backend
        self.requests = []
        self.plans = []
        self.legacy = legacy
        self.advertise = False
        self.checkpoint_refusal = False
        self.refusal = None
        self.failure = None
        self.stop = threading.Event()

    def __enter__(self):
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(self.front))
        self.listener.listen()
        self.listener.settimeout(.1)
        self.thread = threading.Thread(target=self.serve)
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stop.set()
        self.thread.join(timeout=35)
        self.listener.close()
        self.front.unlink()
        assert not self.thread.is_alive()
        if self.failure:
            raise self.failure

    def serve(self):
        try:
            while not self.stop.is_set():
                try:
                    peer, _ = self.listener.accept()
                except TimeoutError:
                    continue
                with peer, peer.makefile("rb") as stream:
                    line = stream.readline()
                    if not line:  # Readiness checks can connect without sending a frame.
                        continue
                    frame = json.loads(line)
                    self.requests.append(frame)
                    method = frame["method"]
                    # A regression in the CLI's dry-run/gate cannot push.
                    assert method != "publish", "preview sent publish"
                    safe = None
                    if self.refusal is not None and method == self.refusal[0]:
                        safe = self.refusal[1]
                        self.refusal = None
                    if method == "checkpoint" and self.checkpoint_refusal and frame["params"]["repo"] == "tool":
                        safe, self.checkpoint_refusal = True, False
                    if self.legacy and "client" in frame:
                        result = {"id": frame["id"], "ok": False, "error": {
                            "rpc": "Failed", "message": 'unknown envelope field "client"'}}
                    elif safe is not None:
                        result = {"id": frame["id"], "ok": False, "error": {
                            "code": "Busy", "message": "fixture refused before dispatch",
                            "detail": {"reason": "upgrading", "retry_safe": safe,
                                       "retry_after_ms": 2000, "handover_id": "c1", "phase": "park"}}}
                    else:
                        result = exchange(self.backend, frame)
                    if method == "publish_plan":
                        self.plans.append(result)
                    if method == "daemon_status" and result["ok"]:
                        features = result["data"]["features"]
                        if self.legacy and "envelope.client" in features:
                            features.remove("envelope.client")
                        if self.advertise:
                            features.append("envelope.client")
                    peer.sendall(json.dumps(result).encode() + b"\n")
        except Exception as exc:
            self.failure = exc
            self.listener.close()


def cli(binary, home, args, cwd=None):
    result = subprocess.run([binary, *args], cwd=cwd or home, env=environment(home),
                            stdin=subprocess.DEVNULL, text=True, capture_output=True, timeout=90)
    lines = result.stdout.splitlines()
    assert len(lines) == 1, (args, result.stderr, lines)
    reply = json.loads(lines[0])
    assert set(reply) == {"ok", "data", "error"} and reply["ok"] == (result.returncode == 0), (args, reply)
    return reply


def corpus(sock, session, other, checkpoint, client):
    cases = json.loads(CORPUS.read_text().replace("s-abcdefgh", session).replace("s-bcdefghi", other))
    cases["publish_plan"]["group"]["batches"] = [
        {"session": session, "repo": "tool", "checkpoint": checkpoint, "paths": ["note.txt"]}]
    cases["status"].update(cwd=None, session=session)
    cases["sync"].update(cwd=None, session=session)
    # Use valid frames and assert handler results, including deterministic
    # refusals for operations that need a pending proposal, conflict or human.
    reads = {"daemon_status": "features", "status": "sessions", "grep": "hits",
             "files": "paths", "diff": "both", "deps_list": "proposals",
             "publish_plan": "repos", "du": "total", "sync": "upstream"}
    for name, field in reads.items():
        result = data(rpc(sock, name, cases.pop(name), client))
        assert field in result, (name, result)
        if name == "publish_plan":
            assert result[field] == ["tool"], result
    def call(name, **overrides):
        params = cases.pop(name)
        params.update(overrides)
        return rpc(sock, name, params, client)

    disposable = data(call("new", ticket="corpus", repos=["tool"], only=True))["session"]["manifest"]["id"]
    assert "front" in data(call("add", session=disposable))["report"]["added"]
    assert data(call("mark", session=disposable))["mark"]["state"] == "done"
    assert data(call("checkpoint"))["checkpoint"]["session"] == session
    error(call("restore", checkpoint=2**40), "NoSuchCheckpoint")
    error(call("deps_propose", session="s-aaaaaaaa"), "NoSuchSession")
    error(call("deps_approve", proposal=2**40), "NoSuchProposal")
    error(call("publish"), "InvalidInput")  # Empty group; no ref can be updated.
    error(call("push", repo="unconfigured"), "NoSuchRepo")
    error(call("assign", group=2**40), "NoSuchPath")
    error(call("resolve_move", session="s-aaaaaaaa"), "NoSuchSession")
    error(call("chain_resolve", repo="unconfigured"), "NoSuchRepo")
    error(call("config_relax"), "InvalidInput", "shown_required")
    assert data(call("config_reload"))["outcome"] in ("applied", "unchanged")
    assert data(call("gc", repo=None, store=True, dry_run=True))["dry_run"] is True
    base = data(call("ports_ensure"))["base"]
    assert data(call("ports_reassign", checked=base))["base"] != base
    data(call("rm", session=disposable))
    lease = data(call("drain"))["lease_id"]
    try:
        error(rpc(sock, "checkpoint", {"session":session, "repo":"tool", "paths":[],
              "reason":{"kind":"Manual", "message":"leased"}}, client), "Busy", "draining")
    finally:
        data(rpc(sock, "drain", {"on": False, "lease_id": lease}, client))
    assert not cases, f"RPCs without a behavioral check: {sorted(cases)}"


def published_window(floor, published, reachable):
    """Select stable releases reachable from the candidate's parent."""
    def version(v):
        return tuple(map(int, v.split(".")))

    releases = {tag[1:] for tag in set(published) & set(reachable)
                if re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", tag) and version(tag[1:]) >= version(floor)}
    oldest = "0.3.1" if floor == "0.3.0" else floor
    assert oldest in releases, f"missing published floor release v{oldest}"
    previous = max(releases, key=version)
    return [{"version": v, "source": "tag" if v == "0.3.0" else "tarball"}
            for v in sorted({floor, oldest, previous}, key=version)]


def publish_preview(binary, front, proxy, source, current, version):
    start, plan_start = len(proxy.requests), len(proxy.plans)
    preview = cli(binary, front, ["publish", "--dry-run", "--from", source])
    if preview["ok"]:
        assert data(preview)["repos"] == ["tool"] and "plan_error" not in data(preview), preview
    else:
        # These three historical CLIs predate the flag. Their non-TTY gate
        # follows the real plan and stops before any publish request.
        assert not current and version in {(0, 3, 0), (0, 3, 1), (0, 3, 2)}, preview
        error(preview, "InvalidInput", "args")
        assert "--dry-run" in preview["error"]["message"] and not proxy.requests[start:], preview
        refusal = cli(binary, front, ["publish", "--from", source])
        error(refusal, "NotTty")
        assert "plan_error" not in refusal["error"].get("detail", {}), refusal
    ops = [q["method"] for q in proxy.requests[start:] if q["method"] != "daemon_status"]
    assert ops == ["checkpoint", "publish_plan"], ops
    plans = proxy.plans[plan_start:]
    assert len(plans) == 1 and data(plans[0])["repos"] == ["tool"], plans
    assert "plan_error" not in data(plans[0]), plans
    if preview["ok"]:
        assert all(data(preview).get(k) == v for k, v in data(plans[0]).items()), (preview, plans)


def commands(binary, front, proxy, current_client, release):
    def new(ticket):
        return cli(binary, front, ["new", ticket, "--repo", "tool"])

    release_number = tuple(map(int, release.split(".")))
    gs2 = current_client or release_number >= (0, 3, 2)
    upgrade_retry = current_client or release_number > (0, 3, 2)

    def check_gs2(method, invoke):
        for safe in [False, True]:
            proxy.refusal = (method, safe)
            before = len(proxy.requests)
            result = invoke()
            frames = [q for q in proxy.requests[before:] if q["method"] == method]
            if safe and gs2:
                assert len(frames) == 2 and frames[0]["params"] == frames[1]["params"], frames
                data(result)
            else:
                error(result, "Busy", "upgrading")
                assert len(frames) == 1, frames

    check_gs2("new", lambda: new("retry-new"))
    manifest = data(cli(binary, front, ["new", "contract", "--repo", "front,tool"]))["manifest"]
    sid = manifest["id"]
    other = data(new("comparison"))["manifest"]["id"]
    check_gs2("add", lambda: cli(binary, front, ["add", "--session", other, "--repo", "front"]))
    ws = Path(manifest["repos"]["tool"]["path"])
    contained(front.parent, ws)
    (ws / "note.txt").write_text("C1 content\n")
    sibling = Path(manifest["repos"]["front"]["path"])
    contained(front.parent, sibling)
    (sibling / "note.txt").write_text("sibling content\n")
    proxy.checkpoint_refusal = True
    before = len(proxy.requests)
    result = cli(binary, front, ["checkpoint", "--session", sid, "-m", "C1"])
    frames = [q["params"] for q in proxy.requests[before:] if q["method"] == "checkpoint"]
    if upgrade_retry:
        assert [p["repo"] for p in frames] == ["front", "tool", "tool"] and frames[1] == frames[2], frames
    else:
        error(result, "Busy", "upgrading")
        assert [p["repo"] for p in frames] == ["front", "tool"], frames
        assert [c["repo"] for c in result["data"]["checkpoints"]] == ["front"], result
        result = cli(binary, front, ["checkpoint", "-m", "C1", "--path", str(ws / "note.txt")], ws)
    c = next(c for c in data(result)["checkpoints"] if c["repo"] == "tool")
    assert c["session"] == sid, c
    status = data(cli(binary, front, ["status", "--session", sid]))
    assert status["manifest"]["id"] == sid, status
    source = f"{sid}:tool:note.txt"
    publish_preview(binary, front, proxy, source, current_client, release_number)
    return sid, other, c["id"]


def migrate_and_check(store, home, sid, old, new, schema, release):
    # Both workers have exited. This is an actual SQLite version transaction
    # on the isolated store, followed by a WAL checkpoint for header readers.
    next_schema = schema + 1
    with sqlite3.connect(store / "store/store.db") as db:
        assert db.execute("PRAGMA user_version").fetchone()[0] == schema
        db.execute("BEGIN IMMEDIATE")
        db.execute("CREATE TABLE c1_fixture_migration (id INTEGER PRIMARY KEY)")
        db.execute(f"PRAGMA user_version = {next_schema}")
        db.commit()
        assert db.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] == 0
    db.close()
    (home / CONFIG / "store_state.json").write_text(json.dumps({
        "root": str(store), "user_version": next_schema, "daemon_version": "1.2.3"}))
    args = ["checkpoint", "--session", sid, "-m", "post-migration"]
    if old is not None:
        if tuple(map(int, release.split("."))) <= (0, 3, 2):
            error(cli(old, home, args), "Corrupt", "user_version")
        else:
            error(cli(old, home, args), "Unsupported", "cli_too_old")
    reply = cli(new, home, args)
    error(reply, "Unsupported", "cli_too_old")
    assert reply["error"]["detail"]["required_version"] == "1.2.3", reply


def pair(scratch, cli_new, cli_old, daemon, current_worker, release, candidate, local=False):
    with tempfile.TemporaryDirectory(prefix="gsfx-", dir="/tmp") as tmp:
        root = Path(tmp).resolve()
        assert root.parent == Path("/tmp").resolve() and root.stat().st_uid == os.geteuid()
        with volume(root, scratch) as vol:
            home, front, store = configure(root, vol)
            backend = home / CONFIG / "daemon.sock"
            with worker(root, home, daemon, current_worker) as info, Proxy(root, front, backend, legacy=local) as proxy:
                expected = candidate if current_worker else release
                assert info["version"] == expected, info
                binary = cli_old if current_worker else cli_new
                version = release if current_worker else candidate
                assert run([binary, "--version"], env=environment(front)).strip().split()[-1] == version
                sid, other, checkpoint = commands(binary, front, proxy, local or not current_worker, release)
                # Raw corpus models a released client's unannotated wire and
                # a new client's capability-driven envelope in each pairing.
                announced = version if (local or not current_worker) and "envelope.client" in info["features"] else None
                corpus(backend, sid, other, checkpoint, announced)
                if current_worker:
                    assert "envelope.client" in info["features"]
                    data(rpc(backend, "daemon_status", {}, release))
                    error(rpc(backend, "daemon_status", {}, "0.0.1"), "InvalidInput", "client_too_old")
                if local or (not current_worker and "envelope.client" not in info["features"]):
                    # Only the probe reply is altered: the actual old release
                    # generates the parser refusal and handles the bare resend.
                    proxy.advertise = True
                    before = len(proxy.requests)
                    data(cli(cli_new, front, ["du"]))
                    frames = proxy.requests[before:]
                    assert [q["method"] for q in frames] == ["daemon_status", "du", "du"], frames
                    assert frames[1]["client"] == candidate and "client" not in frames[2]
                    assert frames[1]["params"] == frames[2]["params"]
            if current_worker:
                assert f"RPC client {release}" in (root / "worker.log").read_text()
            with Proxy(root, front, backend) as witness:
                migrate_and_check(store, front, sid, None if local else cli_old, cli_new, info["store_schema"], release)
                assert not witness.requests, "migration failure must precede the first RPC"
            print(f"C1 passed: CLI {version}, daemon {expected}, all RPCs, commands and migration", flush=True)


def main():
    if sys.argv[1] == "--matrix":
        _, _, floor, published, reachable = sys.argv
        print(json.dumps(published_window(floor, Path(published).read_text().splitlines(),
                                          Path(reachable).read_text().splitlines())))
        return
    candidate = tomllib.loads((REPO / "crates/git-strata/Cargo.toml").read_text())["package"]["version"]
    cli_new = Path(sys.argv[1]).resolve(strict=True)
    if sys.argv[2] == "--fixture-only":
        build = Path(os.environ.get("CARGO_TARGET_DIR", cli_new.parent.parent)).resolve(strict=True)
        fixture = build / "debug/git-stratad-test"
        contained(build, fixture, cli_new)
        pair(build, cli_new, cli_new, fixture, True, candidate, candidate, local=True)
        return
    if sys.platform != "darwin" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("release matrix requires a GitHub-hosted macOS runner")
    release = os.environ["C1_RELEASE"]
    floor = sys.argv[2]
    assert re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", release)
    assert tuple(map(int, release.split("."))) >= tuple(map(int, floor.split(".")))
    scratch = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    build = REPO / "target/debug"
    cli_old = scratch / "c1-release" / f"git-strata-{release}-aarch64-apple-darwin/bin/git-strata"
    old_daemon = cli_old.with_name("git-stratad")
    new_worker = build / "git-stratad-test"
    contained(build, cli_new, new_worker)
    contained(scratch, cli_old, old_daemon)
    for binary in [cli_new, cli_old, old_daemon, new_worker]:
        assert binary.is_file() and os.access(binary, os.X_OK), binary
    pair(scratch, cli_new, cli_old, new_worker, True, release, candidate)
    pair(scratch, cli_new, cli_old, old_daemon, False, release, candidate)


if __name__ == "__main__":
    main()
