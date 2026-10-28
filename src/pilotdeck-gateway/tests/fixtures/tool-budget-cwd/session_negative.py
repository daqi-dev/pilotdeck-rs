#!/usr/bin/env python3
"""#946 HEAD-only nonce receipt controls; account for every selected mutant.

--check validates anchors. --compile-only compiles every selected mutant.
Provide a new output directory and optional mutation names; CARGO_TARGET_DIR
must be supplied. Commit first: the source guard is never bypassed.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# negative_suite owns mutating_sources, run_owned and wait_for_slot.
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
PREFIX = 'runtime::turn::host_factory::tests::'
TEST = 'session_composition_uv_cwd_error_reaches_stream_unwrapped'
SESSION_KIND = '.map_err(|error| HostError::new(HostErrorKind::Session, error.message))?'
# Session composition now fails in prepare_session, before turn event polling.
SESSION_BRANCH = 'Err(error) if error.kind == HostErrorKind::Session => Err(RuntimeError::new(error.message)),'
MUTATIONS = [
    ('session-kind', FACTORY, SESSION_KIND,
     '.map_err(|error| HostError::new(HostErrorKind::Protocol, error.message))?',
     [(TEST, '#885 composition failure must preserve the TS uv_cwd message on the first poll')]),
    ('live-prefix', LIVE, SESSION_BRANCH,
     'Err(error) if error.kind == HostErrorKind::Session => Err(map_host_error(error)),',
     [(TEST, '#885 composition failure must preserve the TS uv_cwd message on the first poll')]),
]



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=1, child_count=1,
                       paths=[FACTORY, LIVE, ROOT / "src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs", FIXTURE / "oracle.json"],
                       any_cell=(), exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
