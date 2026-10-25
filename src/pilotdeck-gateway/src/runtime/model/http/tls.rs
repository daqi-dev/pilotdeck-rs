//! Explicit Node CA sources shared by all HTTP clients and HTTPS proxies.
//! The first default connector snapshots the files for this process. Unlike
//! Node, this is lazy, not process startup (A6.4-TLS-CA-SOURCES-001).
//! Native OS stores and Node CLI/NODE_OPTIONS flags are deliberately not read.
use rustls::pki_types::{CertificateDer, pem::PemObject};
use std::{path::Path, sync::OnceLock};

pub(super) fn client_config() -> rustls::ClientConfig {
    static CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();
    CONFIG
        .get_or_init(|| super::rooted_client_config(default_roots()))
        .clone()
}

fn default_roots() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // The calibrated Homebrew Node 22 build also uses OpenSSL's
    // explicit CA file by default. Do not guess native store paths or
    // treat every file in an OpenSSL hash directory as a trusted CA.
    if let Some(path) = std::env::var_os("SSL_CERT_FILE").filter(|p| !p.is_empty()) {
        load_pem(&mut roots, Path::new(&path));
    }
    if let Some(path) = std::env::var_os("NODE_EXTRA_CA_CERTS").filter(|p| !p.is_empty()) {
        load_pem(&mut roots, Path::new(&path));
    }
    roots
}

fn load_pem(roots: &mut rustls::RootCertStore, path: &Path) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "Warning: could not load CA file {}: {error}",
                path.display()
            );
            return;
        }
    };
    // Like Node/OpenSSL, keep the valid prefix but stop at the first malformed
    // certificate. PEM decoding alone does not validate the X.509 DER.
    for certificate in CertificateDer::pem_slice_iter(&bytes) {
        let result = certificate
            .map_err(|error| error.to_string())
            .and_then(|cert| roots.add(cert).map_err(|error| error.to_string()));
        if let Err(error) = result {
            eprintln!(
                "Warning: could not load CA file {}: {error}",
                path.display()
            );
            break;
        }
    }
}

#[cfg(test)]
mod tests {
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
}
