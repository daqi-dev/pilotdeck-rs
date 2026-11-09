# DingTalk native candidate, issue 1342

Base CLI 3637484a8044ef6ea6ae9b9bea68955423f062ad. Fixed TS cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85. Only this new directory is owned here; root owns registration/configuration/assembly/Cargo.

The transport opens the official gateway endpoint via HTTPS POST with Accept: application/json, then connects returned WebSocket endpoint/ticket. EVENT frames call the adapter and return immediate SUCCESS ACK; business turns use the existing A7 TurnGateway and injected InteractionPort. SYSTEM ping/disconnect are acknowledged; disconnected sockets reconnect using bounded exponential delay with jitter. Transport references adapter weakly. Shared state counts outstanding business tasks and exposes drain(), without retained child handles pointing back to the channel.

Adapter implements topic filtering, message-ID FIFO dedup (2000), sessionWebhook FIFO (500), strict production webhook prefix, text/rich-text extraction, mapper /new state, per-chat admission, text/render output, 20000 UTF16-unit outbound truncation, permission/elicitation helpers with delivery confirmations and cron origin routing. Root must wire real HelperInteraction/InteractionResponder. NoInteraction is used solely in noninteractive tests.

Stop follows fixed TS: disconnect transport, clear webhook/dedup state, do not cancel/wait admitted business turns. Their replies fail after cache clear. Global owner handles drain. Native transport is intrinsic; TS comparison is the approved SDK-installed stub lane, not TS's absent-SDK noop state.

capture.mjs imports the actual fixed TS class and uses the minimal SDK stub approved in issue 1342 comment 6706597227 (issue 1786 human option 1). Only used DWClient/EventAck/TOPIC_ROBOT members are supplied; stub connects the same loopback HTTP/WS mock. Production-origin check remains in actual TS code; only fetch routing after that check redirects to mock. No resources copied or edited, no SDK installed, no real account traffic.

Original _tmp/20261110-dingtalk-ts-r1/oracle.json; oracle.json here same bytes. Node22.22.0/ICU77.1/libuv1.51.0 diagnostic only, not formal ICU78.2 acceptance. Actual observed: 5 HTTP requests, 6 ACKs, 2 turns, cron true/false and post-stop false, second admitted turn released after stop. Run:

node --import /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/tsx/dist/loader.mjs src/pilotdeck-gateway/src/channel/dingtalk/capture.mjs /Users/da/ws/pdrs-runner/resources/PilotDeck _tmp/20261110-dingtalk-ts-r1

Rust source parsed by rustfmt; compilation and semantic tests await root module registration. Tests use real loopback HTTP/WS to compare request bodies and turn calls to TS oracle, plus actual helper permission/elicitation flow. No acceptance claim. Remaining: integrated configuration-start/reload lifecycle, reconnect/system frames runtime tests, cache eviction/truncation boundary oracle expansion, formal pinned environment, Linux run, three killed negative controls, independent reviews. Native wire was informed by official open-dingtalk/dingtalk-stream-sdk-nodejs src/client.ts read 2026-11-10; the fixed adapter/approved stub remains the contract, not a claim to implement all current SDK features.
