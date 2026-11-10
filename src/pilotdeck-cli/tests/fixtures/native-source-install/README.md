# Native source install/update fixture

Packages the actual install-driver.py and consumer-driver.py retained in
checks-r24/_tmp/codex-install-execution-r2-20261110. Original evidence is preserved.
The parameterized installation driver is NOT_RUN. The adapted consumer was
actually run on the prior installed 2b9 generation with --expected-skills 32:
PASS, including owned process cleanup. Default 33 on a new installation remains
NOT_RUN. No release build was repeated.

Use an isolated POSIX build host with /bin/ps supporting lstart, hold the common
parity lock, and reserve Cargo capacity using the existing approved shim.
Supply:
python3 install-driver.py --control /frozen/pilotdeck-control --source /clean/repo --old-ref FULL_OLD_SHA --new-ref FULL_NEW_SHA --cargo-shim /approved/shim/cargo --regress-db /public/regress-object-store --registry-cache /public/cargo/registry --rustup-home /existing/rustup --out /exclusive/new/install
Then (default current contract is 33; use --expected-skills 32 only for old 2b9):
python3 consumer-driver.py --install /exclusive/new/install/installation --out /exclusive/new/consumer

Both output parents must exist and output directories must not. Each generation
has its own .update-target. This performs two real release builds; do not run
merely to test fixture packaging.

Only already cached public dependency objects are permitted. The regress cache
URL must be exactly https://github.com/ridiculousfish/regress and the imported
commit exactly 95c04041cd9b30249961eb80d950102c29c9586c. A fresh bare store imports
that commit; user Git/Cargo configuration and credentials are not copied.
Cargo is offline and Git only permits file transport. No downloads, real accounts,
user PilotDeck homes/config, or live services may be used.

Retained checks: clean source/ancestor revisions; normal control install then
installed CLI update; four executable hashes and generation manifest identities;
complete runtime-library inventory and regular-file hashes (required by image-native source generations, absent for legacy generations);
stable entry links; unchanged old payload bytes/modes/symlinks (except fetch-
modified source/.git); unchanged install policy; 20ms current-link observations.
Sampling is not a syscall-level atomicity proof.

The consumer exclusively binds 127.0.0.1:18789 before CLI launch, preventing
legacy discovery from contacting a preexisting service. Occupied port means
FAIL; never evict the owner or enable address reuse. The mock model uses a
dynamic loopback port. Deadline 180 seconds, at most six requests. Assert all
the exact standalone 32 names and returned read_skill results, 109 asset hashes, paths within
installed skills, final output, and observed default sidecar with no override.

Cleanup signals sampled descendant PID + matching birth time only. Installation
already rejects surviving observed identities; this candidate adds that final
gate to the consumer. Original consumer PASS relied on a separate final census.
Sampling may miss fast-fork/reparented children and is not a kernel ownership
proof. Source and original raw evidence are never modified.

Default --expected-skills 33 additionally requires exactly windows-skills:powershell-safe-invocation at skills/.builtin-plugins/windows-skills/skills/powershell-safe-invocation/SKILL.md, SHA256 6748cbc46061399ad2ce3270cfd7ecd7ac37fc11aef18a38183bd6bdf0eb3d2a. Both loopback listeners explicitly disable address reuse.
