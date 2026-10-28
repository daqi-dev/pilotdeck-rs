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
    #[cfg(test)]
    write_gate: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
}

impl Spill {
    fn append(&mut self, chunk: Vec<u8>) {
        let mut previous = self.tail.clone();
        let (done, tail) = watch::channel(None);
        self.tail = tail;
        let path = self.path.clone();
        #[cfg(test)]
        let write_error = self.write_error;
        #[cfg(test)]
        let write_gate = self.write_gate.take();
        // Own accepted writes independently of the caller and keep their order.
        drop(tokio::spawn(async move {
            let ready = previous
                .wait_for(Option::is_some)
                .await
                .ok()
                .and_then(|v| *v)
                .unwrap_or(false);
            #[cfg(test)]
            if let Some((entered, release)) = write_gate {
                let _ = entered.send(());
                // Sender drop also releases the write when an assertion fails.
                let _ = release.await;
            }
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
                #[cfg(test)]
                write_gate: None,
            }),
        }
    }
    /// Recover only the bounded tail into memory; older bytes stay on disk.
    pub(super) fn restore_from_spill(
        max: usize,
        path: PathBuf,
        expected_total: u64,
    ) -> std::io::Result<Self> {
        let mut store = Self::with_spill(max, Some(path.clone()));
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected_total == 0 => {
                return Ok(store);
            }
            Err(error) => return Err(error),
        };
        let total = file.metadata()?.len();
        if total != expected_total {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Task output at {} has {total} bytes; expected {expected_total}.",
                    path.display()
                ),
            ));
        }
        let memory = total.min(max as u64) as usize;
        let dropped = total - memory as u64;
        file.seek(SeekFrom::Start(dropped))?;
        let mut retained = vec![0; memory];
        file.read_exact(&mut retained)?;
        if !retained.is_empty() {
            store.chunks.push_back(retained);
        }
        store.total = total;
        store.memory = memory;
        store.dropped = dropped;
        store.spill.as_mut().unwrap().tail = watch::channel(Some(true)).1;
        Ok(store)
    }

    /// A pending or failed write must never be advertised as recoverable.
    pub(super) fn has_durable_output(&self) -> bool {
        self.spill.as_ref().is_some_and(|spill| {
            *spill.tail.borrow() == Some(true)
                && fs::metadata(&spill.path).is_ok_and(|metadata| metadata.len() == self.total)
        })
    }
    /// Hold exactly the next accepted write before filesystem I/O.
    #[cfg(test)]
    pub(super) fn gate_next_write(
        &mut self,
    ) -> (
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (entered, waiting) = tokio::sync::oneshot::channel();
        let (release, released) = tokio::sync::oneshot::channel();
        self.spill.as_mut().expect("configured spill").write_gate = Some((entered, released));
        (waiting, release)
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

    #[tokio::test]
    async fn restored_spill_keeps_a_bounded_ring_and_complete_disk_history() {
        let scratch = crate::test_scratch::Scratch::new("617-output-restore");
        let path = scratch.path().join("task.log");
        fs::write(&path, b"abcdef").unwrap();
        let mut store = OutputStore::restore_from_spill(4, path.clone(), 6).unwrap();
        assert_eq!((store.total, store.dropped, store.memory), (6, 2, 4));
        assert!(store.has_durable_output());
        let slice = store.read_slice(&JsValue::Number(0.0), &JsValue::Number(6.0));
        assert_eq!(slice.content, "abcdef");
        assert_eq!(slice.next_offset, 6.0);
        assert!(!slice.truncated);
        let slice = store.read_slice(&JsValue::Number(2.0), &JsValue::Number(4.0));
        assert_eq!(slice.content, "cdef");
        store.append(b"gh");
        store.flush().await;
        assert_eq!(store.total, 8);
        assert!(store.has_durable_output());
        assert_eq!(fs::read(path).unwrap(), b"abcdefgh");
    }

    #[test]
    fn restore_rejects_missing_or_mismatched_nonempty_logs() {
        let scratch = crate::test_scratch::Scratch::new("617-output-invalid");
        let path = scratch.path().join("task.log");
        let empty = OutputStore::restore_from_spill(4, path.clone(), 0).unwrap();
        assert_eq!(empty.total, 0);
        assert!(!empty.has_durable_output());
        assert!(matches!(
            OutputStore::restore_from_spill(4, path.clone(), 6),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ));
        fs::write(&path, b"abc").unwrap();
        for expected in [2, 4] {
            assert!(matches!(
                OutputStore::restore_from_spill(4, path.clone(), expected),
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData
            ));
        }
    }

    #[tokio::test]
    async fn durability_requires_a_finished_complete_write_chain() {
        let scratch = crate::test_scratch::Scratch::new("617-output-durable");
        let mut store = OutputStore::with_spill(4, Some(scratch.path().join("task.log")));
        let (entered, release) = store.gate_next_write();
        store.append(b"abc");
        entered.await.unwrap();
        assert!(!store.has_durable_output());
        release.send(()).unwrap();
        store.flush().await;
        assert!(store.has_durable_output());
        store.spill.as_mut().unwrap().write_error = true;
        store.append(b"lost");
        store.flush().await;
        assert!(!store.has_durable_output());
        store.spill.as_mut().unwrap().write_error = false;
        store.append(b"tail");
        store.flush().await;
        assert!(
            !store.has_durable_output(),
            "a successful tail write must not hide an earlier missing chunk"
        );
    }

    #[tokio::test]
    async fn begin_close_rejects_append_before_ring_is_released() {
        let mut store = OutputStore::new(10);
        store.append(b"accepted");
        let flushed = store.begin_close();
        store.append(b"rejected");
        assert_eq!(
            store.total, 8,
            "begin_close must freeze accepted output #994 background-output assertion"
        );
        let slice = store.read_slice(&JsValue::Number(0.0), &JsValue::Undefined);
        assert_eq!(
            slice.content, "accepted",
            "begin_close must retain the ring #994 background-output assertion"
        );
        flushed.await;
        store.close();
        assert!(store.chunks.is_empty(), "#994 background-output assertion");
    }

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
mod dispose_tests {
    use super::*;
    use crate::runtime::background::{
        BackgroundTaskRuntime, BackgroundTaskRuntimeOptions, Phase, StartTaskSpec,
    };
    use std::{future::poll_fn, task::Poll, time::Duration};

    async fn replay(mode: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            let oracle: serde_json::Value = serde_json::from_str(include_str!(
                "../../../tests/fixtures/background-dispose-flush/oracle.json"
            ))
            .expect("#994 dispose-flush setup oracle");
            let row = oracle["cases"]
                .as_array()
                .expect("#994 dispose-flush setup cases")
                .iter()
                .find(|row| row["mode"] == mode)
                .expect("#994 dispose-flush setup matrix cell");
            let bytes = oracle["content"]
                .as_str()
                .expect("#994 dispose-flush setup content")
                .as_bytes();
            let scratch = crate::test_scratch::Scratch::new("994-dispose-flush");
            let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
                disk_spill_dir: Some(scratch.path().to_path_buf()),
                // A snapshot store would hide a removed disposal await by
                // flushing the same write again during final persistence.
                snapshot_store: None,
                ..Default::default()
            });
            let task = runtime
                .start(StartTaskSpec {
                    command: JsValue::from("exit 0"),
                    cwd: "/".into(),
                    env: None,
                    session_id: JsValue::Undefined,
                    agent_id: JsValue::Undefined,
                    kind: JsValue::Undefined,
                })
                .await
                .expect("#994 dispose-flush setup start");
            let entry = runtime
                .find(&task.task_id, None)
                .expect("#994 dispose-flush setup entry");
            entry.wait_done().await;
            let (entered, release) = {
                let mut state = entry.state.lock().expect("#994 dispose-flush setup lock");
                state
                    .output
                    .spill
                    .as_mut()
                    .expect("#994 dispose-flush setup spill")
                    .write_error = mode == "cancelled";
                state.output.gate_next_write()
            };
            entry.append(bytes);
            entered
                .await
                .expect("#994 dispose-flush setup gate entered");
            let mut release = Some(release);
            if mode == "ready" {
                release
                    .take()
                    .expect("#994 dispose-flush setup ready sender")
                    .send(())
                    .expect("#994 dispose-flush setup ready release");
                let flushed = entry
                    .state
                    .lock()
                    .expect("#994 dispose-flush setup ready lock")
                    .output
                    .flush();
                flushed.await;
            }
            // All predecessors (starts, kill_all, entry.done) are settled.
            // Poll the disposal body itself: the public API spawns this body,
            // so polling just that API would only observe scheduler latency.
            let mut disposal = Box::pin(runtime.dispose_now());
            let disposed = poll_fn(|cx| Poll::Ready(disposal.as_mut().poll(cx).is_ready())).await;
            assert_eq!(
                disposed,
                row["before"]["disposed"]
                    .as_bool()
                    .expect("#994 dispose-flush setup expected pending"),
                "#994 dispose-flush must await gated write without snapshot store"
            );
            let retained: Vec<u8> = entry
                .state
                .lock()
                .expect("#994 dispose-flush setup retained lock")
                .output
                .chunks
                .iter()
                .flatten()
                .copied()
                .collect();
            assert_eq!(
                retained,
                row["before"]["retained"]
                    .as_str()
                    .expect("#994 dispose-flush setup retained")
                    .as_bytes(),
                "#994 dispose-flush ring lifetime"
            );
            if let Some(release) = release {
                // F1 is the never-finishing-predecessor observation: no other
                // code owns the sender, and the write cannot finish until here.
                release.send(()).expect("#994 dispose-flush setup release");
                disposal.await;
            }
            assert_eq!(
                runtime
                    .inner
                    .state
                    .lock()
                    .expect("#994 dispose-flush setup final lock")
                    .phase,
                Phase::Disposed,
                "#994 dispose-flush final phase"
            );
            let path = scratch.path().join("output").join(format!(
                "{}.log",
                String::from_utf16_lossy(task.task_id.units())
            ));
            let disk = match fs::read_to_string(path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("#994 dispose-flush setup disk read: {error}"),
            };
            assert_eq!(
                disk.as_deref(),
                row["after"]["disk"].as_str(),
                "#994 dispose-flush completed disk bytes"
            );
        })
        .await
        .expect("#994 dispose-flush setup bounded probe");
    }

    #[tokio::test]
    async fn dispose_without_snapshot_waits_for_gated_write() {
        replay("pending").await;
    }

    #[tokio::test]
    async fn dispose_without_snapshot_accepts_completed_write() {
        replay("ready").await;
    }

    #[tokio::test]
    async fn dispose_without_snapshot_waits_for_cancelled_write() {
        replay("cancelled").await;
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
        tokio::time::timeout(std::time::Duration::from_secs(30), replay(false))
            .await
            .expect("#994 background-output setup");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn spill_replays_added_ts_boundary_cases() {
        tokio::time::timeout(std::time::Duration::from_secs(30), replay(true))
            .await
            .expect("#994 background-output setup");
    }

    async fn replay(only_added_cases: bool) {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/background-output/oracle.json"
        ))
        .expect("#994 background-output setup");
        assert_eq!(
            oracle["baseline"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",
            "#994 background-output assertion"
        );
        for case in oracle["cases"]
            .as_array()
            .expect("#994 background-output setup")
        {
            let name = case["name"].as_str().expect("#994 background-output setup");
            if only_added_cases
                && !matches!(
                    name,
                    "disk-ring-utf8-boundary"
                        | "disk-cap-and-closed-read"
                        | "unflushed-utf8-ring-boundary"
                )
            {
                continue;
            }
            let scratch = Scratch(
                std::env::temp_dir().join(format!("pdrs-553-rust-{}", uuid::Uuid::new_v4())),
            );
            fs::create_dir_all(&scratch.0).expect("#994 background-output setup");
            let dir = scratch.0.join("output");
            let file = dir.join("task.log");
            let mut store = OutputStore::with_spill(
                super::super::DEFAULT_MEMORY_BYTES,
                case["spill"]
                    .as_bool()
                    .expect("#994 background-output setup")
                    .then(|| file.clone()),
            );
            for (index, step) in case["steps"]
                .as_array()
                .expect("#994 background-output setup")
                .iter()
                .enumerate()
            {
                let mut result = None;
                match step["op"].as_str().expect("#994 background-output setup") {
                    "append" => store.append(
                        step["text"]
                            .as_str()
                            .expect("#994 background-output setup")
                            .repeat(
                                step["repeat"]
                                    .as_u64()
                                    .expect("#994 background-output setup")
                                    as usize,
                            )
                            .as_bytes(),
                    ),
                    "flush" => store.flush().await,
                    "close" => {
                        store.begin_close().await;
                        store.close();
                    }
                    "read" => {
                        let slice = store.read_slice(
                            &JsValue::Number(
                                step["offset"]
                                    .as_f64()
                                    .expect("#994 background-output setup"),
                            ),
                            &step.get("cap").map_or(JsValue::Undefined, |n| {
                                JsValue::Number(n.as_f64().expect("#994 background-output setup"))
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
                                    "{error} #994 background-output assertion"
                                );
                                Value::Null
                            }
                        })
                    }
                    "block" => fs::write(&dir, b"block").expect("#994 background-output setup"),
                    "unblock" => fs::remove_file(&dir).expect("#994 background-output setup"),
                    "deny" => {
                        fs::create_dir(&dir).expect("#994 background-output setup");
                        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500))
                            .expect("#994 background-output setup");
                    }
                    "allow" => fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                        .expect("#994 background-output setup"),
                    "full" => {
                        store
                            .spill
                            .as_mut()
                            .expect("#994 background-output setup")
                            .write_error = true
                    }
                    "healthy" => {
                        store
                            .spill
                            .as_mut()
                            .expect("#994 background-output setup")
                            .write_error = false
                    }
                    "seed" => {
                        fs::create_dir(&dir).expect("#994 background-output setup");
                        fs::write(
                            &file,
                            step["text"].as_str().expect("#994 background-output setup"),
                        )
                        .expect("#994 background-output setup");
                    }
                    "truncate" => OpenOptions::new()
                        .write(true)
                        .open(&file)
                        .expect("#994 background-output setup")
                        .set_len(step["size"].as_u64().expect("#994 background-output setup"))
                        .expect("#994 background-output setup"),
                    "remove" => fs::remove_file(&file).expect("#994 background-output setup"),
                    other => panic!("unknown oracle operation {other}"),
                }
                if let Some(result) = result {
                    // JSON floats and integers compare numerically via a normalized serialization.
                    let expected = &step["result"];
                    if step["op"] == "read" {
                        assert_eq!(
                            result["nextOffset"].as_f64(),
                            expected["nextOffset"].as_f64(),
                            "{name} step {index}: cursor #994 background-output oracle comparison"
                        );
                        assert_eq!(
                            result["totalBytes"].as_f64(),
                            expected["totalBytes"].as_f64(),
                            "{name} step {index}: total #994 background-output oracle comparison"
                        );
                        for key in ["truncated", "bytes", "sha256"] {
                            assert_eq!(
                                result[key], expected[key],
                                "{name} step {index}: {key} #994 background-output oracle comparison"
                            );
                        }
                    } else {
                        assert_eq!(
                            &result, expected,
                            "{name} step {index} #994 background-output oracle comparison"
                        );
                    }
                }
            }
            store.flush().await;
            store.close();
        }
    }
}
