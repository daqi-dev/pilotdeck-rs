# WhatsApp first-party Rust candidate, issue1338

Base CLI 3637484a8044ef6ea6ae9b9bea68955423f062ad; fixed TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85. Only new channel/whatsapp directory is owned here, shared registration/assembly/Cargo remain root-owned.

## Approved process topology

Rust Gateway -> owned Node process executing user-configured external bridgePath -> external WhatsApp integration/account. Rust polls GET bridgeUrl/messages and sends POST bridgeUrl/send. Incoming messages enter the existing Rust A7 TurnGateway and real InteractionPort. There is no internal first-party TS Gateway/helper fallback.

Fixed TS has only WhatsAppChannel.ts, WhatsAppSessionMapper.ts and whatsapp-render.ts. Full baseline git grep found no bridge implementation, Baileys or whatsapp-web.js dependency. The bridge is an externally supplied integration contract; its cryptographic/authentication implementation cannot be inferred from this repository. Root explicitly approved retaining this external JS bridge compatibility mode on 2026-11-10. No claim of all-Rust encryption or all-Node retirement. Formal no-internal-TS witness must list external bridge as the approved retained role. Production never defaults to mock-bridge.mjs.

Node executable availability and bridgePath configuration are distinct: absent path follows fixed TS observable noop/error; absent executable produces spawn error; supplied script failing readiness produces readiness error. node_executable permits root to bind the verified Node binary; default resolves node from PATH. Fixed TS uses its running process.execPath, so formal runs must explicitly supply the matching verified binary.

## Behavior and lifecycle

URL strips trailing slashes, extracts only explicit nonzero port to BRIDGE_PORT. Readiness polls /messages at 400ms with 3s exchanges, accepting2xx/404 until15s. Then polling every2s uses cancellable owned tasks; seen IDs clear on start, per-chat active admission and mapper state match TS. send timeout60s, JSON error fallback and cron origins follow TS. Permission/elicitation use shared helper port with delivery confirmations; integration must supply the real responder.

Bridge spawn has null stdin, piped stdout/stderr, inherited environment plus BRIDGE_PORT. Stderr is observable. Child exit while running logs an error. Fixed TS does NOT automatically restart the bridge: continued poll failures remain observable, and this candidate preserves that behavior. Restart/reload creates a new owned child through channel start, not a hidden SDK reconnect. Stop closes admission, aborts poll tasks and timers, signals only its own Child with TERM, waits5s then KILL/reap. The same owned Unix child is not reaped by another waiter, preventing PID reuse before signal. No PID-wide discovery/kill or unknown-process cleanup. Business turns already admitted are not cancelled/awaited by single-channel stop; active_tasks()/drain() expose them to global owner. Outbound sends after stop returnfalse. Actor handle is retained by ChannelHandle; callback/task shared references have no channel-to-task-to-channel cycle.

## Evidence and remaining gates

capture.mjs imports real fixed TS classes and spawns only the owned mock-bridge.mjs loopback script using the actual external-bridge contract. No SDK stub or resources edit required. Original _tmp/20261110-whatsapp-ts-r1/oracle.json; copy here samebytes. Actual 4 send requests,2 turns, crontrue/false; repeated IDs and activechat skip; second turn released after stop; owned child HTTP endpoint stopped. Node22.22.0/ICU77.1/libuv1.51.0 diagnostic only, not formal ICU78.2 acceptance.

Run: node --import /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/src/channel/whatsapp/capture.mjs /Users/da/ws/pdrs-runner/resources/PilotDeck _tmp/20261110-whatsapp-ts-r1

Rust tests cover mapper/render/normalization goldens plus actual managed Node child/HTTP/A7/reply/stop and missingpath vs missingNode. Rustfmt parsed, semantic compile awaits root. Remaining: integrated configuration/reload, formal pinned environment/Linux, real helper interaction tests, child crash/readiness timeout/TERM resistance/poll abort runtime coverage,3 KILLED negativecontrols, independent review. Test port allocation currently uses ephemeral bind/drop before externalchild bind: TOCTOU risk is disclosed, not treated as formal fixture proof. External production bridge implementation/account capability remains supplied by deployment, never proven by this mock. No E1/channel acceptance claim.
