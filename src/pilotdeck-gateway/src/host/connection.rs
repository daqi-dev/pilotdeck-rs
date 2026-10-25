//! One owned process group and independent pipe pumps. No callbacks in pumps.
use super::{
    HostError, HostErrorKind, Limits,
    budget::{Budget, Frame, Lease},
};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{
    collections::VecDeque,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
    time::timeout,
};

enum WriteCommand {
    Frame(String, Lease),
    Flush(oneshot::Sender<()>),
}

pub(super) struct Connection {
    child: Child,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
    outgoing: Option<mpsc::Sender<WriteCommand>>,
    // Separate one-slot allowances: optional writes cannot consume required
    // queue slots or its byte budget. ACKs coalesce to the latest applied seq.
    ack: watch::Sender<Option<Arc<String>>>,
    cancel: watch::Sender<Option<Arc<String>>>,
    incoming: mpsc::Receiver<Result<Frame, HostError>>,
    pumps: Vec<JoinHandle<()>>,
    stderr_task: JoinHandle<()>,
    stderr: Arc<Mutex<VecDeque<u8>>>,
    pending_required: Arc<AtomicUsize>,
    pub(super) budget: Arc<Budget>,
    limits: Limits,
    closed: bool,
    #[cfg(test)]
    term_sent: Arc<AtomicUsize>,
    // #542: unit tests keep EOF/TERM grace short on purpose, but the wait after
    // SIGKILL is real OS scheduling (Tokio's SIGCHLD reaping). Tests may widen
    // only that last wait. Production always uses `limits.close_timeout`.
    #[cfg(test)]
    reap_deadline: Option<std::time::Duration>,
}

impl Connection {
    pub(super) fn spawn(command: &mut Command, limits: Limits) -> Result<Self, HostError> {
        Self::spawn_with_budget(command, limits, Budget::new(limits.max_buffered_bytes))
    }
    pub(super) fn spawn_with_budget(
        command: &mut Command,
        limits: Limits,
        budget: Arc<Budget>,
    ) -> Result<Self, HostError> {
        // #191: Unix descendants belong to this client, including on Drop.
        #[cfg(unix)]
        command.process_group(0);
        #[cfg(unix)]
        pilotdeck_child_signals::install_child_signal_reset(command.as_std_mut());
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(HostError::io)?;
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut stderr_pipe = child.stderr.take().expect("piped stderr");
        let (outgoing, mut writes) = mpsc::channel(limits.queue_capacity);
        let (reads, incoming) = mpsc::channel(limits.queue_capacity);
        let (ack, mut acks) = watch::channel(None::<Arc<String>>);
        let (cancel, mut cancels) = watch::channel(None::<Arc<String>>);
        let writer_errors = reads.clone();
        let pending_required = Arc::new(AtomicUsize::new(0));
        let pending = pending_required.clone();
        let writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    command = writes.recv() => {
                        let Some(command) = command else { break; };
                        let result = match command {
                            WriteCommand::Frame(line, _lease) => {
                                let result = stdin.write_all(line.as_bytes()).await;
                                if result.is_ok() { pending.fetch_sub(1, Ordering::SeqCst); }
                                result
                            }
                            WriteCommand::Flush(done) => {
                                // Include the optional slots in the barrier, but their
                                // failures must not invalidate an authoritative final.
                                let cancel = cancels.has_changed().unwrap_or(false).then(|| cancels.borrow_and_update().clone()).flatten();
                                let ack = acks.has_changed().unwrap_or(false).then(|| acks.borrow_and_update().clone()).flatten();
                                if let Some(line) = cancel { optional_write(&mut stdin, &line).await; }
                                if let Some(line) = ack { optional_write(&mut stdin, &line).await; }
                                match stdin.flush().await {
                                    Ok(()) => { let _ = done.send(()); Ok(()) }
                                    Err(e) => Err(e),
                                }
                            }
                        };
                        if let Err(error) = result {
                            let _ = writer_errors.send(Err(HostError::io(error))).await;
                            break;
                        }
                    }
                    changed = cancels.changed() => {
                        if changed.is_err() { break; }
                        let line = cancels.borrow_and_update().clone();
                        if let Some(line) = line { optional_write(&mut stdin, &line).await; }
                    }
                    changed = acks.changed() => {
                        if changed.is_err() { break; }
                        let line = acks.borrow_and_update().clone();
                        if let Some(line) = line { optional_write(&mut stdin, &line).await; }
                    }
                }
            }
        });
        let input_budget = budget.clone();
        let reader = tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = Vec::new();
            let mut line_lease = input_budget.reserve(0).expect("empty reservation");
            loop {
                let available = match reader.fill_buf().await {
                    Ok([]) => {
                        if !line.is_empty()
                            && !deliver(&reads, std::mem::take(&mut line), line_lease).await
                        {
                            return;
                        }
                        let _ = reads
                            .send(Err(HostError::new(
                                HostErrorKind::Disconnected,
                                "Sidecar stdout closed before execute reached a terminal event.",
                            )))
                            .await;
                        return;
                    }
                    Ok(bytes) => bytes,
                    Err(error) => {
                        let _ = reads.send(Err(HostError::io(error))).await;
                        return;
                    }
                };
                let size = available
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(available.len(), |i| i + 1);
                if line.len() + size > limits.max_frame_bytes {
                    let _ = reads
                        .send(Err(HostError::new(
                            HostErrorKind::Limit,
                            "Sidecar stdout frame exceeds configured byte limit.",
                        )))
                        .await;
                    return;
                }
                if let Err(error) = line_lease.grow(size) {
                    let _ = reads.send(Err(error)).await;
                    return;
                }
                line.extend_from_slice(&available[..size]);
                reader.consume(size);
                if line.last() == Some(&b'\n') {
                    if !deliver(&reads, std::mem::take(&mut line), line_lease).await {
                        return;
                    }
                    line_lease = input_budget.reserve(0).expect("empty reservation");
                }
            }
        });
        // TS stdio:81-84,158-169: drain continuously, retain only the tail.
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        let retained = stderr.clone();
        let stderr_task = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while let Ok(size) = stderr_pipe.read(&mut bytes).await {
                if size == 0 {
                    break;
                }
                let mut tail = retained.lock().unwrap_or_else(|e| e.into_inner());
                tail.extend(&bytes[..size]);
                let excess = tail.len().saturating_sub(limits.max_stderr_bytes);
                tail.drain(..excess);
            }
        });
        Ok(Self {
            child,
            #[cfg(unix)]
            group,
            outgoing: Some(outgoing),
            ack,
            cancel,
            incoming,
            pumps: vec![reader, writer],
            stderr_task,
            stderr,
            pending_required,
            budget,
            limits,
            closed: false,
            #[cfg(test)]
            term_sent: Arc::new(AtomicUsize::new(0)),
            #[cfg(test)]
            reap_deadline: None,
        })
    }

    pub(super) fn send(&self, value: &JsValue) -> Result<(), HostError> {
        let line = self.encode(value)?;
        let lease = self.budget.reserve(line.len())?;
        self.pending_required.fetch_add(1, Ordering::SeqCst);
        let result = self.enqueue(WriteCommand::Frame(line, lease));
        if result.is_err() {
            self.pending_required.fetch_sub(1, Ordering::SeqCst);
        }
        result
    }
    pub(super) fn send_ack(&self, value: &JsValue) {
        self.send_optional(value, &self.ack);
    }
    pub(super) fn send_cancel(&self, value: &JsValue) {
        self.send_optional(value, &self.cancel);
    }
    fn send_optional(&self, value: &JsValue, slot: &watch::Sender<Option<Arc<String>>>) {
        match self.encode(value) {
            Ok(line) => {
                let _ = slot.send(Some(Arc::new(line)));
            }
            Err(error) => best_effort_log(format_args!(
                "Sidecar optional enqueue failed (best-effort): {error}"
            )),
        }
    }
    fn encode(&self, value: &JsValue) -> Result<String, HostError> {
        let mut line = stringify_json(value)
            .map_err(|e| HostError::protocol(e.to_string()))?
            .ok_or_else(|| HostError::protocol("Cannot send undefined."))?;
        line.push('\n');
        if line.len() > self.limits.max_frame_bytes {
            return Err(HostError::new(
                HostErrorKind::Limit,
                "Outgoing frame exceeds configured byte limit.",
            ));
        }
        Ok(line)
    }
    fn enqueue(&self, command: WriteCommand) -> Result<(), HostError> {
        self.outgoing
            .as_ref()
            .ok_or_else(|| HostError::protocol("Sidecar connection is closed."))?
            .try_send(command)
            .map_err(|error| {
                HostError::new(
                    match error {
                        mpsc::error::TrySendError::Full(_) => HostErrorKind::Limit,
                        mpsc::error::TrySendError::Closed(_) => HostErrorKind::Disconnected,
                    },
                    format!("Sidecar writer unavailable or queue full: {error}"),
                )
            })
    }
    pub(super) async fn receive(&mut self) -> Result<Frame, HostError> {
        self.incoming.recv().await.unwrap_or_else(|| {
            Err(HostError::new(
                HostErrorKind::Disconnected,
                "Sidecar pipe pumps stopped.",
            ))
        })
    }
    pub(super) async fn flush(&self) -> Result<(), HostError> {
        let (done, wait) = oneshot::channel();
        self.enqueue(WriteCommand::Flush(done))?;
        timeout(self.limits.io_timeout, wait)
            .await
            .map_err(|_| HostError::new(HostErrorKind::Timeout, "Sidecar writer flush timed out."))?
            .map_err(|_| {
                HostError::new(
                    HostErrorKind::Disconnected,
                    "Sidecar writer stopped before flush.",
                )
            })
    }
    pub(super) async fn finish_terminal(&self) -> Result<(), HostError> {
        if let Err(error) = self.flush().await {
            if self.pending_required.load(Ordering::SeqCst) != 0 {
                return Err(error);
            }
            best_effort_log(format_args!(
                "Sidecar terminal optional flush failed (best-effort): {error}"
            ));
        } else if self.pending_required.load(Ordering::SeqCst) != 0 {
            return Err(HostError::new(
                HostErrorKind::Disconnected,
                "Sidecar required frame was not written before terminal flush.",
            ));
        }
        Ok(())
    }
    pub(super) fn diagnostic(&self) -> String {
        let tail = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&tail.iter().copied().collect::<Vec<_>>())
            .trim()
            .into()
    }
    pub(super) fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    #[cfg(test)]
    pub(super) fn writer_task(&self) -> tokio::task::AbortHandle {
        // A test-only observation barrier: stdin EOF can precede completion of
        // its writer task. Advancing time before the join completes can consume
        // the writer-join timeout instead of the EOF grace period (#308/#346).
        self.pumps.last().expect("stdin writer").abort_handle()
    }

    #[cfg(test)]
    pub(super) fn term_sent(&self) -> Arc<AtomicUsize> {
        self.term_sent.clone()
    }

    // #308/#542: the same widening for the integration-style host tests. Only
    // the post-SIGKILL reap (pure OS scheduling) is widened; EOF/TERM grace
    // stays at limits.close_timeout. The product default is unchanged.
    #[cfg(test)]
    pub(super) fn widen_reap_for_test(&mut self) {
        self.reap_deadline = Some(std::time::Duration::from_secs(10));
    }

    // #542: the deadline for the only wait that depends on real OS scheduling
    // (reaping after SIGKILL). A test may override just this segment; the
    // product default is unchanged.
    #[cfg(test)]
    fn reap_deadline(&self) -> std::time::Duration {
        self.reap_deadline.unwrap_or(self.limits.close_timeout)
    }
    #[cfg(not(test))]
    fn reap_deadline(&self) -> std::time::Duration {
        self.limits.close_timeout
    }

    fn kill_group(&mut self) {
        crate::process_group::kill(self.group.take(), rustix::process::Signal::KILL);
    }
    #[cfg(unix)]
    async fn wait_for_exit_unreaped(&mut self) -> Result<bool, HostError> {
        crate::process_group::wait_for_exit_unreaped(self.group, self.limits.close_timeout)
            .await
            .map_err(|error| HostError::io(error.into()))
    }
    async fn stop_child(&mut self) -> Result<(), HostError> {
        #[cfg(unix)]
        {
            if !self.wait_for_exit_unreaped().await? {
                crate::process_group::kill(self.group, rustix::process::Signal::TERM);
                #[cfg(test)]
                self.term_sent.store(1, Ordering::SeqCst);
                self.wait_for_exit_unreaped().await?;
            }
            // Also clean descendants of a naturally exited leader. Clear the
            // group before the only reaping wait, so Drop cannot signal it again.
            self.kill_group();
        }
        #[cfg(not(unix))]
        {
            if let Ok(result) = timeout(self.limits.close_timeout, self.child.wait()).await {
                return result.map(|_| ()).map_err(HostError::io);
            }
            let _ = self.child.start_kill();
        }
        timeout(self.reap_deadline(), self.child.wait())
            .await
            .map_err(|_| {
                HostError::new(HostErrorKind::Timeout, "Sidecar did not exit after kill.")
            })?
            .map(|_| ())
            .map_err(HostError::io)
    }
    pub(super) async fn close(&mut self) -> Result<(), HostError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.outgoing.take();
        // Close stdin and stdout; stderr must survive through child exit/kill.
        for pump in &self.pumps {
            pump.abort();
        }
        // Aborting only schedules cancellation. Join the writer before starting
        // the EOF grace period so the stdin handle has actually been dropped.
        if let Some(mut writer) = self.pumps.pop() {
            let _ = timeout(self.limits.close_timeout, &mut writer).await;
        }
        let result = self.stop_child().await;
        // TS stdio:123-154 keeps collecting exit diagnostics. EOF normally wins;
        // a descendant that escaped the owned process group cannot hang close.
        if timeout(self.limits.close_timeout, &mut self.stderr_task)
            .await
            .is_err()
        {
            self.stderr_task.abort();
            let _ = timeout(self.limits.close_timeout, &mut self.stderr_task).await;
        }
        let _ = timeout(self.limits.close_timeout, async {
            for task in self.pumps.drain(..) {
                let _ = task.await;
            }
        })
        .await;
        // Recovery retains application/cache leases, but unread transport bytes
        // belong to this dead connection and must release the shared budget.
        self.incoming.close();
        while self.incoming.try_recv().is_ok() {}
        result
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.kill_group();
        for pump in &self.pumps {
            pump.abort();
        }
        self.stderr_task.abort();
        // Child::kill_on_drop also reaps dropped spawn/execute futures.
    }
}
async fn deliver(
    sender: &mpsc::Sender<Result<Frame, HostError>>,
    bytes: Vec<u8>,
    lease: Lease,
) -> bool {
    let value = match std::str::from_utf8(&bytes) {
        Ok(text) if text.trim().is_empty() => return true,
        Ok(text) => parse_json(text)
            .map_err(|error| HostError::protocol(format!("Malformed NDJSON: {error}"))),
        Err(error) => Err(HostError::protocol(format!("Malformed UTF-8: {error}"))),
    }
    .map(|value| Frame { value, lease });
    drop(bytes);
    let valid = value.is_ok();
    sender.send(value).await.is_ok() && valid
}
async fn optional_write(stdin: &mut tokio::process::ChildStdin, line: &str) {
    // TS client:943-970: cancel and ACK failures never enter the input stream.
    if let Err(error) = stdin.write_all(line.as_bytes()).await {
        best_effort_log(format_args!(
            "Sidecar optional write failed (best-effort): {error}"
        ));
    }
}
fn best_effort_log(message: std::fmt::Arguments<'_>) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "{message}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::HostClient;
    #[cfg(unix)]
    use crate::process_spawn::{child_signals, test_support};
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn host_resets_inherited_signals() {
        child_signals::ChildSignalProbe::new()
            .launch("host::connection::tests::host_signal_worker", false);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "isolated worker launched with inherited ignored/blocked signals"]
    async fn host_signal_worker() {
        test_support::assert_inherited();
        let probe = child_signals::probe_path();
        let report = probe.with_file_name("host-report.txt");
        let mut command = Command::new(&probe);
        command.arg("report-file").arg(&report);
        let mut connection = Connection::spawn(&mut command, Limits::default()).unwrap();
        test_support::assert_reset_report_file(&report).await;
        connection.close().await.unwrap();
        test_support::assert_inherited();
    }

    fn peer(mode: &str) -> Command {
        let mut command = Command::new("python3");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../scripts/a6-host/fixture.py"
            ))
            .arg(mode);
        command
    }
    fn limits() -> Limits {
        Limits {
            io_timeout: Duration::from_millis(20),
            close_timeout: Duration::from_millis(50),
            ..Limits::default()
        }
    }
    // #542: this module deliberately drives EOF/TERM grace with 50 ms. Only the
    // wait after SIGKILL is real OS scheduling (Tokio's SIGCHLD reaping), so
    // widen that single stage; the protocol deadlines stay short on purpose.
    fn widen_reap(connection: &mut Connection) {
        connection.widen_reap_for_test();
    }
    fn controlled_queue(capacity: usize) -> (Connection, mpsc::Receiver<WriteCommand>) {
        let mut connection = Connection::spawn(&mut peer("handshake-hang"), limits()).unwrap();
        widen_reap(&mut connection);
        let (sender, receiver) = mpsc::channel(capacity);
        // Freeze only the writer; the private channel makes pressure deterministic.
        connection.pumps[1].abort();
        connection.outgoing = Some(sender);
        (connection, receiver)
    }
    #[tokio::test]
    async fn ndjson_splits_batched_frames_and_preserves_cross_write_partial_frame() {
        let mut connection =
            Connection::spawn(&mut peer("batched-split-frames"), limits()).unwrap();
        widen_reap(&mut connection);
        connection.send(&parse_json("{}").unwrap()).unwrap();
        for expected in [1.0, 2.0] {
            let frame = timeout(Duration::from_secs(2), connection.receive())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                frame.get("frame").and_then(JsValue::as_number),
                Some(expected)
            );
        }
        assert!(
            timeout(Duration::from_millis(30), connection.receive())
                .await
                .is_err()
        );
        connection
            .send(&parse_json(r#"{"continue":true}"#).unwrap())
            .unwrap();
        let frame = timeout(Duration::from_secs(2), connection.receive())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(frame.get("frame").and_then(JsValue::as_number), Some(3.0));
        drop(frame);
        connection.close().await.unwrap();
        println!("NDJSON_PASS one write: frames 1,2 + partial 3; second write: completed 3");
    }
    #[tokio::test]
    #[cfg(all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    async fn exited_leader_is_not_reaped_until_group_signal_is_consumed() {
        let mut command = Command::new("python3");
        command.args(["-c", "pass"]);
        let mut connection = Connection::spawn(
            &mut command,
            Limits {
                close_timeout: Duration::from_secs(2),
                ..limits()
            },
        )
        .unwrap();
        let pid = connection.pid().unwrap();
        assert!(connection.wait_for_exit_unreaped().await.unwrap());
        assert_eq!(connection.pid(), Some(pid));
        assert!(connection.group.is_some());
        // A second observation of the zombie also succeeds: WNOWAIT did not reap.
        assert!(connection.wait_for_exit_unreaped().await.unwrap());
        connection.kill_group();
        assert!(connection.group.is_none());
        connection.child.wait().await.unwrap();
        assert_eq!(connection.pid(), None);
        connection.close().await.unwrap();
        assert!(connection.group.is_none());
        println!(
            "GROUP_LIFETIME_PASS exited leader retained until group signal; no pgid after wait"
        );
    }
    #[tokio::test]
    async fn ack_coalesces_without_consuming_required_slots_or_budget() {
        let (mut connection, mut receiver) = controlled_queue(1);
        let frame = parse_json(r#"{"kind":"request","method":"ack"}"#).unwrap();
        // Keep receivers alive after the controlled writer was aborted.
        let mut ack_receiver = connection.ack.subscribe();
        for sequence in 0..100 {
            let ack = parse_json(&format!(
                r#"{{"kind":"request","method":"ack","lastAppliedSequence":{sequence}}}"#
            ))
            .unwrap();
            connection.send_ack(&ack);
        }
        connection.send(&frame).unwrap();
        assert!(matches!(
            receiver.recv().await,
            Some(WriteCommand::Frame(_, _))
        ));
        let latest = ack_receiver.borrow_and_update().clone().unwrap();
        assert_eq!(
            parse_json(&latest)
                .unwrap()
                .get("lastAppliedSequence")
                .and_then(JsValue::as_number),
            Some(99.0)
        );
        connection.send(&frame).unwrap();
        assert_eq!(
            connection.send(&frame).unwrap_err().kind,
            HostErrorKind::Limit
        );
        drop(receiver);
        connection.send_ack(&frame);
        assert!(
            connection
                .send(&frame)
                .unwrap_err()
                .message
                .contains("channel closed")
        );
        connection.close().await.unwrap();
    }
    #[tokio::test]
    async fn terminal_flush_timeout_is_diagnostic_unless_required_frames_are_pending() {
        let (mut connection, _receiver) = controlled_queue(4);
        // The receiver stays alive without consuming the flush command.
        connection.finish_terminal().await.unwrap();
        let response = parse_json(r#"{"kind":"response","ok":true}"#).unwrap();
        connection.send(&response).unwrap();
        let error = connection.finish_terminal().await.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Timeout);
        assert!(error.message.contains("writer flush timed out"), "{error}");
        assert_eq!(connection.pending_required.load(Ordering::SeqCst), 1);
        connection.close().await.unwrap();
    }
    #[tokio::test]
    async fn terminal_flush_success_still_fails_when_required_frame_is_pending() {
        let (mut connection, mut receiver) = controlled_queue(4);
        let drain = tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                if let WriteCommand::Flush(done) = command {
                    let _ = done.send(());
                }
            }
        });
        connection.finish_terminal().await.unwrap();
        // A required frame counted but not yet written when the Flush completes.
        connection.pending_required.fetch_add(1, Ordering::SeqCst);
        let error = connection.finish_terminal().await.unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Disconnected);
        assert!(
            error.message.contains("required frame was not written"),
            "{error}"
        );
        connection.pending_required.fetch_sub(1, Ordering::SeqCst);
        connection.close().await.unwrap();
        drain.abort();
    }
    #[tokio::test]
    async fn ack_pipe_failure_keeps_writer_alive_to_report_required_write_failure() {
        let mut client = HostClient::spawn(&mut peer("terminal-then-exit"), None, limits())
            .await
            .unwrap();
        widen_reap(&mut client.connection);
        let connection = &mut client.connection;
        connection.send(&parse_json(r#"{"kind":"request","messageId":"execute","method":"execute","runId":"run","operationId":"operation","requestId":"request","sessionId":"session","turnId":"turn","payload":{}}"#).unwrap()).unwrap();
        let accepted = connection.receive().await.unwrap();
        assert_eq!(accepted.get("ok").and_then(JsValue::as_bool), Some(true));
        // The fixture closes stdin before writing this event, so ACK must hit EPIPE.
        let event = connection.receive().await.unwrap();
        assert_eq!(event.get("final").and_then(JsValue::as_bool), Some(false));
        let ack = parse_json(r#"{"kind":"request","messageId":"ack-0","method":"ack","streamId":"stream","lastAppliedSequence":0}"#).unwrap();
        connection.send_ack(&ack);
        connection
            .flush()
            .await
            .expect("writer must continue after ACK EPIPE");
        let final_event = connection.receive().await.unwrap();
        assert_eq!(
            final_event.get("final").and_then(JsValue::as_bool),
            Some(true)
        );
        let eof = connection.receive().await.unwrap_err();
        assert_eq!(eof.kind, HostErrorKind::Disconnected);
        assert!(matches!(
            connection.incoming.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        let response = parse_json(r#"{"kind":"response","inReplyTo":"call","ok":true}"#).unwrap();
        connection.send(&response).unwrap();
        let error = timeout(Duration::from_secs(2), connection.receive())
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Io);
        assert_eq!(connection.pending_required.load(Ordering::SeqCst), 1);
        assert!(connection.finish_terminal().await.is_err());
        connection.close().await.unwrap();
    }
    #[tokio::test]
    async fn inbound_and_outbound_frame_limits_include_newline_at_exact_boundary() {
        let value = parse_json(r#"{"boundary":"x"}"#).unwrap();
        let size = stringify_json(&value).unwrap().unwrap().len() + 1;
        for maximum in [size, size - 1] {
            let bounded = Limits {
                max_frame_bytes: maximum,
                ..limits()
            };
            let mut connection = Connection::spawn(&mut peer("frame-boundary"), bounded).unwrap();
            widen_reap(&mut connection);
            if maximum == size {
                connection.send(&value).unwrap();
                let received = connection.receive().await.unwrap();
                assert_eq!(received.value, value);
            } else {
                assert_eq!(
                    connection.send(&value).unwrap_err().kind,
                    HostErrorKind::Limit
                );
                connection.send(&parse_json("{}").unwrap()).unwrap();
                let error = connection.receive().await.unwrap_err();
                assert_eq!(error.kind, HostErrorKind::Limit);
                assert!(error.message.contains("stdout frame exceeds"));
            }
            connection.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn required_write_byte_budget_is_released_when_queue_drops() {
        let (mut connection, mut receiver) = controlled_queue(4);
        let value = parse_json(r#"{"payload":"retained"}"#).unwrap();
        let size = connection.encode(&value).unwrap().len();
        connection.budget = Budget::new(size);
        let _ack_receiver = connection.ack.subscribe();
        connection.send_ack(&value);
        connection.send(&value).unwrap();
        let error = connection.send(&value).unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Limit);
        assert!(error.message.contains("wire-byte budget"));
        drop(receiver.recv().await);
        connection.send(&value).unwrap();
        drop(receiver);
        // No retained required bytes remain after queue destruction.
        let _lease = connection.budget.reserve(size).unwrap();
        connection.close().await.unwrap();
    }

    #[tokio::test]
    async fn required_write_failure_after_terminal_never_reconnects() {
        use crate::host::{ExecuteRequest, HostFuture, HostHandler, ReconnectObservation};
        use pilotdeck_protocol::{JsObject, admission::ExecutionIdentity};
        use tokio::sync::Notify;
        struct Handler {
            terminal: Notify,
            release: Arc<Notify>,
            observations: AtomicUsize,
        }
        impl HostHandler for Handler {
            fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
                let release = self.release.clone();
                Box::pin(async move {
                    release.notified().await;
                    Ok(JsObject::new())
                })
            }
            fn event(&self, _event: JsValue) -> HostFuture<()> {
                self.terminal.notify_one();
                Box::pin(async { Ok(()) })
            }
            fn observe_transport(&self, _observation: ReconnectObservation) {
                self.observations.fetch_add(1, Ordering::SeqCst);
            }
        }
        let handler = Arc::new(Handler {
            terminal: Notify::new(),
            release: Arc::new(Notify::new()),
            observations: AtomicUsize::new(0),
        });
        let client = HostClient::spawn_resumable(
            peer("terminal-pending-callback"),
            None,
            Limits {
                terminal_drain_timeout: Duration::from_secs(2),
                ..limits()
            },
        )
        .await
        .unwrap();
        let writer = client.connection.pumps[1].abort_handle();
        let task_handler = handler.clone();
        let execution = tokio::spawn(async move {
            client
                .execute(
                    ExecuteRequest {
                        identity: ExecutionIdentity {
                            run_id: "run".into(),
                            operation_id: "operation".into(),
                            request_id: "request".into(),
                            session_id: "session".into(),
                            turn_id: "turn".into(),
                        },
                        payload: JsObject::new(),
                        idempotency_key: None,
                        operation_deadline: None,
                    },
                    task_handler,
                )
                .await
        });
        timeout(Duration::from_secs(2), handler.terminal.notified())
            .await
            .unwrap();
        writer.abort();
        timeout(Duration::from_secs(2), async {
            while !writer.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Force a required enqueue failure only after the authoritative final
        // is applied. It must not open a handshake or extend terminal drain.
        handler.release.notify_one();
        let error = timeout(Duration::from_secs(2), execution)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind, HostErrorKind::Disconnected);
        assert_eq!(error.last_applied_sequence, Some(0.0));
        assert_eq!(handler.observations.load(Ordering::SeqCst), 0);
        println!(
            "TERMINAL_RECONNECT_PASS required enqueue failure after final remains fail-closed; no reconnect observation"
        );
    }
}
