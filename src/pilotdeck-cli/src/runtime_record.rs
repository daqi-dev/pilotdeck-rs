//! One locked, authenticated runtime locator per private installation.
use crate::update::{Error, Installation};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::Mutex,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeAddress {
    pub install_id: String,
    pub generation: String,
    pub instance_id: String,
    pub address: SocketAddr,
}
fn failure(message: impl std::fmt::Display) -> Error {
    Error::new("runtimeUnavailable", message.to_string())
}
fn open(root: &Path, create: bool) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    let file = options.open(root.join("runtime.json")).map_err(failure)?;
    let m = file.metadata().map_err(failure)?;
    if !m.is_file()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
        || m.nlink() != 1
    {
        return Err(failure("Unsafe native runtime locator."));
    }
    Ok(file)
}
fn loopback(address: SocketAddr) -> Result<SocketAddr, Error> {
    let ip = match address.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip if !ip.is_multicast() => ip,
        _ => return Err(failure("Native runtime locator is not a unicast address.")),
    };
    if address.port() == 0 {
        return Err(failure("Native runtime locator has no port."));
    }
    if ip == address.ip() {
        return Ok(address);
    }
    Ok(SocketAddr::new(ip, address.port()))
}
/// Prove that a concrete address belongs to a local interface without sending
/// a packet. The caller keeps the socket alive through the authenticated call.
pub fn hold_local_address(mut address: SocketAddr) -> Result<UdpSocket, Error> {
    if loopback(address)? != address {
        return Err(failure("Non-concrete native runtime address."));
    }
    address.set_port(0);
    UdpSocket::bind(address).map_err(|_| failure("Native runtime address is not local."))
}
pub struct RuntimeRecord {
    file: Mutex<File>,
    install_id: String,
    generation: String,
    instance_id: String,
}
impl RuntimeRecord {
    pub fn lock(installation: &Installation) -> Result<Self, Error> {
        let file = open(&installation.root, true)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| failure("Another native service owns this installation."))?;
        // Owning the locator does not make the preceding instance's address
        // live again. Publish only after the replacement listener is ready.
        file.set_len(0)
            .and_then(|()| file.sync_all())
            .map_err(failure)?;
        Ok(Self {
            file: Mutex::new(file),
            install_id: installation.policy.install_id.clone(),
            generation: installation.expected_generation.clone(),
            instance_id: uuid::Uuid::new_v4().to_string(),
        })
    }
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
    pub fn publish(&self, address: SocketAddr) -> Result<(), Error> {
        let value = RuntimeAddress {
            install_id: self.install_id.clone(),
            generation: self.generation.clone(),
            instance_id: self.instance_id.clone(),
            address: loopback(address)?,
        };
        let _local = hold_local_address(value.address)?;
        let bytes = serde_json::to_vec(&value).map_err(failure)?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| failure("Runtime locator lock failed."))?;
        file.seek(SeekFrom::Start(0))
            .and_then(|_| file.set_len(0))
            .and_then(|()| file.write_all(&bytes))
            .and_then(|()| file.sync_all())
            .map_err(failure)
    }
}
/// A stale record is never accepted: its lock must be held and its complete
/// identity must then be authenticated against the actual listening server.
pub fn discover(installation: &Installation) -> Result<RuntimeAddress, Error> {
    let mut file = open(&installation.root, false)?;
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => return Err(failure("No live native service owns this runtime locator.")),
        Err(e) if e == rustix::io::Errno::WOULDBLOCK => (),
        Err(e) => return Err(failure(e)),
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(failure)?;
    if bytes.len() > 4096 {
        return Err(failure("Native runtime locator is too large."));
    }
    if bytes.is_empty() {
        return Err(failure(
            "The native service is starting; no listener is published yet.",
        ));
    }
    let value: RuntimeAddress = serde_json::from_slice(&bytes).map_err(failure)?;
    if value.install_id != installation.policy.install_id
        || value.generation != installation.expected_generation
        || uuid::Uuid::parse_str(&value.instance_id).is_err()
        || loopback(value.address)? != value.address
    {
        return Err(failure("Native runtime locator identity mismatch."));
    }
    let _local = hold_local_address(value.address)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{Generation, InstallManifest};
    use std::collections::BTreeMap;
    #[test]
    fn live_lock_selects_exact_listener_and_stale_record_is_not_live() {
        let root = std::env::temp_dir().join(format!("pdrs-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let install = Installation {
            root: root.clone(),
            expected_generation: "generation".into(),
            policy: InstallManifest {
                schema_version: 1,
                install_id: "installation".into(),
                approved_remote: "local".into(),
                approved_ref: "refs/heads/main".into(),
            },
            current: Generation {
                schema_version: 1,
                generation: "generation".into(),
                source_sha: "0".repeat(40),
                source_checkout: root.join("source"),
                bin_hashes: BTreeMap::new(),
                library_profile: None,
                library_hashes: BTreeMap::new(),
            },
        };
        let record = RuntimeRecord::lock(&install).unwrap();
        record.publish("127.0.0.1:32123".parse().unwrap()).unwrap();
        let live = discover(&install).unwrap();
        assert_eq!(live.address, "127.0.0.1:32123".parse().unwrap());
        assert_eq!(live.instance_id, record.instance_id());
        assert!(RuntimeRecord::lock(&install).is_err());
        // An owned-but-malformed or oversized record never becomes authority.
        std::fs::write(root.join("runtime.json"), b"{").unwrap();
        assert!(discover(&install).is_err());
        std::fs::write(root.join("runtime.json"), vec![b'x'; 4097]).unwrap();
        assert!(discover(&install).is_err());
        record.publish("127.0.0.1:32123".parse().unwrap()).unwrap();
        drop(record);
        assert!(discover(&install).is_err());
        let replacement = RuntimeRecord::lock(&install).unwrap();
        assert!(std::fs::read(root.join("runtime.json")).unwrap().is_empty());
        assert!(discover(&install).unwrap_err().message.contains("starting"));
        replacement
            .publish("127.0.0.1:32124".parse().unwrap())
            .unwrap();
        let next = discover(&install).unwrap();
        assert_ne!(next.instance_id, live.instance_id);
        assert_eq!(next.address, "127.0.0.1:32124".parse().unwrap());
        drop(replacement);
        std::fs::remove_dir_all(root).unwrap();
    }
}
