# Mattermost fixed-source diagnostic

The capture reads only fixed cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85 Git source objects; it records per-source SHA256 and actual REST/WebSocket traffic to an owned 127.0.0.1 mock. The original baseline uses the already-installed ws library, not a new SDK stub or package installation. Its actual version (8.21.0) is recorded. Token is the fixture literal. No external account, resource edits or credentials.

oracle-diagnostic.json was actually captured on cb8 Node22.22.1/ICU78.2 (PDRS_DIAGNOSTIC=1, formalAcceptance:false). The trace covers token check, real WS Origin/query, self/string-hook filtering, boolean hook distinction, CRLF normalization, thread session/root reply and stop closure. Default capture retains exact22.22.0/78.2 prerequisite. No frozen criteria changed.

Native code preserves the baseline's unusual URL contract: REST is serverUrl-minus-one-trailing-slash + /users/me or /posts; WS preserves the URL pathname and appends /api/v4/websocket. Do not silently insert /api/v4 on REST or remove a duplicate WS path without a separate approved behavior decision. Mattermost baseline has no cron delivery method; the native adapter retains the protocol's default false rather than inventing support.

Rust tests are written but not yet compiled/run in this batch. Three mutations must first pass baseline, compile separately, fail runtime, and finally rebuild/pass the byte-restored original. The runner has not been run. Formal dual-platform, interaction/reload/reconnect/failure coverage and independent reviews remain required; #1340 is not complete.
