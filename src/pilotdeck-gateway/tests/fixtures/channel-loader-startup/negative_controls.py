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

#2426/#2427/#2428: the per-key mount wiring joined this table, so the prefix
widened from `native_process::channels_tests::` to `''` with every cell a
full module path — same widening precedent as `channel-bluebubbles`
(`channel::`). A single prefix cannot express cells that live in three
modules (`native_process::channels_tests`, `channel::wecom_callback`,
`channel::api_server`, `config`), and a second fixture would split one
concern (the loader's per-key option forwarding) across two registries. The
new rows pin the three #1853-shaped skips the review found: a mount that
forwards the wrong source field (#2426 corpId), a snapshot key list that
silently drops a loader key (#2427), and an apiKey passthrough whose removal
turns auth off (#2428 fail-open).
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
assembly = root / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
channels = root / 'src/pilotdeck-gateway/src/native_process/channels.rs'
loader = root / 'src/pilotdeck-gateway/src/channel/loader.rs'
snapshot = root / 'src/pilotdeck-gateway/src/config/snapshot.rs'

# The loader-table construction call inside the Ok(host) branch.
STARTUP_CALL = (
    '            startup.extend(channel_assembly.create_startup_configured());\n'
)

# The configured half's body: the loadEnabledChannels port itself. #2301
# fix-r2 (review P0 #2): the loader's console.error closure is the same
# best-effort console write as the channel log (`eprintln!` panics on a
# failed write, which `assembly::start` calls before any channel starts).
CONFIGURED_BODY = '    fn create_configured(&self, config: &JsValue) -> Vec<Arc<dyn ChannelAdapter>> {\n        let channels = load_enabled_channels(\n            config.get("adapters"),\n            &LoadEnabledChannelsOptions {\n                pilot_home: Some(self.pilot_home.to_string_lossy().into_owned().into()),\n            },\n            &self.configured_mounts(),\n            &mut |message| console_log_line(&mut std::io::stderr(), &message),\n        );\n        let mut managed = self.managed_keys.lock().expect("channel managed keys");\n        for channel in &channels {\n            if !managed.contains(channel.channel_key()) {\n                managed.push(channel.channel_key().clone());\n            }\n        }\n        channels\n    }\n'

API_SERVER_MOUNT = '    crate::channel::api_server::register(&mut mounts);\n'

WECOM_MOUNT = (
    '    mounts.register(\n'
    '        "wecomCallback",\n'
    '        match test_origin {\n'
    '            Some(origin) => {\n'
    '                crate::channel::wecom_callback::WeComCallbackChannel::constructor_with_base(\n'
    '                    responder.port(),\n'
    '                    format!("{origin}/cgi-bin"),\n'
    '                )\n'
    '            }\n'
    '            None => {\n'
    '                crate::channel::wecom_callback::WeComCallbackChannel::constructor(responder.port())\n'
    '            }\n'
    '        },\n'
    '    );\n'
)

# #2426: the wecomCallback arm's first extra-field forward. Pointing corpId
# at a different source field is the silent #1853 skip in its narrowest form:
# the channel still constructs, only with the wrong (absent) corp id.
WECOM_CORP_ID = (
    '            insert(&mut built, "corpId", extra_field(cfg, "corpId"));\n'
)

# #2427: the snapshot key list's wecomCallback entry — the same drift a
# seventh loader key produces when neither list derives from the other.
SNAPSHOT_WECOM_CALLBACK = '    "wecomCallback",\n'

# #2428: the apiServer arm's apiKey forward. Dropping it to undefined makes
# `resolve` fall through to the env/"" fallback — auth silently off.
API_KEY_FORWARD = (
    '            insert(&mut built, "apiKey", field(cfg, "apiKey"));\n'
)

# The end-to-end cell: six lifecycle entries only appear when the assembly
# really constructs the configured loader channels.
STARTED = ('native_process::channels_tests::startup_builds_the_enabled_loader_channels',
           '#2301 all six loader channels report their lifecycle entry')
# The declaration cell: a mount dropped from the production registry leaves
# its loader key neither rejected nor mounted — the #1853 silent skip again.
COVERS_API = ('native_process::channels_tests::production_mounts_cover_every_non_rejected_loader_key',
              '#2301 loader key "apiServer" is neither rejected nor mounted in production')
COVERS_WECOM = ('native_process::channels_tests::production_mounts_cover_every_non_rejected_loader_key',
                '#2301 loader key "wecomCallback" is neither rejected nor mounted in production')
# #2426: the mount-level cell — the captured options must carry the fixture's
# corpId, item by item.
WECOM_MOUNT_CELL = ('channel::wecom_callback::tests::loader_mount_builds_wecom_callback_channel',
                    '#2426 loader mount maps corpId through to the channel options')
# #2427: the snapshot cell — every CHANNEL_LOADER_KEYS entry must survive the
# 16-key whitelist into config.adapters.
SNAPSHOT_CELL = ('config::tests::snapshot_adapters_cover_every_channel_loader_key',
                 '#2427 the config snapshot carries loader key wecomCallback into config.adapters')
# #2428: the mount auth cell — with the passthrough dropped, the request that
# must answer 401 answers 200 instead (fail-open).
API_KEY_CELL = ('channel::api_server::tests::loader_mount_builds_api_server_channel',
                '#2428 a request without the Bearer key is rejected')

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
     '    fn create_configured(&self, _config: &JsValue) -> Vec<Arc<dyn ChannelAdapter>> { Vec::new() }\n\n',
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
    # #2426: the corpId forward reads a different source field; the channel
    # still loads, only with the wrong options — exactly the silent skip the
    # mount-level item-by-item assertions exist to keep dead.
    ('wecom-mount-corp-id-misfielded', loader, WECOM_CORP_ID,
     '            insert(&mut built, "corpId", extra_field(cfg, "agentId"));\n',
     [WECOM_MOUNT_CELL]),
    # #2427: the snapshot key list drifts from CHANNEL_LOADER_KEYS — the
    # adapter object silently loses the loader key, and no test but the
    # seam-level one notices.
    ('snapshot-loader-key-typo', snapshot, SNAPSHOT_WECOM_CALLBACK,
     '    "wecomCallbackTypo",\n',
     [SNAPSHOT_CELL]),
    # #2428: the apiKey forward is dropped; auth falls open and the
    # no-Bearer request that must answer 401 answers 200.
    ('api-key-passthrough-dropped', loader, API_KEY_FORWARD,
     '            insert(&mut built, "apiKey", undefined());\n',
     [API_KEY_CELL]),
]


def main():
    guarded = [assembly, channels, loader, snapshot]
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    # No `any_cell`: every mutation must fail its declared cell outright, and
    # requiring all declared cells keeps a partially-weakened row from passing.
    # Prefix '': every cell is a full module path (see the docstring).
    return matrix_main(root, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
