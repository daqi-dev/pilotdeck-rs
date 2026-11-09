# Native Cron consumer probe

native-cron.py reuses the isolated native-history process harness with checked insertion anchors. It enables Cron and loopback webhook, creates a task, runs it through public control, polls persisted run, verifies the mock provider received the task and checks the webhook result log. It then runs the original harness, retaining its same-source binary snapshots, deadlines and PID/SID cleanup.

Invoke with native-history --pilotdeck, --control, --gateway, --sidecar, --out arguments against an isolated same-source build. No real provider or channel credentials are used. An absent webhook log is an observation failure, not by itself proof of failed delivery. This remains a proposed probe until raw results and binary hashes are recorded.
