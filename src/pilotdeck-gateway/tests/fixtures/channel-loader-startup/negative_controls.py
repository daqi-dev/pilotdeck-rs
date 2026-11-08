#!/usr/bin/env python3
"""#2301 production-assembly controls for the configured loader channels: the
startup wiring must stay real — `load_enabled_channels` actually called from
`assembly::start` over `production_loader_mounts`, not merely existing as a
seam.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

Sibling of `channel-unmigrated-startup` (the #1763 refusal gate at the same
call site) and of `channel-unmigrated` (the seam-level tables): this fixture
pins the #2301 terminal wiring. Deleting the startup call, emptying the
configured half, or dropping a production mount must turn
`startup_builds_the_enabled_loader_channels` /
`production_mounts_cover_every_non_rejected_loader_key` red — the silent
skip #1853 registered is the one failure mode these rows exist to keep dead.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
assembly = root / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
channels = root / 'src/pilotdeck-gateway/src/native_process/channels.rs'

# The loader-table construction call inside the Ok(host) branch.
STARTUP_CALL = (
    '            startup.extend(channel_assembly.create_startup_configured());\n'
)

# The configured half's body: the loadEnabledChannels port itself. #2301
# fix-r2 (review P0 #2): the loader's console.error closure is the same
# best-effort console write as the channel log (`eprintln!` panics on a
# failed write, which `assembly::start` calls before any channel starts).
CONFIGURED_BODY = (
    '        load_enabled_channels(\n'
    '            self.config.get("adapters"),\n'
    '            &LoadEnabledChannelsOptions { pilot_home: None },\n'
    '            &production_loader_mounts(),\n'
    '            &mut |message| console_log_line(&mut std::io::stderr(), &message),\n'
    '        )\n'
)

API_SERVER_MOUNT = '    crate::channel::api_server::register(&mut mounts);\n'

WECOM_MOUNT = (
    '    mounts.register(\n'
    '        "wecomCallback",\n'
    '        crate::channel::wecom_callback::WeComCallbackChannel::constructor(\n'
    '            crate::channel::wecom_callback::no_interaction(),\n'
    '        ),\n'
    '    );\n'
)

# The end-to-end cell: six lifecycle entries only appear when the assembly
# really constructs the configured loader channels.
STARTED = ('startup_builds_the_enabled_loader_channels',
           '#2301 all six loader channels report their lifecycle entry')
# The declaration cell: a mount dropped from the production registry leaves
# its loader key neither rejected nor mounted — the #1853 silent skip again.
COVERS_API = ('production_mounts_cover_every_non_rejected_loader_key',
              '#2301 loader key "apiServer" is neither rejected nor mounted in production')
COVERS_WECOM = ('production_mounts_cover_every_non_rejected_loader_key',
                '#2301 loader key "wecomCallback" is neither rejected nor mounted in production')

MUTATIONS = [
    # The production call site is deleted altogether. The seam, its unit
    # tests, and the mount-declaration test all still pass, so only the
    # real-entrypoint cell can notice — that is exactly what this row pins.
    ('assembly-loader-call-removed', assembly, STARTUP_CALL,
     '            // #2301 negative control: call site deleted\n',
     [STARTED]),
    # The call stays but constructs nothing — "load" turns into the silent
    # skip #1853 registered. Same cell must kill.
    ('assembly-loader-silently-skipped', channels, CONFIGURED_BODY,
     '        Vec::new()\n',
     [STARTED]),
    # The apiServer mount drops out of the production registry while the
    # loader table still lists the key.
    ('mount-registration-dropped', channels, API_SERVER_MOUNT,
     '    // #2301 negative control: apiServer mount dropped\n',
     [COVERS_API]),
    # The wecomCallback mount drops out the same way; the interaction-port
    # indirection makes this arm worth pinning separately.
    ('wecom-mount-dropped', channels, WECOM_MOUNT,
     '    // #2301 negative control: wecomCallback mount dropped\n',
     [COVERS_WECOM]),
]


def main():
    guarded = [assembly, channels]
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    # No `any_cell`: every mutation must fail its declared cell outright, and
    # requiring all declared cells keeps a partially-weakened row from passing.
    return matrix_main(root, __file__, 'native_process::channels_tests::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
