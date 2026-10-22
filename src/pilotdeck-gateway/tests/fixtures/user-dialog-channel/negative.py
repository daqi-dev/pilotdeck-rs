#!/usr/bin/env python3
"""Mutation controls for the user-dialog journal/channel oracle.

Each mutant is applied only to a throwaway /tmp copy of the module. A control
passes when the real Rust replay fails an oracle assertion (exit 101), never a
compile error, and the failure names the intended branch.
"""
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
src_root = fixture.parents[3]
gateway = src_root / "pilotdeck-gateway"
module = gateway / "src/user_dialog"
baseline_helper = gateway / "tests/common/baseline.rs"
oracle_source = fixture / "oracle.json"

originals = {p.name: p.read_text() for p in module.glob("*.rs")}
originals.pop("tests.rs", None)
originals["mod.rs"] = originals["mod.rs"].replace("#[cfg(test)]\nmod tests;\n", "")
assert "mod tests;" not in originals["mod.rs"]
originals["oracle.json"] = oracle_source.read_text()

mutants = [
    (
        "journal-recover-omits-request",
        "journal.rs",
        '                event.insert("request".into(), request);',
        "                let _request = request;",
        "store-put-fails",
    ),
    (
        "journal-recover-wrong-reason",
        "journal.rs",
        'event.insert("reason".into(), "gateway_restarted".into());',
        'event.insert("reason".into(), "restarted".into());',
        "store-put-fails",
    ),
    (
        "journal-remove-keeps-empty-file",
        "journal.rs",
        """        if dialogs.is_empty() {
            return remove_file(&self.path);
        }""",
        """        if dialogs.is_empty() {
            return self.write(&JournalRecord { dialogs });
        }""",
        "input-answered",
    ),
    (
        "channel-skips-owner-release",
        "channel.rs",
        "        release(&state.request_id, &state.owner_id)?;",
        "        let _ = release;",
        "input-answered",
    ),
    (
        "channel-accepts-unavailable-owner",
        "channel.rs",
        "        if !claimed {",
        "        if false && !claimed {",
        "claim-false",
    ),
    (
        "channel-renew-failure-continues",
        "channel.rs",
        """                Ok(false) | Err(_) => {
                    finish_poll(&self.inner, request_id);
                    return;
                }""",
        "                Ok(false) | Err(_) => {}",
        "renew-failure-then-answer",
    ),
    (
        "channel-abort-before-register-ignored",
        "channel.rs",
        "        if aborted {",
        "        if false && aborted {",
        "abort-during-store-put",
    ),
    (
        "channel-accepts-cross-gateway-invalid-value",
        "channel.rs",
        "                if !(state.accepts)(value) {",
        "                if false && !(state.accepts)(value) {",
        "cross-gateway-invalid",
    ),
    (
        "channel-swallows-emit-error",
        "channel.rs",
        """        if let Err(error) = (self.inner.options.emit)(request_event.clone()) {
            self.complete(&state, Err(error.to_string()));
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }""",
        """        if let Err(_error) = (self.inner.options.emit)(request_event.clone()) {
            return ChannelRequest {
                request_id,
                dialog_kind: dialog_kind.into(),
                state,
                receiver,
            };
        }""",
        "emit-throws",
    ),
    (
        "channel-ignores-store-put-error",
        "channel.rs",
        "            store.put(key, stored)?;",
        "            let _ = store.put(key, stored);",
        "store-put-fails",
    ),
    (
        "channel-settles-store-before-journal",
        "channel.rs",
        """    if let Some(journal) = &inner.options.journal {
        journal.remove(&state.request_id)?;
    }
    if let (Some(store), Some(key)) = (&inner.options.store, &inner.options.store_key) {
        store.remove(key, &state.request_id)?;
    }""",
        """    if let (Some(store), Some(key)) = (&inner.options.store, &inner.options.store_key) {
        store.remove(key, &state.request_id)?;
    }
    if let Some(journal) = &inner.options.journal {
        journal.remove(&state.request_id)?;
    }""",
        "journal-remove-fails",
    ),
    (
        "channel-abort-during-register-leaks",
        "channel.rs",
        """        if !state.registered.load(Ordering::SeqCst) {
            // The request is between the persist gate and `registered = true`.
            // Only raise the flag: removing the state here would make the
            // post-registration cancel a no-op and leak the bus entry, owner
            // lease, journal record and store record. `begin` settles it once
            // the entry is registered.
            state
                .cancelled_before_registration
                .store(true, Ordering::SeqCst);
            return;
        }""",
        """        if !state.registered.load(Ordering::SeqCst) {
            state
                .cancelled_before_registration
                .store(true, Ordering::SeqCst);
            if !state.settled.load(Ordering::SeqCst) {
                self.complete(&state, Ok(DialogAnswer::Cancelled(Some(reason.into()))));
            }
            return;
        }""",
        "abort-during-register",
    ),
    (
        "channel-shared-signal-clears-siblings",
        "channel.rs",
        """            .callbacks
            .retain(|(id, _)| *id != token);""",
        """            .callbacks
            .clear();""",
        "shared-signal-two-requests",
    ),
    (
        "journal-record-unlocked",
        "journal.rs",
        """    pub fn record(&self, event: &JsValue) -> Result<()> {
        let _guard = self.lock.lock().unwrap();""",
        """    pub fn record(&self, event: &JsValue) -> Result<()> {""",
        "concurrent record lost entries",
    ),
    (
        "journal-remove-unlocked",
        "journal.rs",
        """    pub fn remove(&self, request_id: &JsString) -> Result<()> {
        let _guard = self.lock.lock().unwrap();""",
        """    pub fn remove(&self, request_id: &JsString) -> Result<()> {""",
        "concurrent remove lost updates",
    ),
    (
        "journal-directory-read-as-missing",
        "journal.rs",
        """            let bytes = match fs::read(&self.path) {""",
        """            if self.path.is_dir() {
                return Ok(JournalRecord {
                    dialogs: Vec::new(),
                });
            }
            let bytes = match fs::read(&self.path) {""",
        "journal directory-at-path",
    ),
    (
        "channel-cancel-drops-empty-reason",
        "mod.rs",
        """                if let Some(reason) = reason {
                    set(&mut value, "reason", JsValue::String(reason.clone()));""",
        """                if let Some(reason) = reason.as_ref().filter(|reason| !reason.is_empty()) {
                    set(&mut value, "reason", JsValue::String(reason.clone()));""",
        "respond-cancel-empty-reason",
    ),
    (
        "channel-cross-gateway-keeps-empty-reason",
        "channel.rs",
        """                let reason = reason.clone().filter(|reason| !reason.is_empty());
                (entry.resolve)(DialogAnswer::Cancelled(reason));""",
        """                (entry.resolve)(DialogAnswer::Cancelled(reason.clone()));""",
        "cross-gateway-cancelled-empty-reason",
    ),
    (
        "tampered-source-hash",
        "oracle.json",
        json.loads(originals["oracle.json"])["sourceSha256"][
            "src/gateway/user-dialog/GatewayUserDialogJournal.ts"
        ],
        "0" * 64,
        "oracle hash for src/gateway/user-dialog/GatewayUserDialogJournal.ts",
    ),
]

selected = set(sys.argv[2:])
assert selected <= {"baseline"} | {name for name, *_ in mutants}, selected
if selected - {"baseline"}:
    mutants = [mutant for mutant in mutants if mutant[0] in selected]

# Most controls are killed by the oracle replay; the journal lock races are
# only observable through the deterministic concurrency test.
targets = {
    "journal-record-unlocked": "user_dialog::channel_tests::journal_concurrent_record_and_remove_are_serialized",
    "journal-remove-unlocked": "user_dialog::channel_tests::journal_concurrent_record_and_remove_are_serialized",
}

manifest = f"""[package]
name = "pdrs-udc-negative"
version = "0.0.0"
edition = "2024"

[dependencies]
base64 = "=0.22.1"
pilotdeck-protocol = {{ path = {json.dumps(str(src_root / "pilotdeck-protocol"))} }}
pilotdeck-transport = {{ path = {json.dumps(str(src_root / "pilotdeck-transport"))} }}
sha2 = "=0.10.9"
uuid = {{ version = "=1.27.0", features = ["v4"] }}
"""


def stage(scratch: Path) -> None:
    target = scratch / "src/user_dialog"
    target.mkdir(parents=True)
    (scratch / "src/lib.rs").write_text(
        '#[path = "user_dialog/mod.rs"] pub mod user_dialog;\n'
        '#[path = "test_baseline.rs"] mod test_baseline;\n'
    )
    (scratch / "Cargo.toml").write_text(manifest)
    (scratch / "src/test_baseline.rs").write_text(baseline_helper.read_text())
    (scratch / "tests/fixtures/user-dialog-channel").mkdir(parents=True)


def write_sources(scratch: Path, name: str | None = None) -> None:
    for filename, content in originals.items():
        if filename == "oracle.json":
            destination = scratch / "tests/fixtures/user-dialog-channel/oracle.json"
        else:
            destination = scratch / "src/user_dialog" / filename
        destination.write_text(content)
    if name is not None:
        mutant = next(item for item in mutants if item[0] == name)
        _, filename, before, after, _ = mutant
        path = (
            scratch / "tests/fixtures/user-dialog-channel/oracle.json"
            if filename == "oracle.json"
            else scratch / "src/user_dialog" / filename
        )
        content = path.read_text()
        assert content.count(before) == 1, (name, content.count(before))
        path.write_text(content.replace(before, after))


def run(scratch: Path, command: list[str]) -> subprocess.CompletedProcess:
    env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TARGET_DIR=str(scratch / "target"))
    assert env.get("CARGO_HOME") and env.get("PDRS_BASELINE")
    return subprocess.run(command, env=env, capture_output=True, text=True, timeout=1800)


gate = subprocess.run(
    [
        "bash",
        "-c",
        'until bash "$1"; do sleep 30; done',
        "cargo-slots",
        str(fixture.parents[4] / "scripts/cargo-slots.sh"),
    ],
    capture_output=True,
    text=True,
    check=True,
)

summary = []
with tempfile.TemporaryDirectory(prefix="pdrs-udc-negative-", dir="/tmp") as temp:
    scratch = Path(temp)
    stage(scratch)
    for name in ["baseline"] + [item[0] for item in mutants]:
        write_sources(scratch, None if name == "baseline" else name)
        command = [
            "cargo",
            "test",
            "--offline",
            "--manifest-path",
            str(scratch / "Cargo.toml"),
            "--lib",
            targets.get(
                name,
                "user_dialog::channel_tests::channel_and_journal_match_real_ts_oracle_step_by_step",
            ),
            "--",
            "--nocapture",
        ]
        result = run(scratch, command)
        raw = result.stdout + result.stderr
        expected = (
            "oracle hash for src/gateway/user-dialog/GatewayUserDialogJournal.ts"
            if name == "tampered-source-hash"
            else next((item[4] for item in mutants if item[0] == name), "")
        )
        passed = (
            result.returncode == 0
            if name == "baseline"
            else result.returncode == 101
            and "assertion `left" in raw
            and expected in raw
            and "could not compile" not in raw
        )
        (output / f"negative-{name}.log").write_text(
            gate.stdout
            + gate.stderr
            + "COMMAND "
            + " ".join(command)
            + "\n"
            + raw
            + f"\nexit={result.returncode}\n"
        )
        summary.append(
            {
                "name": name,
                "expected_failure_context": expected,
                "exit": result.returncode,
                "passed": passed,
            }
        )
        (output / "negative-summary.json").write_text(
            json.dumps(summary, ensure_ascii=False, indent=2) + "\n"
        )
        print(name, "PASS" if passed else "INVALID", flush=True)
        if not passed:
            print(raw[-4000:], flush=True)
            raise SystemExit(1)
    if selected:
        print("selected controls rejected; temporary sources removed on exit")
        raise SystemExit(0)

print("all controls rejected; temporary sources removed")
