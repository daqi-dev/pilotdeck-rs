//! Byte ring and optional ordered, best-effort TS-compatible spill log.
use super::OutputSlice;
use pilotdeck_protocol::JsValue;
use std::{
    collections::VecDeque,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};
use tokio::sync::watch;

struct Spill {
    path: PathBuf,
    // Some(ready) means the preceding write finished, including failed writes.
    tail: watch::Receiver<Option<bool>>,
    #[cfg(test)]
    write_error: bool,
}

impl Spill {
    fn append(&mut self, chunk: Vec<u8>) {
        let mut previous = self.tail.clone();
        let (done, tail) = watch::channel(None);
        self.tail = tail;
        let path = self.path.clone();
        #[cfg(test)]
        let write_error = self.write_error;
        // Own accepted writes independently of the caller and keep their order.
        drop(tokio::spawn(async move {
            let ready = previous
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|v| *v)
                .unwrap_or(false);
            let ready = tokio::task::spawn_blocking(move || {
                #[cfg(test)]
                if write_error {
                    return false;
                }
                let write = || -> std::io::Result<()> {
                    if !ready {
                        fs::create_dir_all(path.parent().expect("spill parent"))?;
                    }
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)?
                        .write_all(&chunk)
                };
                write().is_ok()
            })
            .await
            .unwrap_or(false);
            done.send_replace(Some(ready));
        }));
    }
}

/// `TaskOutputStore`: complete append-only disk log plus an in-memory byte ring.
pub(super) struct OutputStore {
    chunks: VecDeque<Vec<u8>>,
    memory: usize,
    pub(super) total: u64,
    dropped: u64,
    max: usize,
    closed: bool,
    spill: Option<Spill>,
}

impl OutputStore {
    #[cfg(test)]
    pub(super) fn new(max: usize) -> Self {
        Self::with_spill(max, None)
    }
    pub(super) fn with_spill(max: usize, path: Option<PathBuf>) -> Self {
        Self {
            chunks: VecDeque::new(),
            memory: 0,
            total: 0,
            dropped: 0,
            max,
            closed: false,
            spill: path.map(|path| Spill {
                path,
                tail: watch::channel(Some(false)).1,
                #[cfg(test)]
                write_error: false,
            }),
        }
    }
    pub(super) fn append(&mut self, chunk: &[u8]) {
        if self.closed || chunk.is_empty() {
            return;
        }
        self.total += chunk.len() as u64;
        if let Some(spill) = &mut self.spill {
            spill.append(chunk.to_vec());
        }
        self.chunks.push_back(chunk.to_vec());
        self.memory += chunk.len();
        while self.memory > self.max {
            let overflow = self.memory - self.max;
            let Some(oldest) = self.chunks.front_mut() else {
                break;
            };
            if oldest.len() <= overflow {
                let length = oldest.len();
                self.chunks.pop_front();
                self.memory -= length;
                self.dropped += length as u64;
            } else {
                oldest.drain(..overflow);
                self.memory -= overflow;
                self.dropped += overflow as u64;
            }
        }
    }
    /// `readSlice(offset, maxBytes)`: `offset` is used only when it is a
    /// finite number; a `maxBytes` that is not a finite number (after `??
    /// Infinity`) reads up to the ring size.
    pub(super) fn read_slice(&self, offset: &JsValue, max_bytes: &JsValue) -> OutputSlice {
        let head = self.total as f64;
        let mut cursor = match offset {
            JsValue::Number(n) if n.is_finite() => n.floor().max(0.0),
            _ => 0.0,
        };
        let mut remaining = match max_bytes {
            JsValue::Number(n) if n.is_finite() => n.floor().max(0.0),
            _ => self.max as f64,
        };
        let mut truncated = false;
        let slice = |content: Vec<u8>, next_offset: f64, truncated: bool| OutputSlice {
            content: String::from_utf8_lossy(&content).into_owned(),
            next_offset,
            total_bytes: head,
            truncated,
        };
        let mut content = Vec::new();
        if cursor < self.dropped as f64
            && remaining > 0.0
            && let Some(disk) = self.read_disk(cursor as u64, remaining as u64)
        {
            cursor += disk.len() as f64;
            remaining -= disk.len() as f64;
            content = disk;
        }
        if remaining <= 0.0 {
            return slice(content, cursor, truncated);
        }
        let dropped = self.dropped as f64;
        if cursor < dropped {
            truncated = true;
            cursor = dropped;
        }
        if cursor >= head {
            return slice(content, cursor.min(head), truncated);
        }
        let wanted = (head - cursor).min(remaining);
        remaining = wanted;
        let mut skip = (cursor - dropped) as usize;
        let disk_bytes = content.len();
        for chunk in &self.chunks {
            if remaining <= 0.0 {
                break;
            }
            if skip >= chunk.len() {
                skip -= chunk.len();
                continue;
            }
            let end = chunk.len().min(skip + remaining as usize);
            content.extend_from_slice(&chunk[skip..end]);
            remaining -= (end - skip) as f64;
            skip = 0;
        }
        let collected = (content.len() - disk_bytes) as f64;
        slice(content, cursor + collected, truncated)
    }
    /// Freeze accepted writes before snapshotting the tail, like TS close().
    pub(super) fn begin_close(&mut self) -> impl Future<Output = ()> + Send + use<> {
        self.closed = true;
        self.flush()
    }
    /// After flushing close's accepted writes, release the ring; totals stay.
    pub(super) fn close(&mut self) {
        self.closed = true;
        self.chunks.clear();
        self.memory = 0;
    }
}

impl OutputStore {
    fn read_disk(&self, offset: u64, max: u64) -> Option<Vec<u8>> {
        let path = &self.spill.as_ref()?.path;
        let size = fs::metadata(path).ok()?.len().min(self.total);
        let wanted = size.saturating_sub(offset).min(max);
        if wanted == 0 {
            return None;
        }
        let mut file = File::open(path).ok()?;
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut bytes = vec![0; usize::try_from(wanted).ok()?];
        let count = file.read(&mut bytes).ok()?;
        if count == 0 {
            return None;
        }
        bytes.truncate(count);
        Some(bytes)
    }
    /// Snapshot the accepted write chain; waiting never holds the entry lock.
    pub(super) fn flush(&self) -> impl Future<Output = ()> + Send + use<> {
        let mut tail = self.spill.as_ref().map(|spill| spill.tail.clone());
        async move {
            if let Some(tail) = &mut tail {
                let _ = tail.wait_for(Option::is_some).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_ring_drops_oldest_bytes_and_reports_the_gap() {
        let mut store = OutputStore::new(4);
        store.append(b"abc");
        store.append(b"def");
        assert_eq!((store.total, store.dropped, store.memory), (6, 2, 4));
        let slice = store.read_slice(&JsValue::Number(0.0), &JsValue::Undefined);
        assert_eq!(
            (slice.content.as_str(), slice.next_offset, slice.truncated),
            ("cdef", 6.0, true)
        );
        let slice = store.read_slice(&JsValue::Number(3.5), &JsValue::Number(2.0));
        assert_eq!(
            (slice.content.as_str(), slice.next_offset, slice.truncated),
            ("de", 5.0, false)
        );
        let slice = store.read_slice(&JsValue::from("3"), &JsValue::Number(0.0));
        assert_eq!((slice.content.as_str(), slice.next_offset), ("", 0.0));
        store.close();
        store.append(b"x");
        let slice = store.read_slice(&JsValue::Number(0.0), &JsValue::Undefined);
        assert_eq!(
            (slice.content.as_str(), slice.next_offset, slice.total_bytes),
            ("", 2.0, 6.0)
        );
    }
}

#[cfg(test)]
mod oracle_tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0.join("output"), fs::Permissions::from_mode(0o700));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spill_replays_real_ts_oracle() {
        tokio::time::timeout(std::time::Duration::from_secs(30), replay())
            .await
            .unwrap();
    }

    async fn replay() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/background-output/oracle.json"
        ))
        .unwrap();
        assert_eq!(
            oracle["baseline"],
            "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
        );
        for case in oracle["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let scratch = Scratch(
                std::env::temp_dir().join(format!("pdrs-553-rust-{}", uuid::Uuid::new_v4())),
            );
            fs::create_dir_all(&scratch.0).unwrap();
            let dir = scratch.0.join("output");
            let file = dir.join("task.log");
            let mut store = OutputStore::with_spill(
                super::super::DEFAULT_MEMORY_BYTES,
                case["spill"].as_bool().unwrap().then(|| file.clone()),
            );
            for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
                let mut result = None;
                match step["op"].as_str().unwrap() {
                    "append" => store.append(
                        step["text"]
                            .as_str()
                            .unwrap()
                            .repeat(step["repeat"].as_u64().unwrap() as usize)
                            .as_bytes(),
                    ),
                    "flush" => store.flush().await,
                    "close" => {
                        store.begin_close().await;
                        store.close();
                    }
                    "read" => {
                        let slice = store.read_slice(
                            &JsValue::Number(step["offset"].as_f64().unwrap()),
                            &step.get("cap").map_or(JsValue::Undefined, |n| {
                                JsValue::Number(n.as_f64().unwrap())
                            }),
                        );
                        result = Some(
                            json!({"nextOffset":slice.next_offset,"totalBytes":slice.total_bytes,"truncated":slice.truncated,"bytes":slice.content.len(),"sha256":digest(slice.content.as_bytes())}),
                        );
                    }
                    "disk" => {
                        result = Some(match fs::read(&file) {
                            Ok(bytes) => json!({"bytes":bytes.len(),"sha256":digest(&bytes)}),
                            Err(error) => {
                                assert!(
                                    matches!(
                                        error.raw_os_error(),
                                        Some(libc::ENOENT | libc::ENOTDIR)
                                    ),
                                    "{error}"
                                );
                                Value::Null
                            }
                        })
                    }
                    "block" => fs::write(&dir, b"block").unwrap(),
                    "unblock" => fs::remove_file(&dir).unwrap(),
                    "deny" => {
                        fs::create_dir(&dir).unwrap();
                        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
                    }
                    "allow" => {
                        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap()
                    }
                    "full" => store.spill.as_mut().unwrap().write_error = true,
                    "healthy" => store.spill.as_mut().unwrap().write_error = false,
                    "seed" => {
                        fs::create_dir(&dir).unwrap();
                        fs::write(&file, step["text"].as_str().unwrap()).unwrap();
                    }
                    "truncate" => OpenOptions::new()
                        .write(true)
                        .open(&file)
                        .unwrap()
                        .set_len(step["size"].as_u64().unwrap())
                        .unwrap(),
                    "remove" => fs::remove_file(&file).unwrap(),
                    other => panic!("unknown oracle operation {other}"),
                }
                if let Some(result) = result {
                    // JSON floats and integers compare numerically via a normalized serialization.
                    let expected = &step["result"];
                    if step["op"] == "read" {
                        assert_eq!(
                            result["nextOffset"].as_f64(),
                            expected["nextOffset"].as_f64(),
                            "{name} step {index}: cursor"
                        );
                        assert_eq!(
                            result["totalBytes"].as_f64(),
                            expected["totalBytes"].as_f64(),
                            "{name} step {index}: total"
                        );
                        for key in ["truncated", "bytes", "sha256"] {
                            assert_eq!(result[key], expected[key], "{name} step {index}: {key}");
                        }
                    } else {
                        assert_eq!(&result, expected, "{name} step {index}");
                    }
                }
            }
            store.flush().await;
            store.close();
        }
    }
}
