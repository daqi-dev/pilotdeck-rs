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
    native_file: Option<std::sync::Arc<File>>,
    native_link_check: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
    allow_prefix: bool,
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
        let native_file = self.native_file.clone();
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
                    if let Some(file) = native_file {
                        if !ready {
                            return Err(std::io::Error::other(
                                "An earlier native output write failed.",
                            ));
                        }
                        return (&*file).write_all(&chunk);
                    }
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
                native_file: None,
                native_link_check: None,
                allow_prefix: false,
                tail: watch::channel(Some(false)).1,
                #[cfg(test)]
                write_error: false,
                #[cfg(test)]
                write_gate: None,
            }),
        }
    }
    pub(super) fn with_native_file(
        max: usize,
        file: super::native_output::NativeOutputFile,
    ) -> Self {
        let mut store = Self::with_spill(max, Some(file.path));
        let spill = store.spill.as_mut().unwrap();
        spill.native_file = Some(std::sync::Arc::new(file.file));
        spill.allow_prefix = file.allow_prefix;
        spill.native_link_check = Some(file.link_check);
        spill.tail = watch::channel(Some(true)).1;
        store
    }
    pub(super) fn restore_native_file(
        max: usize,
        file: super::native_output::NativeOutputFile,
        expected_total: u64,
    ) -> std::io::Result<Self> {
        use std::os::unix::fs::FileExt;
        let actual = file.file.metadata()?.len();
        if actual < expected_total || (!file.allow_prefix && actual != expected_total) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Native task output length does not match its snapshot.",
            ));
        }
        let memory = expected_total.min(max as u64) as usize;
        let dropped = expected_total - memory as u64;
        let mut bytes = vec![0; memory];
        file.file.read_exact_at(&mut bytes, dropped)?;
        let mut store = Self::with_native_file(max, file);
        if !bytes.is_empty() {
            store.chunks.push_back(bytes);
        }
        store.total = expected_total;
        store.memory = memory;
        store.dropped = dropped;
        store.spill.as_mut().unwrap().tail = watch::channel(Some(true)).1;
        Ok(store)
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

    /// Freeze one accepted native prefix. A later append may replace the
    /// live tail while this future waits; it must not turn the earlier prefix
    /// into a fictitious IO failure. Actual write failures remain sticky.
    pub(super) fn native_checkpoint(
        &self,
    ) -> Option<std::pin::Pin<Box<dyn Future<Output = bool> + Send>>> {
        let spill = self.spill.as_ref()?;
        let file = spill.native_file.as_ref()?.clone();
        let link_check = spill.native_link_check.as_ref()?.clone();
        let mut tail = spill.tail.clone();
        let total = self.total;
        Some(Box::pin(async move {
            let ready = match tail.wait_for(Option::is_some).await {
                Ok(status) => *status == Some(true),
                Err(_) => false,
            };
            ready
                && link_check()
                && file
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() >= total)
        }))
    }
    /// A pending or failed write must never be advertised as recoverable.
    pub(super) fn has_durable_output(&self) -> bool {
        self.spill.as_ref().is_some_and(|spill| {
            *spill.tail.borrow() == Some(true)
                && spill.native_link_check.as_ref().is_none_or(|check| check())
                && spill
                    .native_file
                    .as_ref()
                    .map_or_else(|| fs::metadata(&spill.path), |file| file.metadata())
                    .is_ok_and(|metadata| {
                        if spill.allow_prefix {
                            metadata.len() >= self.total
                        } else {
                            metadata.len() == self.total
                        }
                    })
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
        let spill = self.spill.as_ref()?;
        if let Some(file) = &spill.native_file {
            use std::os::unix::fs::FileExt;
            let size = file.metadata().ok()?.len().min(self.total);
            let wanted = size.saturating_sub(offset).min(max);
            if wanted == 0 {
                return None;
            }
            let mut bytes = vec![0; usize::try_from(wanted).ok()?];
            let count = file.read_at(&mut bytes, offset).ok()?;
            if count == 0 {
                return None;
            }
            bytes.truncate(count);
            return Some(bytes);
        }
        let path = &spill.path;
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
            entry.append(bytes, None);
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

#[cfg(test)]
mod native_checkpoint_tests {
    use super::*;
    use std::{fs::OpenOptions, sync::Arc, time::Duration};
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    fn native_store() -> (Scratch, OutputStore) {
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "pdrs-native-output-checkpoint-{}",
            uuid::Uuid::new_v4()
        )));
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .open(&scratch.0)
            .unwrap();
        let store = OutputStore::with_native_file(
            1024,
            super::super::native_output::NativeOutputFile {
                file,
                path: scratch.0.clone(),
                allow_prefix: false,
                link_check: Arc::new(|| true),
            },
        );
        (scratch, store)
    }
    #[tokio::test]
    async fn newer_pending_append_does_not_invalidate_a_flushed_native_prefix() {
        let (scratch, mut store) = native_store();
        let (entered_first, release_first) = store.gate_next_write();
        store.append(b"first");
        entered_first.await.unwrap();
        let checkpoint = store.native_checkpoint().unwrap();
        let (entered_second, release_second) = store.gate_next_write();
        store.append(b"second");
        release_first.send(()).unwrap();
        let persisted = tokio::time::timeout(Duration::from_secs(2), checkpoint)
            .await
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), entered_second)
            .await
            .unwrap();
        let live_tail_is_pending = !store.has_durable_output();
        let prefix = fs::read(&scratch.0).unwrap();
        release_second.send(()).unwrap();
        store.flush().await;
        assert!(persisted, "captured first prefix was persisted");
        assert!(live_tail_is_pending);
        assert_eq!(prefix, b"first");
        assert!(store.native_checkpoint().unwrap().await);
        assert_eq!(fs::read(&scratch.0).unwrap(), b"firstsecond");
    }
    #[tokio::test]
    async fn native_output_write_failure_stays_failed_after_a_later_successful_attempt() {
        let (scratch, mut store) = native_store();
        store.spill.as_mut().unwrap().write_error = true;
        store.append(b"lost");
        store.flush().await;
        store.spill.as_mut().unwrap().write_error = false;
        store.append(b"later");
        store.flush().await;
        assert!(!store.native_checkpoint().unwrap().await);
        assert!(!store.has_durable_output());
        assert!(
            fs::read(&scratch.0).unwrap().is_empty(),
            "later bytes must not conceal an earlier missing prefix"
        );
    }
}
