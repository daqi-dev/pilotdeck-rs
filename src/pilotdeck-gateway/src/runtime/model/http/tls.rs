//! Explicit Node CA sources shared by all HTTP clients and HTTPS proxies.
//! The first default connector snapshots the files for this process. Unlike
//! Node, this is lazy, not process startup (A6.4-TLS-CA-SOURCES-001).
//! Native OS stores are deliberately not read; Node's `--use-*-ca` startup
//! modes are observed through `NODE_OPTIONS`, the only carrier a non-Node
//! process can see.
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::OnceLock,
};

pub(super) fn client_config() -> rustls::ClientConfig {
    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    CONFIG
        .get_or_init(|| super::rooted_client_config(default_roots()))
        .clone()
}

/// Node's `--use-*-ca` startup selection. The gateway process has no Node
/// CLI, so the flags are read from `NODE_OPTIONS`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CaMode {
    /// Startup default (and `--use-system-ca`, which only adds native-store
    /// roots): every explicit env source is trusted on top of bundled roots.
    Default,
    /// `--use-openssl-ca`: OpenSSL's explicit file and hash directory sources
    /// (`ssl-unflagged` / `openssl-dir` / `openssl-plain` probes). Node
    /// replaces the bundled roots with the OpenSSL store in this mode; native
    /// stores stay out of scope here, so the bundled roots are kept and the
    /// trust set is a superset of Node's. Recorded as a residual difference in
    /// `A6.4-TLS-CA-SOURCES-001-R1`. It shares `Default`'s branch: the two only
    /// diverge once the native store is read.
    Openssl,
    /// `--use-bundled-ca`, also with `--use-system-ca`: Node stops reading
    /// `SSL_CERT_FILE` / `SSL_CERT_DIR` entirely (forced-bundled /
    /// bundled-system probes).
    Bundled,
}

fn ca_mode(options: &OsStr) -> CaMode {
    let mut mode = CaMode::Default;
    for token in options.to_string_lossy().split_whitespace() {
        match token {
            "--use-bundled-ca" => return CaMode::Bundled,
            "--use-openssl-ca" => mode = CaMode::Openssl,
            _ => {}
        }
    }
    mode
}

fn default_roots() -> rustls::RootCertStore {
    roots_from(
        ca_mode(
            std::env::var_os("NODE_OPTIONS")
                .as_deref()
                .unwrap_or_default(),
        ),
        std::env::var_os("SSL_CERT_FILE").as_deref(),
        std::env::var_os("SSL_CERT_DIR").as_deref(),
        std::env::var_os("NODE_EXTRA_CA_CERTS").as_deref(),
    )
}

fn roots_from(
    mode: CaMode,
    cert_file: Option<&OsStr>,
    cert_dir: Option<&OsStr>,
    extra: Option<&OsStr>,
) -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // The calibrated Homebrew Node 22 build trusts OpenSSL's explicit CA
    // file and hash-named directory by default; `--use-bundled-ca` (also
    // with `--use-system-ca`) is the only mode that stops reading them.
    if mode != CaMode::Bundled {
        if let Some(file) = explicit(cert_file) {
            load_pem(&mut roots, Path::new(file));
        }
        if let Some(dirs) = explicit(cert_dir) {
            // This project reads `SSL_CERT_DIR` as a path list. OpenSSL's own
            // list semantics were not measured against the baseline, so the
            // choice is recorded in `A6.4-TLS-CA-SOURCES-001-R1` residuals.
            for dir in std::env::split_paths(dirs).filter(|dir| !dir.as_os_str().is_empty()) {
                load_hash_dir(&mut roots, &dir);
            }
        }
    }
    // Like Node, `NODE_EXTRA_CA_CERTS` augments every mode.
    if let Some(path) = explicit(extra) {
        load_pem(&mut roots, Path::new(path));
    }
    roots
}

fn explicit(value: Option<&OsStr>) -> Option<&OsStr> {
    value.filter(|value| !value.is_empty())
}

fn load_pem(roots: &mut rustls::RootCertStore, path: &Path) {
    let bytes = match read_ca_file(path) {
        Some(bytes) => bytes,
        None => return,
    };
    // Like Node/OpenSSL, keep the valid prefix but stop at the first malformed
    // certificate. PEM decoding alone does not validate the X.509 DER.
    for certificate in CertificateDer::pem_slice_iter(&bytes) {
        let result = certificate
            .map_err(|error| error.to_string())
            .and_then(|cert| roots.add(cert).map_err(|error| error.to_string()));
        if let Err(error) = result {
            // #2558: a best-effort warn line, like Node's `console.error` — an
            // `eprintln!` here panics when the write fails (std behavior), and
            // this runs inside `client_config()`'s `OnceLock::get_or_init`, so
            // the panic would unwind the task of the first HTTPS model request
            // that is building its transport.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "Warning: could not load CA file {}: {error}",
                    path.display()
                ),
            );
            break;
        }
    }
}

/// A CA bundle is small, and the snapshot runs inside `OnceLock::get_or_init`:
/// a blocking read here would wedge TLS for the whole process, and an unbounded
/// one would allocate on that shared path. The bound is this project's own —
/// Node applies no equivalent limit — so a larger bundle is skipped with a
/// warning and the trust set is *smaller* than Node's (`-R1` residuals).
const MAX_CA_FILE_BYTES: u64 = 8 << 20;

/// Opens the bundle so that a FIFO cannot block, then judges and reads the same
/// handle. Checking the path and then re-reading it by name left a window where
/// a swapped-in FIFO blocked the process-wide snapshot.
fn open_ca_file(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        // `O_NONBLOCK` keeps `open` from waiting for a writer; the handle is
        // judged by `fstat` below, so a FIFO is skipped before any read.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = std::fs::File::open(path)?;
    Ok(file)
}

fn read_ca_file(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;

    let file = match open_ca_file(path) {
        Ok(file) => file,
        Err(error) => {
            // #2558: the load_pem warn line's non-panicking shape.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "Warning: could not load CA file {}: {error}",
                    path.display()
                ),
            );
            return None;
        }
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => {
            // #2558: the load_pem warn line's non-panicking shape.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "Warning: could not load CA file {}: {error}",
                    path.display()
                ),
            );
            return None;
        }
    };
    if !metadata.is_file() {
        // #2558: the load_pem warn line's non-panicking shape.
        crate::native_process::channels::console_log_line(
            &mut std::io::stderr(),
            &format!(
                "Warning: skipping CA file {}: not a regular file",
                path.display()
            ),
        );
        return None;
    }
    if metadata.len() > MAX_CA_FILE_BYTES {
        // #2558: the load_pem warn line's non-panicking shape.
        crate::native_process::channels::console_log_line(
            &mut std::io::stderr(),
            &format!(
                "Warning: skipping CA file {}: {} bytes is larger than the {} byte limit",
                path.display(),
                metadata.len(),
                MAX_CA_FILE_BYTES
            ),
        );
        return None;
    }
    let mut bytes = Vec::new();
    // The size was just bounded, so the reservation is bounded too; without it
    // `read_to_end` doubles the buffer and peaks at 2-3x the limit.
    bytes.reserve(metadata.len() as usize);
    // Bound the read too: the file may have grown since `metadata()`.
    // `saturating_add` because the size-limit negative control rewrites the
    // constant to `u64::MAX`: a plain `+ 1` would overflow (debug panic /
    // release wrap to zero) before the assertion could fail.
    match (&file)
        .take(MAX_CA_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        Ok(_) if bytes.len() as u64 <= MAX_CA_FILE_BYTES => Some(bytes),
        Ok(_) => {
            // #2558: the load_pem warn line's non-panicking shape.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "Warning: skipping CA file {}: grew past the {} byte limit while reading",
                    path.display(),
                    MAX_CA_FILE_BYTES
                ),
            );
            None
        }
        Err(error) => {
            // #2558: the load_pem warn line's non-panicking shape.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "Warning: could not load CA file {}: {error}",
                    path.display()
                ),
            );
            None
        }
    }
}

/// OpenSSL's `X509_LOOKUP_hash_dir` layout: only entries named
/// `<subject hash>.<index>` (eight lowercase hex digits, then one index digit
/// — `X509_LOOKUP_hash_dir` probes `.0`..`.9`, as written by `c rehash`) are
/// certificates; a plain PEM directory is not an OpenSSL CA source.
fn load_hash_dir(roots: &mut rustls::RootCertStore, dir: &Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            // #2558: the load_pem warn line's non-panicking shape.
            crate::native_process::channels::console_log_line(
                &mut std::io::stderr(),
                &format!(
                    "Warning: could not read CA directory {}: {error}",
                    dir.display()
                ),
            );
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.file_name().is_some_and(is_hash_entry))
        .collect();
    paths.sort();
    for path in paths {
        load_pem(roots, &path);
    }
}

fn is_hash_entry(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some((hash, index)) = name.split_once('.') else {
        return false;
    };
    hash.len() == 8
        && hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        && index.len() == 1
        && index.bytes().all(|byte| byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Self-signed throwaway CA (`openssl req -x509`, CN=pdrs-1552-test-ca);
    /// not in the bundled Mozilla set, so counting roots is unambiguous.
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIDKTCCAhGgAwIBAgIUYF4Fioulvmvr6KxKLhVNg/PbhQgwDQYJKoZIhvcNAQEL
BQAwHDEaMBgGA1UEAwwRcGRycy0xNTUyLXRlc3QtY2EwHhcNMjYxMTA1MTQyNTIw
WhcNMzYxMTAyMTQyNTIwWjAcMRowGAYDVQQDDBFwZHJzLTE1NTItdGVzdC1jYTCC
ASIwDQYJKoZIhvcNAQEBBQADggEPADCCAQoCggEBAOctaSIb9b7887RS9DvSowBA
Eg0wMnLZAcqnk4Z6cFAWVLwGud6mClkppOWwYtpPpfulRukmumn1FClLlhYi00QB
+nwT0dNhrHAKfsw+s6fUcL/gIU59ZWD54vlqikuuU8xmfqzHIzmEHTfr5a3ITe5+
nF0TfBGC4zaaSc4095hC/s9NgdR4uJf/divxqOKsUMF0tUFzJRW1VzwVYMe89maO
0rVWBd6Wympgx3wVqdvuTkW+RjcQfZI1ok9He9HfTKcguf96kVjRSBBtQVm1HeMU
2azSO6LqD8uQKv6M7aPN1OshjrAzQUWNUYm5cm5XwkpfdY4xo3cTZ22gWTjpedMC
AwEAAaNjMGEwHQYDVR0OBBYEFGM3HDki+RBQSjkzHrdZ9uERCGrLMB8GA1UdIwQY
MBaAFGM3HDki+RBQSjkzHrdZ9uERCGrLMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0P
AQH/BAQDAgEGMA0GCSqGSIb3DQEBCwUAA4IBAQBUgq/uel5YWTVrtSBsKU9rec+F
FuMc2iQyUh+aXCGd0S5xkLvFSnfNYe8u7QrqvDNXKzEqDb2LYvpQ4i3j0262sBjB
I+Ya/QuzfDDruC751RfsEr6fRxkO4KQtHuHB5A/If8qLiMiJNwh+eFho3GJxWnZ5
zEws2PaHzf5f/WCGxqpdTzA82/Z1bOEGdjO+zV4eZBah6H8WarFSBQGzxypJQNzX
Q5e9cARiJDOePujJEsFxXAIOUudccPAufGDHYSLNpbLyVw60TMcCQUnNloXWjnfY
jenPvKuS2DWn64nN+FQXsa0/wFoUqRVU0Mb2oFPws0U/XhR5wq9qRQYM/0h8
-----END CERTIFICATE-----
";

    fn write_cert(path: &Path) {
        fs::write(path, TEST_CA_PEM).expect("#1552 write test CA");
    }

    #[test]
    fn bundled_roots_are_retained() {
        let roots = super::default_roots();
        for anchor in webpki_roots::TLS_SERVER_ROOTS {
            assert!(
                roots.roots.contains(anchor),
                "extra CA loading removed a bundled anchor"
            );
        }
    }

    #[test]
    fn ssl_cert_dir_hash_entries_become_trusted() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-hash-dir");
        let dir = scratch.path().join("cert-dir");
        fs::create_dir(&dir).expect("#1552 create hash dir");
        write_cert(&dir.join("3b8f0bda.0"));
        let roots = roots_from(CaMode::Default, None, Some(dir.as_os_str()), None);
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1,
            "#1552 default mode trusts a hash-named SSL_CERT_DIR entry"
        );
        let roots = roots_from(CaMode::Openssl, None, Some(dir.as_os_str()), None);
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1,
            "#1552 openssl mode trusts a hash-named SSL_CERT_DIR entry"
        );
    }

    #[test]
    fn plain_pem_directory_is_not_a_ca_source() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-plain-dir");
        let dir = scratch.path().join("plain-dir");
        fs::create_dir(&dir).expect("#1552 create plain dir");
        write_cert(&dir.join("ca.pem"));
        for mode in [CaMode::Default, CaMode::Openssl] {
            let roots = roots_from(mode, None, Some(dir.as_os_str()), None);
            crate::negative_assert_eq!(
                roots.roots.len(),
                webpki_roots::TLS_SERVER_ROOTS.len(),
                "#1552 plain PEM directory entries are not trusted"
            );
        }
    }

    #[test]
    fn bundled_mode_ignores_explicit_sources() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-bundled");
        let file = scratch.path().join("ca.pem");
        write_cert(&file);
        let dir = scratch.path().join("cert-dir");
        fs::create_dir(&dir).expect("#1552 create hash dir");
        write_cert(&dir.join("3b8f0bda.0"));
        let roots = roots_from(
            CaMode::Bundled,
            Some(file.as_os_str()),
            Some(dir.as_os_str()),
            None,
        );
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len(),
            "#1552 bundled mode ignores SSL_CERT_FILE and SSL_CERT_DIR"
        );
    }

    #[test]
    fn extra_ca_certs_still_apply_in_bundled_mode() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-extra-bundled");
        let extra = scratch.path().join("extra.pem");
        write_cert(&extra);
        let roots = roots_from(CaMode::Bundled, None, None, Some(extra.as_os_str()));
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1,
            "#1552 NODE_EXTRA_CA_CERTS still applies in bundled mode"
        );
    }

    #[test]
    fn ssl_cert_file_is_trusted_by_default() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-file");
        let file = scratch.path().join("ca.pem");
        write_cert(&file);
        let roots = roots_from(CaMode::Default, Some(file.as_os_str()), None, None);
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1,
            "#1552 default mode trusts SSL_CERT_FILE"
        );
    }

    #[test]
    fn ca_modes_from_node_options() {
        crate::negative_assert_eq!(
            ca_mode(OsStr::new("")),
            CaMode::Default,
            "#1552 no flags keeps the default mode"
        );
        crate::negative_assert_eq!(
            ca_mode(OsStr::new("--use-system-ca")),
            CaMode::Default,
            "#1552 --use-system-ca keeps explicit sources"
        );
        crate::negative_assert_eq!(
            ca_mode(OsStr::new("--max-old-space-size=4096 --use-openssl-ca")),
            CaMode::Openssl,
            "#1552 --use-openssl-ca selects the openssl mode"
        );
        crate::negative_assert_eq!(
            ca_mode(OsStr::new("--use-bundled-ca")),
            CaMode::Bundled,
            "#1552 --use-bundled-ca selects the bundled mode"
        );
        crate::negative_assert_eq!(
            ca_mode(OsStr::new("--use-bundled-ca --use-system-ca")),
            CaMode::Bundled,
            "#1552 --use-bundled-ca also wins with --use-system-ca"
        );
        crate::negative_assert_eq!(
            ca_mode(OsStr::new("--use-openssl-ca --use-bundled-ca")),
            CaMode::Bundled,
            "#1552 --use-bundled-ca wins over --use-openssl-ca"
        );
    }

    #[test]
    fn hash_entry_names() {
        for name in ["3b8f0bda.0", "00000000.9", "ffffffff.0"] {
            crate::negative_assert!(
                is_hash_entry(OsStr::new(name)),
                "#1552 hash entry name accepted: {name}"
            );
        }
        for name in [
            "ca.pem",
            "cert.pem",
            "3B8F0BDA.0",
            "3b8f0bd.0",
            "3b8f0bdaa.0",
            "3b8f0bda.",
            "3b8f0bda",
            "3b8f0bda.a",
            "3b8f0bda.0.pem",
            "3b8f0bda.0x",
            "3b8f0bda.-1",
            "3b8f0bda.10",
            "00000000.12",
            ".0",
            "readme",
            "3b8f0bda.0 ",
        ] {
            crate::negative_assert!(
                !is_hash_entry(OsStr::new(name)),
                "#1552 non-hash entry name rejected: {name}"
            );
        }
    }

    #[test]
    fn ssl_cert_dir_accepts_an_openssl_list() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-dir-list");
        let first = scratch.path().join("first-dir");
        let second = scratch.path().join("second-dir");
        fs::create_dir(&first).expect("#1552 create first hash dir");
        fs::create_dir(&second).expect("#1552 create second hash dir");
        write_cert(&second.join("3b8f0bda.0"));
        let list = std::env::join_paths([first.as_os_str(), second.as_os_str()])
            .expect("#1552 join a two-entry SSL_CERT_DIR");
        let roots = roots_from(CaMode::Default, None, Some(list.as_os_str()), None);
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len() + 1,
            "#1552 SSL_CERT_DIR is read as a list of hash directories"
        );
    }

    #[test]
    fn non_regular_hash_entries_are_skipped() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-hash-dir-entry");
        let dir = scratch.path().join("cert-dir");
        fs::create_dir(&dir).expect("#1552 create hash dir");
        fs::create_dir(dir.join("3b8f0bda.0")).expect("#1552 hash-named directory");
        let roots = roots_from(CaMode::Default, None, Some(dir.as_os_str()), None);
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len(),
            "#1552 hash-named non-regular entries are skipped"
        );
    }

    #[test]
    fn oversized_ca_files_are_skipped() {
        let scratch = crate::test_scratch::Scratch::new("tls-ca-oversized");
        let bundle = scratch.path().join("bundle.pem");
        let mut bytes = TEST_CA_PEM.as_bytes().to_vec();
        // A valid certificate followed by filler: without the size limit the
        // loader would trust it, so the control below is killable. The size is
        // deliberately not derived from `MAX_CA_FILE_BYTES` — the size-limit
        // negative control rewrites that constant, and a derived size would
        // overflow instead of failing the assertion.
        const TEST_OVERSIZED_BYTES: u64 = (8 << 20) + 1;
        bytes.resize(TEST_OVERSIZED_BYTES as usize, b'#');
        fs::write(&bundle, &bytes).expect("#1552 write an oversized CA bundle");
        let roots = roots_from(CaMode::Default, Some(bundle.as_os_str()), None, None);
        crate::negative_assert_eq!(
            roots.roots.len(),
            webpki_roots::TLS_SERVER_ROOTS.len(),
            "#1552 CA bundles above the size limit are skipped"
        );
    }
}
