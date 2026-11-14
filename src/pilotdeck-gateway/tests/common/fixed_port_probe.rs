//! #2461 fix-r6: the one connect-probe judgment every fixed-port criterion in
//! this crate shares. Rounds r4→r5 hardened the five points a diff happened to
//! touch; the r6 review then found four more same-shaped points the diffs had
//! passed by (the slack replay guard, the head_timeout/native_ws stop-and-drain
//! probes, the sms dual-stack probe), so the judgment is being converged here
//! instead of being re-derived per site.
//!
//! The rule, measured against Node v22.22.0 (`net.createConnection` to a
//! closed 127.0.0.1 port surfaces `code: 'ECONNREFUSED', errno: -61`): only a
//! refusal answers the question the caller asked — is the port free, closed,
//! or lacking the capability. An accepted connect is a live listener, and
//! every other error kind (EMFILE, permission faults, a probe that never
//! answered) is an unknown that must surface as a named failure, never as a
//! pass. `native_process::channels_tests` grew this shape first (#2462); this
//! module is that judgment lifted out, generic over the accepted payload so
//! `raw_request` can keep its stream.
//!
//! Tests-only by construction: no product module may depend on this file.

use std::io;
use std::time::Duration;

/// What a connect probe proved. `Accepted` keeps its payload — the sms
/// `raw_request` path needs the stream back when the listener is alive.
pub(crate) enum FixedPortProbe<T> {
    Refused,
    Accepted(T),
    Unknown(io::Error),
}

/// The judgment itself: a pure function so tests can drive it with
/// constructed errors without needing a real port.
pub(crate) fn classify_fixed_port_probe<T>(probe: Result<T, io::Error>) -> FixedPortProbe<T> {
    match probe {
        Ok(value) => FixedPortProbe::Accepted(value),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            FixedPortProbe::Refused
        }
        Err(error) => FixedPortProbe::Unknown(error),
    }
}

/// A bounded connect, fail-closed like the `bounded` helpers it replaces: a
/// probe that never answers is an unknown, not a pass.
pub(crate) async fn probe_fixed_port<A: tokio::net::ToSocketAddrs>(
    label: &str,
    addr: A,
    bound: Duration,
) -> FixedPortProbe<tokio::net::TcpStream> {
    match tokio::time::timeout(bound, tokio::net::TcpStream::connect(addr)).await {
        Ok(result) => classify_fixed_port_probe(result),
        Err(_elapsed) => FixedPortProbe::Unknown(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("the {label} did not answer inside {bound:?}"),
        )),
    }
}

/// The unified failure text, `fixed_port_close_failure`'s caliber: only a
/// refusal is `None`. Callers that must keep a site-specific message (the sms
/// v4 guard's `#1339 port {port} already in use`, the `raw_request` error
/// mapping) match on the enum themselves and still route through
/// `classify_fixed_port_probe` — the judgment stays shared, the wording stays
/// anchored where a test or a negative control pins it.
pub(crate) fn fixed_port_probe_failure<T>(
    tag: &str,
    port: u16,
    claim: &str,
    probe: &FixedPortProbe<T>,
) -> Option<String> {
    match probe {
        FixedPortProbe::Refused => None,
        FixedPortProbe::Accepted(_) => Some(format!(
            "{tag} the fixed-port listener on port {port} is still accepting \
             connections, so it is not {claim}"
        )),
        FixedPortProbe::Unknown(error) => Some(format!(
            "{tag} the probe on port {port} answered {error} (kind {:?}, raw os error {:?}) — \
             only ConnectionRefused proves a port {claim}, so this is a named failure, not a pass",
            error.kind(),
            error.raw_os_error()
        )),
    }
}
