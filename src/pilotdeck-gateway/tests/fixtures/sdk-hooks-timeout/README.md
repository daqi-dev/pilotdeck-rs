# SDK hook matcher timeout TS observation

Fixed checkout cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85; fixed resource: SessionInteractionBundle.ts SHA256 e31efd0d2525410915b9ec8f8a23baac757db90455ab73a2ece8a8a5f49fd209; Node v22.22.0, ICU 78.2. The user-authored capture imports the actual TS bundle, supplies an abort-aware fake fetch, passes matcher.timeout=0.01, and delays the response 250 ms. The retained observed.json reports 261.2 ms and the successful SDK_BLOCK effect; elapsed time is an observation, not an exact golden. Source lines 325–341 confirm the timeout is validated at admission but not copied into HTTP matcher settings.

On mbp2, choose an unused output directory and run:

    PATH=/Users/da/opt/node-v22.22.0-icu78-darwin-arm64/bin:$PATH /Users/da/ws/pdrs-runner/resources/PilotDeck/node_modules/.bin/tsx capture.mts /absolute/unused/output

The capture uses exclusive output-file creation and performs no external networking. It requires the fixed checkout and does not download dependencies.
