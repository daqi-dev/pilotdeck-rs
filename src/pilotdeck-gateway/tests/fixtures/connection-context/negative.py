#!/usr/bin/env python3
"""Only mutate isolated /tmp sources; an oracle assertion, not a build failure, catches behavior mutants."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
source = rust / "pilotdeck-gateway/src/runtime/connection"
originals = {p.name: p.read_text() for p in source.glob("*.rs")}
env = dict(os.environ, CARGO_NET_OFFLINE="true", CARGO_TARGET_DIR=str(rust / "target"))
assert env.get("CARGO_HOME") and env.get("PDRS_BASELINE")
# Capture exact formatted blocks so the validation order mutation moves whole checks.
module = originals["mod.rs"]
start = module.index('            if frame["protocolVersion"] != PROTOCOL_VERSION {')
end = module.index('            state.authed = true;', start)
checks = module[start:end]
protocol, auth = checks.split('            if frame["token"] != self.token {', 1)
auth = '            if frame["token"] != self.token {' + auth
mutants = [
    ("cleanup-after-final-frame", "mod.rs", "        drop(cleanup);", "        let _late_cleanup = cleanup;", 1),
    ("remove-server-override", "context.rs", 'params["interactionBinding"] = binding.to_json();', 'params["interactionBinding"] = params.get("interactionBinding").cloned().unwrap_or(Value::Null);', 2),
    ("remove-sdk-control-special-case", "context.rs", 'if !sdk_control {', 'if true || !sdk_control {', 1),
    ("swap-validation-order", "mod.rs", checks, auth + protocol, 1),
    ("wrong-close-code", "mod.rs", 'self.text.close(4003, "auth_failed");', 'self.text.close(4001, "auth_failed");', 1),
    ("numeric-generation-to-string", "binding.rs", '"generation": self.generation', '"generation": self.generation.to_string()', 1),
    ("accept-string-generation", "binding.rs", '.as_f64()', '.as_f64().or_else(|| value["generation"].as_str().and_then(|s| s.parse::<f64>().ok()))', 1),
    ("ignore-preserve-turn", "mod.rs", 'if in_flight && !preserve_turn {', 'if in_flight && (true || !preserve_turn) {', 1),
    ("old-binding-disconnects-current", "binding.rs", 'if bindings.get(owner) != Some(binding) {', 'if false && bindings.get(owner) != Some(binding) {', 1),
    ("old-submit-clears-replacement", "mod.rs", '.retain(|(_, ticket)| *ticket != self.ticket);', '.retain(|(_, _ticket)| false);', 1),
]
summary = []
with tempfile.TemporaryDirectory(prefix="pdrs-pr3-negative-", dir="/tmp") as temp:
    scratch = Path(temp)
    (scratch / "src/runtime/connection").mkdir(parents=True)
    (scratch / "tests/fixtures/connection-context").mkdir(parents=True)
    (scratch / "tests/fixtures/connection-context/oracle.json").write_bytes((fixture / "oracle.json").read_bytes())
    base_lib = '#[path="runtime/connection/mod.rs"] pub mod connection;\n' + f'#[cfg(test)] #[path={json.dumps(str(rust / "pilotdeck-gateway/tests/common/baseline.rs"))}] mod test_baseline;\n'
    manifest = '[package]\nname="pdrs-pr3-negative"\nversion="0.0.0"\nedition="2024"\n[dependencies]\n' + f'pilotdeck-protocol={{path={json.dumps(str(rust / "pilotdeck-protocol"))}}}\n' + 'serde_json={version="=1.0.151",features=["float_roundtrip"]}\nsha2="=0.10.9"\nuuid={version="=1.27.0",features=["v4"]}\ntokio={version="=1.48.0",features=["macros","rt-multi-thread","sync"]}\n'
    (scratch / "Cargo.toml").write_text(manifest)
    for name, filename, before, after, count in [("baseline", None, None, None, 0)] + mutants:
        for file, content in originals.items():
            if file == filename:
                assert content.count(before) == count, (name, content.count(before))
                content = content.replace(before, after)
            (scratch / "src/runtime/connection" / file).write_text(content)
        (scratch / "src/lib.rs").write_text(base_lib)
        gate = subprocess.run(["bash", "-c", "until bash /Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh; do sleep 30; done"], capture_output=True, text=True, check=True)
        test_filter = "connection::tests::"
        command = ["cargo", "test", "--offline", "--manifest-path", str(scratch / "Cargo.toml"), "--lib", test_filter, "--", "--nocapture"]
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=900)
        raw = result.stdout + result.stderr
        passed = result.returncode == 0 if name == "baseline" else result.returncode == 101 and "assertion `left" in raw and "could not compile" not in raw
        (output / f"negative-{name}.log").write_text(gate.stdout + gate.stderr + "COMMAND " + " ".join(command) + "\n" + raw + f"\nexit={result.returncode}\n")
        summary.append({"name":name,"file":filename,"before":before,"after":after,"exit":result.returncode,"passed":passed})
        (output / "negative-summary.json").write_text(json.dumps(summary,ensure_ascii=False,indent=2)+"\n")
        print(name, "PASS" if passed else "INVALID", flush=True)
        if not passed: raise SystemExit(1)
    # Compiler-level identity controls use the real project runtime type, not a
    # lookalike. These are explicitly type rejection, not oracle behavior catches.
    for file, content in originals.items(): (scratch / "src/runtime/connection" / file).write_text(content)
    (scratch / "Cargo.toml").write_text(manifest + f'pilotdeck-gateway={{path={json.dumps(str(rust / "pilotdeck-gateway"))}}}\n')
    for name, statement in [
        ("project-runtime-as-binding-generation", 'let _: u64 = runtime;'),
        ("binding-generation-as-project-runtime", 'let _: std::sync::Arc<pilotdeck_gateway::runtime::project::ProjectRuntime> = binding.generation();'),
    ]:
        (scratch / "src/lib.rs").write_text(base_lib + 'fn reject(runtime: std::sync::Arc<pilotdeck_gateway::runtime::project::ProjectRuntime>, binding: connection::binding::WsBinding) {' + statement + '}')
        gate = subprocess.run(["bash", "-c", "until bash /Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh; do sleep 30; done"],capture_output=True,text=True,check=True)
        command=["cargo","check","--offline","--manifest-path",str(scratch / "Cargo.toml"),"--lib"]
        result=subprocess.run(command,env=env,capture_output=True,text=True,timeout=900)
        raw=result.stdout+result.stderr
        passed=result.returncode==101 and "error[E0308]" in raw and "ProjectRuntime" in raw and "u64" in raw
        (output / f"negative-{name}.log").write_text(gate.stdout+gate.stderr+"COMMAND "+" ".join(command)+"\n"+raw+f"\nexit={result.returncode}\n")
        summary.append({"name":name,"kind":"compile-time identity rejection","statement":statement,"exit":result.returncode,"passed":passed})
        (output / "negative-summary.json").write_text(json.dumps(summary,ensure_ascii=False,indent=2)+"\n")
        print(name,"PASS" if passed else "INVALID",flush=True)
        if not passed: raise SystemExit(1)
print("all controls rejected; temporary sources removed")
