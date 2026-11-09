#!/usr/bin/env python3
"""Run the existing isolated native-history driver with a portable-resume case.

The host, fake provider, binary snapshots, deadlines and PID/SID cleanup remain
owned by native-history/native.py. Fails closed if its insertion anchor drifts.
"""
from pathlib import Path

source = Path(__file__).resolve().parent.parent / "native-history" / "native.py"
text = source.read_text()
anchor = '    url = "ws://" + address + "/ws"\n    code, out, err = control("status", {})'
if text.count(anchor) != 1:
    raise SystemExit("native-history driver anchor changed")
case = r"""
    # Real public Gateway request -> disk restore -> session turn -> mock provider.
    fixture = json.loads((Path(__file__).resolve().parent / "oracle.json").read_text())
    archive = fixture["cases"]["export_restored"]["value"]
    session = "portable-resume"
    call = {"method": "restore_session_transcript", "params": {
        "projectKey": str(project), "sessionKey": session, "archive": archive}}
    code, out, err = control("call", call)
    assert code == 0 and not err, (code, out, err)
    restored = json.loads(out)
    assert restored["ok"] is True and restored["result"]["importedMessages"] == 2, restored
    saved = history(session)
    before = [(m.get("role"), m.get("text")) for m in saved["messages"] if m.get("kind") == "text"]
    assert before[:2] == [("user", "hello"), ("assistant", "world")], before
    assert saved["session"].get("customTitle") == "Title", saved["session"]
    turn = {"sessionKey": session, "runId": "portable-resume-run", "channelKey": "cli",
            "projectKey": str(project), "message": "followup", "canPrompt": False}
    first_request = len(rows)
    code, out, err = control("stream", turn)
    assert code == 0 and not err, (code, out, err)
    verify_stream(out, True)
    requests = [row["provider"] for row in rows[first_request:] if "provider" in row
                and row["provider"].get("stream")]
    assert requests, "no actual model streaming request"
    messages = requests[-1]["messages"]
    expected = [("user", "hello"), ("assistant", "world"), ("user", "followup")]
    found = []
    for msg in messages:
        role = msg.get("role")
        content = json.dumps(msg.get("content"), ensure_ascii=False)
        for item in expected:
            if role == item[0] and item[1] in content and item not in found:
                found.append(item)
    assert found == expected, {"expected": expected, "found": found, "messages": messages}
    after = history(session)
    texts = [(m.get("role"), m.get("text")) for m in after["messages"] if m.get("kind") == "text"]
    assert ("user", "followup") in texts and ("assistant", "native stream text") in texts, texts
    rows.append({"portable_resume": {"model_messages": found, "history_before": before, "history_after": texts}})
"""
prefix, status = anchor.split("\n", 1)
exec(compile(text.replace(anchor, prefix + "\n" + case + "\n" + status, 1), str(source), "exec"))
