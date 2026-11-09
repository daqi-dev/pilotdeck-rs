# Production cache collation regression

This is an additional product fixture. It does not change the existing 10-locale,
118810-sign, 70-plan oracle. Four deliberately opposed tool-name/description
inputs are executed by the real fixed `CachePlan.ts` module. The two lone UTF16
surrogates compare differently, while a replacement-character decoder would
collapse them and incorrectly select the description tiebreak. Both input orders
and an explicit U+FFFD tool are covered.

`default_factory_cache_tests.rs` calls the production
`assemble_with_native_collation` function, including its real process locale
lookup and comparator, for each captured input in normal and preview modes.
It compares the entire Node plan and fingerprint and verifies that the actual
request's tool order remains unchanged. The parent test starts an owned test
subprocess with LANG=en_US.UTF-8 and no LC_ALL/LC_MESSAGES; it never mutates the
Rust process environment or constructs a test comparator. Its ignored helper is
actually required to succeed by the parent within fifteen seconds.

Capture uses Node v22.22.0 / ICU78.2 / TypeScript5.9.3 and verifies the complete
CachePlan.ts SHA256 before in-memory transpilation. Source baseline:
OpenBMB/PilotDeck@cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85.
Only that pure source module is evaluated; no application bootstrap, provider,
real installation or user home is read. The capture script is self-written,
does not copy the baseline implementation, takes BASELINE and NEW_OUTPUT_DIRECTORY,
and makes the latter exclusively.

On mbp2, prepare an exclusive scratch directory containing empty home/tmp and
use sandbox-exec to deny all network, process fork, and file writes except below
that scratch directory (the same profile as the existing collect.py).
Run the pinned Node with `env -i PATH=/usr/bin:/bin HOME=<scratch/home>
PILOT_HOME=<scratch/home> TMPDIR=<scratch/tmp> LANG=en_US.UTF-8 TZ=UTC`,
then `production-capture.mjs BASELINE <scratch/capture>`.
LC_ALL and LC_MESSAGES must be absent. Warnings and version/source mismatches
are fatal. Compare capture/oracle.json byte-for-byte with production-oracle.json;
do not derive expected fingerprints from Rust.

The original actual capture is kept at mbp2
`/Users/da/ws/pdrs-worker-wt/control-cli-20261108/_tmp/cache-p2-sol-20261108/capture/`
with stdout, stderr, environment, sandbox profile and oracle. Oracle SHA256:
`1cb2ac2015a150eac673e664196b0609888cb5fc3524149f754aa50759ac8fad`.

Rust validation (only on the assigned remote build slot):

```sh
cargo test --manifest-path src/Cargo.toml -p pilotdeck-transport --lib \
  default_factory::native_cache_collation_tests::production_cache_assembly_preserves_lone_surrogate_tool_order -- --nocapture
```

A real negative check should temporarily change the unique production
`compare_utf16_preserving_surrogates` call in default_factory.rs to
`compare_utf16`, require this parent test to FAIL, then restore and verify the
original source SHA. The standalone cache helper test cannot substitute for this
production injection test. No author Rust run is claimed by the candidate.
