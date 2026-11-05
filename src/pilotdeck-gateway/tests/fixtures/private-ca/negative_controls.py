#!/usr/bin/env python3
"""#1552 TLS CA source controls. --check is read-only; commit before mutations.

Each selected mutant of the shared CA-source loader is compiled, run, restored
and accounted for by the shared matrix executor. Use the caller's
CARGO_TARGET_DIR; never create a shadow repo. The full suite is a coordinator
job after applying/committing the delivery (the sandbox cannot commit).
"""
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(fixtures))
from negative_suite import matrix_main

tls = root / 'src/pilotdeck-gateway/src/runtime/model/http/tls.rs'
scope = 'runtime::model::http::tls::tests::'
# Literal six-field rows are also consumed by anchor_selfcheck.py.
cases = [
    ('hash-dir-skipped', tls, '        if let Some(dirs) = explicit(cert_dir) {\n            // OpenSSL reads `SSL_CERT_DIR` as a list of hash directories.\n            for dir in std::env::split_paths(dirs).filter(|dir| !dir.as_os_str().is_empty()) {\n                load_hash_dir(&mut roots, &dir);\n            }\n        }', '        if let Some(dirs) = explicit(cert_dir) {\n            let _ = dirs;\n        }', scope+'ssl_cert_dir_hash_entries_become_trusted', '#1552 default mode trusts a hash-named SSL_CERT_DIR entry'),
    ('plain-dir-trusted', tls, '        .filter(|path| path.file_name().is_some_and(is_hash_entry))', '        .filter(|_| true)', scope+'plain_pem_directory_is_not_a_ca_source', '#1552 plain PEM directory entries are not trusted'),
    ('bundled-mode-still-reads-sources', tls, '    if mode != CaMode::Bundled {', '    if true {', scope+'bundled_mode_ignores_explicit_sources', '#1552 bundled mode ignores SSL_CERT_FILE and SSL_CERT_DIR'),
    ('openssl-mode-treated-as-bundled', tls, '            "--use-openssl-ca" => mode = CaMode::Openssl,', '            "--use-openssl-ca" => mode = CaMode::Bundled,', scope+'ca_modes_from_node_options', '#1552 --use-openssl-ca selects the openssl mode'),
    ('bundled-flag-ignored', tls, '            "--use-bundled-ca" => return CaMode::Bundled,', '            "--use-bundled-ca" => {}', scope+'ca_modes_from_node_options', '#1552 --use-bundled-ca selects the bundled mode'),
    ('hash-name-length-relaxed', tls, '    hash.len() == 8', '    true', scope+'hash_entry_names', '#1552 non-hash entry name rejected: 3b8f0bd.0'),
    ('ssl-cert-file-ignored', tls, '        if let Some(file) = explicit(cert_file) {', '        if let Some(file) = explicit(cert_file).filter(|_| false) {', scope+'ssl_cert_file_is_trusted_by_default', '#1552 default mode trusts SSL_CERT_FILE'),
    ('ssl-cert-dir-list-truncated', tls, '            for dir in std::env::split_paths(dirs).filter(|dir| !dir.as_os_str().is_empty()) {', '            for dir in std::env::split_paths(dirs).filter(|dir| !dir.as_os_str().is_empty()).take(1) {', scope+'ssl_cert_dir_accepts_an_openssl_list', '#1552 SSL_CERT_DIR is read as a list of hash directories'),
    ('non-regular-hash-entry-trusted', tls, '        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_CA_FILE_BYTES => {}', '        Ok(metadata) if metadata.len() <= MAX_CA_FILE_BYTES => {}', scope+'non_regular_hash_entries_are_skipped', '#1552 hash-named non-regular entries are skipped'),
    ('extra-skipped-in-bundled-mode', tls, '    if let Some(path) = explicit(extra) {', '    if let Some(path) = explicit(extra).filter(|_| mode == CaMode::Default) {', scope+'extra_ca_certs_still_apply_in_bundled_mode', '#1552 NODE_EXTRA_CA_CERTS still applies in bundled mode'),
]
if __name__ == '__main__':
    raise SystemExit(matrix_main(root, __file__, '', cases, passed_count=1,
        child_count=0, paths=[tls,
        root / 'src/pilotdeck-gateway/tests/private_ca.rs'], exact=True))
