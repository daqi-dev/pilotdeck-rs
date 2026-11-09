#!/usr/bin/env python3
# anchor-selfcheck: unsupported Native business integration probe; no source mutation table.
"""Configured Cron to model and webhook, using the native-history harness."""
from pathlib import Path
source = Path(__file__).resolve().parent.parent / "native-history" / "native.py"
text = source.read_text()
config_anchor = '(home / "pilotdeck.yaml").write_text(config)'
anchor = '    url = "ws://" + address + "/ws"\n    code, out, err = control("status", {})'
monitor_anchor = "    monitor = threading.Thread(target=observe_descendants, daemon=True)"
stdout_assertion = '    assert host.stdout.read() == b"", "foreground host emitted unexpected stdout"'
if any(text.count(item) != 1 for item in (config_anchor, anchor, monitor_anchor, stdout_assertion)):
    raise SystemExit("native-history driver anchors changed")
stdout_reader = """    channel_stdout = []
    def collect_channel_stdout(child):
        for line in child.stdout:
            channel_stdout.append(line.decode(errors="replace"))
    channel_reader = threading.Thread(target=collect_channel_stdout, args=(host,), daemon=True)
    channel_reader.start()
"""
text = text.replace(monitor_anchor, stdout_reader + monitor_anchor, 1)
text = text.replace(stdout_assertion, '    channel_reader.join(timeout=remaining()); assert not channel_reader.is_alive(); rows.append({"channel_stdout": channel_stdout})', 1)
text = text.replace(config_anchor, r'(home / "pilotdeck.yaml").write_text(config + "\ncron:\n  enabled: true\nadapters:\n  webhook: {enabled: true, extra: {port: 0}}\n")', 1)
case = r"""
    project_key = str(project)
    create = {"method": "cron_create", "params": {
        "projectKey": project_key, "message": "cron-consumer-check",
        "sessionKey": "webhook:chat=cron-probe:general", "channelKey": "webhook",
        "mode": "default", "schedule": {"type": "once", "runAt": "2099-01-01T00:00:00.000Z"}}}
    code, out, err = control("call", create)
    assert code == 0 and not err, (code, out, err)
    created = json.loads(out)
    assert created["ok"] is True, created
    original_id = created["result"]["task"]["taskId"]
    code, out, err = control("call", {"method": "cron_run_now", "params": {
        "projectKey": project_key, "taskId": original_id}})
    assert code == 0 and not err, (code, out, err)
    started = json.loads(out)
    assert started["ok"] is True and started["result"]["started"] is True, started
    run_task_id = started["result"]["taskId"]
    finish = None
    while time.monotonic() < deadline:
        code, out, err = control("call", {"method": "cron_list", "params": {
            "projectKey": project_key, "includeHistory": True}})
        assert code == 0 and not err, (code, out, err)
        listing = json.loads(out)
        assert listing["ok"] is True, listing
        finish = next((r for r in listing["result"].get("recentRuns", [])
                       if r.get("taskId") == run_task_id), None)
        if finish is not None:
            break
        time.sleep(0.1)
    assert finish is not None and finish["outcome"] == "completed", finish
    provider = [r["provider"] for r in rows if "provider" in r and r["provider"].get("stream")
                and "cron-consumer-check" in json.dumps(r["provider"].get("messages"), ensure_ascii=False)]
    assert provider, "cron never reached the configured model adapter"
    delivered = None
    while time.monotonic() < deadline:
        delivered = next((line for line in channel_stdout
                          if "webhook: response for cron-probe:" in line and "native stream text" in line), None)
        if delivered:
            break
        time.sleep(0.1)
    assert delivered, "no observed webhook result; inspect gateway.stderr before assigning root cause"
    rows.append({"cron_consumer": {"task": run_task_id, "run": finish, "delivery": delivered}})
"""
prefix, status = anchor.split("\n", 1)
text = text.replace(anchor, prefix + "\n" + case + "\n" + status, 1)
exec(compile(text, str(source), "exec"))
