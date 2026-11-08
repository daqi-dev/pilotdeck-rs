# SDK hook matcher timeout TS observation

Fixed checkout cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85; fixed resource: SessionInteractionBundle.ts SHA256 e31efd0d2525410915b9ec8f8a23baac757db90455ab73a2ece8a8a5f49fd209; Node v22.22.0, ICU 78.2. The capture imports the actual TS bundle, supplies an abort-aware fake fetch, passes matcher.timeout=0.01, and delays the response 250 ms. The retained observed.json reports 261.2 ms and the successful SDK_BLOCK effect; elapsed time is an observation, not an exact golden. InProcessGateway validates the timeout at admission; SessionInteractionBundle lines 325–341 do not copy it into HTTP matcher settings.

On mbp2, choose an unused output directory and run:

    PATH=/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin:$PATH /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/.bin/tsx capture.mts /absolute/unused/output

The capture uses exclusive output-file creation and performs no external networking. It requires the fixed checkout and does not download dependencies.


`lifecycle.mts` imports the same actual bundle and captures four further cases in `lifecycle.json`. Accepted ConfigChange and FileChanged HTTP work drains before scope disposal; FileChanged `didSave` waits for its hook. Async ConfigChange stays in its local pending registry and does not call the Gateway registration callback, whereas FileChanged does. This is a controlled transport observation, not a full Gateway smoke. Run it with the same pinned tsx and a new output directory. The first trial used the wrong response key `invocationId`; that failed assertion is retained remotely. The successful capture uses the actual `asyncHookId` wire key. Native event dispatch additionally has a per-session concurrency budget of 64; it is not shared across sessions.
