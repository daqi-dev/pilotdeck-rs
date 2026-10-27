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
META = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
PAIR = ROOT / 'src/pilotdeck-gateway/src/pairing.rs'
TEST = ROOT / 'src/pilotdeck-gateway/tests/fixtures/metadata-from-env/regression.rs'
PREFIX = 'metadata::from_env_tests::'
CWD = '''let cwd = std::env::current_dir().map_err(|error| {
            let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
            io::Error::new(error.kind(), message)
        })?;'''
NORMAL = ['relative_normal_cwd', 'tilde_normal_cwd', 'tilde_child_normal_cwd',
          'absolute_normal_cwd', 'empty_normal_cwd']
DELETED = ['relative_deleted_cwd', 'tilde_deleted_cwd', 'tilde_child_deleted_cwd',
           'absolute_deleted_cwd', 'empty_deleted_cwd']
MUTATIONS = [
    ('cwd-none', META, CWD,
     'let Ok(cwd) = std::env::current_dir() else { return Ok(None); };',
     DELETED, 'metadata from_env matrix:'),
    ('cwd-message', META, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)', DELETED, 'metadata from_env matrix:'),
    ('normal-home-layout', META, 'Ok(Some(Self {\n            home,\n            cwd,',
     'Ok(Some(Self {\n            home: home.join("mutated"),\n            cwd,',
     NORMAL, 'metadata from_env matrix:'),
    ('normal-cwd-layout', META, 'Ok(Some(Self {\n            home,\n            cwd,',
     'Ok(Some(Self {\n            home,\n            cwd: cwd.join("mutated"),',
     NORMAL, 'metadata from_env matrix:'),
    ('unset-default-home', META,
     'let Ok(home) = std::env::var("PILOT_HOME") else {\n            return Ok(None);\n        };',
     'let Ok(home) = std::env::var("PILOT_HOME") else {\n'
     '            return Ok(Some(Self { home: PathBuf::from("/default-home"), '
     'cwd: PathBuf::from("/"), files: NativeFiles }));\n        };',
     ['unset_normal_cwd', 'unset_deleted_cwd'], 'metadata from_env matrix:'),
    ('missing-home-default', META,
     'let Ok(user_home) = std::env::var("HOME") else {\n                return Ok(None);\n            };',
     'let Ok(user_home) = std::env::var("HOME") else {\n'
     '                return Ok(Some(Self { home: PathBuf::from("/default-home"), '
     'cwd, files: NativeFiles }));\n            };',
     ['missing_home_normal_cwd'], 'metadata from_env matrix:'),
    ('pairing-swallow', PAIR, 'crate::metadata::Metadata::from_env()?',
     'crate::metadata::Metadata::from_env().unwrap_or(None)',
     DELETED, 'pairing propagates cwd failure:'),
]



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=13, child_count=13,
                       paths=[META, PAIR, TEST, FIXTURE / "oracle.json", FIXTURE / "generate.mts"],
                       any_cell=(), exact=False)


if __name__ == '__main__':
    raise SystemExit(main())
