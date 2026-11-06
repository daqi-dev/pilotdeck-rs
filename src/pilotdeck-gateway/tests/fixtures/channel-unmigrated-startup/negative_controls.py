#!/usr/bin/env python3
"""#1763 production-assembly controls: the unmigrated-channel refusal must stay
wired into `native_process::assembly::start`, not merely exist as a seam.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

Sibling of `channel-unmigrated`, which pins the seam itself (`channel::tests::`
over `channel/loader.rs`). This fixture pins the *assembly*: deleting the call
site or swallowing its error must turn `startup_refuses_*` red, and the bundle
key declaration must stay closed in both directions — every `BUNDLE_CHANNEL_KEYS`
entry needs an assembly constructor (#1694 §7; `feishu` joined both tables with
#1329, so an enabled feishu is now built rather than refused).
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
assembly = root / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
loader = root / 'src/pilotdeck-gateway/src/channel/loader.rs'

REJECT_BLOCK = (
    '        let rejected = crate::channel::loader::reject_unmigrated_channels(config.get("adapters"))\n'
    '            .and_then(|()| {\n'
    '                crate::channel::loader::reject_unimplemented_bundle_channels(\n'
    '                    config.get("adapters"),\n'
    '                    super::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS,\n'
    '                )\n'
    '            });\n'
    '        if let Err(error) = rejected {\n'
    '            // Same teardown order as the failure branch below (#1328): the\n'
    '            // session factory first, then the project registry.\n'
    '            let _ = tokio::time::timeout(options.drain_budget, async {\n'
    '                let _ = factory.dispose().await;\n'
    '                project.dispose().await\n'
    '            })\n'
    '            .await;\n'
    '            return Err(error.into());\n'
    '        }\n'
)

EVERY = ('startup_refuses_every_unmigrated_channel',
         '#1763 enabled unmigrated channel must refuse native startup')
# #1329: `feishu` is implemented, so the feishu cell moved from "must refuse"
# to the declaration invariant it now belongs to (see `BUNDLE` mutation).
BUNDLE = ('bundle_keys_match_the_assembly',
          '#1763 the assembly constructs exactly the bundle keys the loader owns')

MUTATIONS = [
    # #1763: the production call site is deleted altogether. The seam still
    # exists and its unit tests still pass, so only the real-entrypoint cells
    # can notice — that is exactly what this row pins.
    ('assembly-reject-call-removed', assembly, REJECT_BLOCK,
     '        // #1763 negative control: call site deleted\n',
     [EVERY]),
    # #1763: the error is swallowed instead of propagating, i.e. "refuse" turns
    # into the silent skip #1302 B forbids. Same cells must kill.
    ('assembly-reject-silently-skipped', assembly, REJECT_BLOCK,
     '        let rejected = crate::channel::loader::reject_unmigrated_channels(config.get("adapters"))\n'
     '            .and_then(|()| {\n'
     '                crate::channel::loader::reject_unimplemented_bundle_channels(\n'
     '                    config.get("adapters"),\n'
     '                    super::channels::IMPLEMENTED_BUNDLE_CHANNEL_KEYS,\n'
     '                )\n'
     '            });\n'
     '        // #1763 negative control: silent skip\n'
     '        let _ = rejected;\n',
     [EVERY]),
    # #1694 §7: `feishu` drops out of the bundle table while the assembly still
    # claims to construct it, so the two declarations disagree.
    ('bundle-feishu-dropped', loader,
     'pub const BUNDLE_CHANNEL_KEYS: &[&str] = &["weixin", "feishu"];',
     'pub const BUNDLE_CHANNEL_KEYS: &[&str] = &["weixin"];',
     [BUNDLE]),
]


def main():
    guarded = [assembly, loader]
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    # No `any_cell`: both assembly mutations hand `expect_err` an `Ok`, so the
    # base cell must stay red for each of them; requiring every declared cell
    # keeps a partially-weakened row from passing.
    return matrix_main(root, __file__, 'native_process::channels_tests::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
