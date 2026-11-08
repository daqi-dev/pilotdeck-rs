# Plan and todo fixtures

`read-file-oracle.json` was captured using the fixed TS `createPlanFileManager` with real empty, ASCII whitespace, JS whitespace, and nonempty files. Reproduce with `PILOTDECK_TS_ROOT=/path/to/pinned/PilotDeck tsx read-file.mts <new-output-directory>`. Raw mbp2 evidence: `/Users/da/ws/pdrs-worker-wt/control-cli-20261108/_tmp/20261108-runtime-boundary-oracles/plans/oracle.json`. The native file reader compares all four cases, including TS undefined for empty content.
