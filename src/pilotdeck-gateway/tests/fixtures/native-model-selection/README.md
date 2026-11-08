# Native turn model selection oracle

`oracle.json` uses the fixed TS `GatewaySessionModelBundle` and its real `NativeSessionModelSelectionPolicy` with a recording project resolver and empty session store. It was generated on mbp2 with Node v22.22.0 and the pinned PilotDeck checkout. Only the fixture project path was normalized to `<project>`; error names were removed because the native Services port exposes code and message. The original full output and source are in `_tmp/20261108-native-model-oracle-01/run-extended/oracle.json`.

Run `tsx generate.mts <new-empty-output-directory>` with `/Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/.bin/tsx` and PATH `/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin`; normalize only generated projectKey and omit `reads` and error `name` as above. No network/provider calls are made. The recording resolver asserts project validation order; this is not a claim that it is the real project registry.
