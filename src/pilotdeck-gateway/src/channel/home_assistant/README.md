# HomeAssistant candidate, issue 1335

Base CLI 3637484a8044ef6ea6ae9b9bea68955423f062ad; TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85. New exclusive directory only, no shared registration/Cargo changes. Existing permission/owner files remain frozen.

Uses TurnGateway and injected InteractionPort. Integration must inject HelperInteraction with real InteractionResponder. NoInteraction does not demonstrate permission wiring.

Tests cover real loopback WS auth/subscription/events/service ACK, mapper/render oracle, chat admission, permission/elicitation receipts, cron origin, reconnect, invalid auth, cancelled start, stop/restart. Stop follows TS: closes admission/socket/reconnect and fails pending ACK, without cancelling/waiting admitted turns. active_tasks()/drain() expose outstanding tasks; global A7 owner drains business work. No channel-to-task-to-channel handle cycle. Existing BlueBubbles stop differences remain separate.

TS capture imports actual official classes and existing ws package, without SDK stubs or copied resources. Original: _tmp/20261110-home-assistant-ts-r1/oracle.json; oracle.json here is identical. Observed seven wire requests, two turns, cron true/false, pending ACK false on stop and second turn released after stop. Node 22.22.0/ICU 77.1/libuv 1.51.0 are diagnostic only, not formal pinned ICU 78.2 acceptance.

Run: node --import /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/src/channel/home_assistant/capture.mjs /Users/da/ws/pdrs-runner/resources/PilotDeck _tmp/20261110-home-assistant-ts-r1

Rustfmt syntax check passed. Semantic compilation/tests await root registration. No acceptance claim. TLS, real HA deployment, formal environment and integrated config-start/inbound/turn/outbound/reload/stop still require integration validation; loopback restart is not integrated hot reload evidence.
