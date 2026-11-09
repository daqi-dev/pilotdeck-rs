use super::*;
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{collections::HashSet, fs::OpenOptions, io::Write, time::UNIX_EPOCH};
use uuid::Uuid;

impl ProjectSessionForkPort for NativeProjectSessionForkPort {
    fn fork(&self, request: ProjectSessionForkRequest) -> ForkFuture<'_, ()> {
        let adapter = self.adapter.clone();
        let now = self.now.clone();
        Box::pin(async move {
            let source_key = request.source_session_id.to_utf8().map_err(failure)?;
            let target_key = request.target_session_id.to_utf8().map_err(failure)?;
            let storage = |session_id| {
                adapter.create_session_storage(SessionStorageInput {
                    project_root: request.project_root.clone(),
                    pilot_home: request.pilot_home.clone(),
                    session_id,
                    now: Some(now.clone()),
                })
            };
            let source = storage(request.source_session_id).map_err(failure)?;
            let target = storage(request.target_session_id).map_err(failure)?;
            confined(&source.chat_dir, &source.transcript_path)?;
            confined(&target.chat_dir, &target.transcript_path)?;
            create_fork(
                &source,
                &target,
                &request.entries,
                &source_key,
                &target_key,
                request.transform_auxiliary_entry.as_deref(),
            )
        })
    }
}

impl HistoryStorage {
    pub async fn fork(&self, input: &JsValue) -> ReplacementResult {
        let guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let source = self.storage(input)?;
        self.recover(&source)?;
        let read = self.read_entries(&source, &source.transcript_path)?;
        if read.entries.is_empty() {
            return Err(error(
                "fork_empty_transcript",
                "Cannot fork an empty session transcript.",
            ));
        }
        if read.diagnostics.iter().any(|d| d.severity == "error") {
            return Err(error(
                "fork_invalid_transcript",
                "The conversation transcript could not be safely forked.",
            ));
        }
        let entries = read.entries;
        let from = required(input, "fromEntryId", "fork_entry_not_found")?;
        let target = entries
            .iter()
            .find(|e| field(e, "entryId").as_string() == Some(&from))
            .ok_or_else(|| {
                error(
                    "fork_entry_not_found",
                    format!("Transcript entry not found: {}", text(input, "fromEntryId")),
                )
            })?;
        let accepted = if is(target, "type", "accepted_input") {
            target
        } else {
            entries
                .iter()
                .find(|e| {
                    is(e, "type", "accepted_input") && field(e, "turnId") == field(target, "turnId")
                })
                .ok_or_else(|| {
                    error(
                        "fork_turn_not_found",
                        format!(
                            "No accepted_input found for turn {}",
                            text(target, "turnId")
                        ),
                    )
                })?
        };
        let preserve = !is(target, "type", "accepted_input")
            || matches!(field(input, "resumeAt"), JsValue::Bool(true));
        if input
            .get("resumeDropsTurn")
            .is_some_and(|v| !v.is_undefined())
        {
            let drop = entries.iter().find(|e| {
                field(e, "entryId") == field(input, "resumeDropsTurn")
                    && is(e, "type", "accepted_input")
            });
            let prefix = "Resume rejected by --resume-drops-turn: ";
            let drop = drop.ok_or_else(|| {
                error(
                    "resume_drops_turn_invalid",
                    format!("{prefix}the supplied UUID is not an accepted-input entry."),
                )
            })?;
            if number(drop, "sequence") <= number(target, "sequence") {
                return Err(error(
                    "resume_drops_turn_invalid",
                    format!("{prefix}the dropped turn must occur after the resume point."),
                ));
            }
            let discarded: Vec<_> = entries
                .iter()
                .filter(|e| number(e, "sequence") > number(target, "sequence"))
                .collect();
            if discarded.is_empty()
                || discarded
                    .iter()
                    .any(|e| field(e, "turnId") != field(drop, "turnId"))
            {
                return Err(error(
                    "resume_drops_turn_mismatch",
                    format!(
                        "{prefix}entries after the resume point are not attributable solely to the requested turn."
                    ),
                ));
            }
        }
        if !preserve
            && array(field(accepted, "messages")).iter().any(|m| {
                array(field(m, "content"))
                    .iter()
                    .any(|b| !is(b, "type", "text"))
            })
        {
            return Err(error(
                "fork_unsupported_content",
                "Forking messages with attachments or non-text input is not supported yet.",
            ));
        }
        let prefill = prompt(accepted);
        let preserved: Vec<_> = entries
            .iter()
            .filter(|e| {
                if !preserve {
                    number(e, "sequence") < number(accepted, "sequence")
                } else {
                    number(e, "sequence") <= number(target, "sequence")
                        || field(e, "turnId") == field(target, "turnId")
                            && (is(e, "type", "turn_result") || is(e, "type", "file_artifacts"))
                }
            })
            .cloned()
            .collect();
        let carried: usize = preserved
            .iter()
            .map(|e| {
                if is(e, "type", "accepted_input") {
                    array(field(e, "messages")).len()
                } else {
                    usize::from(
                        [
                            "assistant_message",
                            "tool_result_message",
                            "durable_message",
                        ]
                        .iter()
                        .any(|t| is(e, "type", t)),
                    )
                }
            })
            .sum();
        let new_key = format!("web:s_{}", Uuid::new_v4());
        let source_key = text(input, "sessionKey");
        let source_key_js = JsString::from(source_key.as_str());
        let new_key_js = JsString::from(new_key.as_str());
        let mut target_input = input.as_object().cloned().unwrap_or_default();
        target_input.insert("sessionKey".into(), new_key.as_str().into());
        let dest = self.storage(&JsValue::Object(target_input))?;
        let mut rewritten: Vec<_> = preserved
            .iter()
            .map(|e| retarget(e, &source_key, &new_key, &source, &dest))
            .collect();
        let preserved_len = rewritten.len();
        let metadata = projection::metadata(&entries);
        let inherited = ["title", "aiTitle"]
            .iter()
            .map(|key| text(&metadata, key))
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        let normalized = prefill.split_whitespace().collect::<Vec<_>>().join(" ");
        let title = if !normalized.is_empty() {
            let units: Vec<_> = normalized.encode_utf16().collect();
            if units.len() > 48 {
                let mut title = "⑂ ".encode_utf16().collect::<Vec<_>>();
                title.extend_from_slice(&units[..48]);
                while title.last().is_some_and(|u| {
                    char::from_u32(*u as u32).is_some_and(crate::metadata::js_space)
                }) {
                    title.pop();
                }
                title.push('…' as u16);
                JsValue::String(JsString::from_utf16(title))
            } else {
                js(format!("⑂ {normalized}"))
            }
        } else if !inherited.is_empty() {
            js(format!("⑂ {inherited}"))
        } else {
            (if carried > 0 {
                "⑂ Forked session"
            } else {
                "⑂ New branch"
            })
            .into()
        };
        let mut meta = JsObject::new();
        meta.insert("parentSessionId".into(), source_key.as_str().into());
        meta.insert("forkedFromTurnId".into(), field(accepted, "turnId").clone());
        meta.insert("title".into(), title);
        meta.insert(
            "firstPrompt".into(),
            if prefill.is_empty() {
                JsValue::Undefined
            } else {
                js(prefill.clone())
            },
        );
        meta.insert("updatedAt".into(), js((self.now)()));
        rewritten.push(metadata_entry(
            &rewritten,
            &new_key,
            format!("fork-{}", Uuid::new_v4()),
            meta,
            &(self.now)(),
        ));
        confined(&self.project(input)?.project_root, &source.tool_results_dir)?;
        confined(&source.chat_dir, &source.file_history_dir)?;
        confined(&source.chat_dir, &source.subagents_dir)?;
        drop(guard);
        let execution_fork = self
            .execution_fork_port
            .as_ref()
            .map(|port| port.begin(source_key_js.clone(), new_key_js.clone()));
        let execution_fork = match execution_fork {
            Some(fork) => Some(fork.await?),
            None => None,
        };
        if let Some(fork) = &execution_fork {
            for entry in &mut rewritten[..preserved_len] {
                *entry = fork.retarget_entry(entry.clone(), &source_key_js, &new_key_js);
            }
        }
        let transform_auxiliary_entry = execution_fork.as_ref().map(|fork| {
            let fork = fork.clone();
            let source_key = source_key_js.clone();
            let target_key = new_key_js.clone();
            Arc::new(move |entry| fork.retarget_entry(entry, &source_key, &target_key))
                as ForkEntryTransform
        });
        let published = if let Some(port) = &self.session_fork_port {
            let project = self.project(input)?;
            port.fork(ProjectSessionForkRequest {
                project_root: project.project_root,
                pilot_home: project.pilot_home,
                source_session_id: source_key_js.clone(),
                target_session_id: new_key_js.clone(),
                entries: rewritten.clone(),
                transform_auxiliary_entry: transform_auxiliary_entry.clone(),
            })
            .await
        } else {
            create_fork(
                &source,
                &dest,
                &rewritten,
                &source_key,
                &new_key,
                transform_auxiliary_entry.as_deref(),
            )
        };
        let committed = match published {
            Ok(()) => match &execution_fork {
                Some(fork) => fork.commit().await,
                None => Ok(()),
            },
            Err(error) => Err(error),
        };
        if let Err(error) = committed {
            if let Some(fork) = &execution_fork {
                fork.rollback().await?;
            }
            return Err(error);
        }
        let mut result = object([
            ("newSessionKey", js(new_key)),
            (
                "prefillText",
                if preserve { "".into() } else { js(prefill) },
            ),
            ("carriedMessageCount", JsValue::Number(carried as f64)),
        ])
        .as_object()
        .cloned()
        .unwrap();
        if is(field(accepted, "metadata"), "permissionMode", "plan") {
            result.insert("mode".into(), "plan".into());
        }
        let run = text(field(accepted, "metadata"), "runMode");
        if ["agent", "plan", "ask"].contains(&run.as_str()) {
            result.insert("runMode".into(), js(run));
        }
        Ok(JsValue::Object(result))
    }

    pub fn prepare(&self, input: &JsValue) -> ReplacementResult {
        for name in ["sessionKey", "expectedTurnId", "replacementTurnId"] {
            required(input, name, "replace_invalid_input")?;
        }
        let _guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let storage = self.storage(input)?;
        self.recover(&storage)?;
        if !artifacts(&storage)?.is_empty() {
            return Err(error(
                "replace_transaction_pending",
                "This session already has a replacement transaction pending.",
            ));
        }
        let read = self.read_entries(&storage, &storage.transcript_path)?;
        if read.diagnostics.iter().any(|d| d.severity == "error") {
            return Err(error(
                "replace_invalid_transcript",
                "The conversation transcript could not be safely rewritten.",
            ));
        }
        let index = read
            .entries
            .iter()
            .rposition(|e| is(e, "type", "accepted_input"))
            .ok_or_else(|| {
                error(
                    "replace_empty_transcript",
                    "No user turn is available to replace.",
                )
            })?;
        let latest = &read.entries[index];
        if field(latest, "turnId") != field(input, "expectedTurnId") {
            return Err(error(
                "replace_turn_conflict",
                "The selected message is no longer the latest user turn.",
            ));
        }
        let mut preserved = read.entries[..index].to_vec();
        let first = !preserved.iter().any(|e| is(e, "type", "accepted_input"));
        if first {
            for entry in &mut preserved {
                if is(entry, "type", "session_metadata") {
                    let meta = without(field(entry, "metadata"), &["aiTitle"]);
                    *entry = set(entry, "metadata", meta);
                }
            }
        }
        let removed_keys: &[&str] = if first {
            &["lastPrompt", "firstPrompt", "aiTitle"]
        } else {
            &["lastPrompt"]
        };
        let mut meta = without(&projection::metadata(&read.entries), removed_keys)
            .as_object()
            .cloned()
            .unwrap_or_default();
        if meta.entries().iter().any(|(key, value)| {
            !key.equals_str("isSnapshot") && !key.equals_str("updatedAt") && !value.is_undefined()
        }) {
            meta.insert("isSnapshot".into(), JsValue::Bool(true));
            meta.insert("updatedAt".into(), js((self.now)()));
            let entry = metadata_entry(
                &preserved,
                &text(input, "sessionKey"),
                "metadata-replace".into(),
                meta,
                &(self.now)(),
            );
            preserved.push(entry);
        }
        let transaction = Uuid::new_v4().to_string();
        let (backup, journal) = paths(&storage, &transaction);
        // Preserve exact source bytes, including harmless whitespace and trailing LF.
        let original = fs::read(&storage.transcript_path).map_err(failure)?;
        let owner = object([
            ("instanceId", js(Uuid::new_v4().to_string())),
            ("pid", JsValue::Number(std::process::id() as f64)),
        ]);
        let journal_value = object([
            ("version", JsValue::Number(2.0)),
            ("transactionId", transaction.as_str().into()),
            ("sessionKey", field(input, "sessionKey").clone()),
            (
                "replacementTurnId",
                field(input, "replacementTurnId").clone(),
            ),
            ("preparedAt", js((self.now)())),
            ("owner", owner),
        ]);
        write_new(&journal, json_bytes(&journal_value)?.as_slice())?;
        let result = (|| {
            write_new(&backup, &original)?;
            sync_dir(&storage.chat_dir)?;
            replace(&storage, &preserved)?;
            sync_dir(&storage.chat_dir)
        })();
        if let Err(e) = result {
            // Keep the only recovery copy if replacement may already be visible.
            if fs::read(&storage.transcript_path).ok().as_deref() == Some(original.as_slice()) {
                let _ = remove(&backup);
                let _ = remove(&journal);
            }
            return Err(e);
        }
        Ok(object([
            ("sessionKey", field(input, "sessionKey").clone()),
            ("replacedTurnId", field(latest, "turnId").clone()),
            (
                "removedEntryCount",
                JsValue::Number((read.entries.len() - index) as f64),
            ),
            ("transactionId", js(transaction)),
        ]))
    }

    pub fn finalize(&self, input: &JsValue) -> ReplacementResult {
        required(input, "sessionKey", "replace_invalid_transaction")?;
        let id = required(input, "transactionId", "replace_invalid_transaction")?
            .to_utf8()
            .map_err(failure)?;
        validate_transaction(&id)?;
        let action = text(input, "action");
        if action != "commit" && action != "rollback" {
            return Err(error(
                "replace_invalid_action",
                "Replacement action must be commit or rollback.",
            ));
        }
        let _guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        let storage = self.storage(input)?;
        let (backup, journal) = paths(&storage, &id);
        confined(&storage.chat_dir, &backup)?;
        confined(&storage.chat_dir, &journal)?;
        if action == "rollback" {
            let original = fs::read(&backup).map_err(|e| {
                error(
                    "replace_transaction_missing",
                    format!("The replacement transaction could not be restored: {e}"),
                )
            })?;
            let read = self.read_entries(&storage, &storage.transcript_path)?;
            let journal_value = read_journal(&journal, &id)?;
            // A durable accepted replacement is the commit point, even after a crash.
            if read.entries.iter().any(|e| {
                is(e, "type", "accepted_input")
                    && field(e, "turnId") == field(&journal_value, "replacementTurnId")
            }) {
                return Err(error(
                    "replace_transaction_active",
                    "The replacement cannot be rolled back after its input was accepted.",
                ));
            }
            atomic_bytes(&storage.transcript_path, &original)?;
        }
        remove(&backup)?;
        remove(&journal)?;
        sync_dir(&storage.chat_dir)?;
        Ok(object([
            ("sessionKey", field(input, "sessionKey").clone()),
            ("transactionId", js(id)),
            ("action", js(action)),
        ]))
    }

    /// Invoke before accepting requests at startup as well as on cold reads.
    pub fn recover_replacements(&self, input: &JsValue) -> Result<(), ReplacementError> {
        let _guard = self.edits.lock().unwrap_or_else(|e| e.into_inner());
        self.recover(&self.storage(input)?)
    }

    pub(super) fn recover(&self, storage: &SessionStorage) -> Result<(), ReplacementError> {
        // TS NodeProjectSessionReplacementPort.ts:85-180,244-248: a bad or
        // unreadable journal is absent evidence, and preparedAt participates in
        // newest-transaction selection alongside both artifact mtimes.
        let mut pending = artifacts(storage)?
            .into_iter()
            .map(|(id, backup, journal)| {
                let value = recovery_journal(&journal, &id);
                let prepared_at = value
                    .as_ref()
                    .and_then(|v| field(v, "preparedAt").as_string())
                    .and_then(|s| {
                        pilotdeck_transport::system_clock::parse_node22_date(s)
                            .ok()
                            .flatten()
                    })
                    .filter(|n| n.is_finite())
                    .unwrap_or(0.0);
                let backup_mtime = artifact_mtime(&backup);
                let order = prepared_at
                    .max(backup_mtime.unwrap_or(0.0))
                    .max(artifact_mtime(&journal).unwrap_or(0.0));
                (id, backup, journal, value, order, backup_mtime.is_some())
            })
            .collect::<Vec<_>>();
        pending.sort_by(|left, right| right.4.total_cmp(&left.4));
        let Some((id, backup, _, journal_value, _, has_backup)) = pending.first() else {
            return Ok(());
        };
        if let Some(journal) = journal_value {
            let pid = number(field(journal, "owner"), "pid");
            if pid > 0.0
                && pid <= i32::MAX as f64
                && pid.fract() == 0.0
                && process_alive(pid as i32)
            {
                return Ok(());
            }
        }
        // Recovery consumes only durable accepted-input evidence, tolerating
        // malformed unrelated JSONL lines and failed reads like TS:121-143.
        let current = recovery_accepted_turn_ids(&storage.transcript_path).unwrap_or_default();
        let committed = if let Some(journal) = journal_value {
            field(journal, "replacementTurnId")
                .as_string()
                .is_some_and(|id| current.contains(id))
        } else {
            recovery_accepted_turn_ids(backup)
                .is_ok_and(|original| current.iter().any(|id| !original.contains(id)))
        };
        if !committed {
            if !has_backup {
                return Err(failure(format!(
                    "Newest replacement transaction {id} has no backup to restore."
                )));
            }
            let original = fs::read(backup).map_err(failure)?;
            atomic_bytes(
                &storage.transcript_path,
                String::from_utf8_lossy(&original).as_bytes(),
            )?;
        }
        for (_, backup, journal, _, _, _) in pending {
            remove(&backup)?;
            remove(&journal)?;
        }
        sync_dir(&storage.chat_dir)
    }
}

fn artifact_mtime(path: &Path) -> Option<f64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    Some(match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs_f64() * 1000.0,
        Err(error) => -error.duration().as_secs_f64() * 1000.0,
    })
}

fn recovery_journal(path: &Path, id: &str) -> Option<JsValue> {
    let bytes = fs::read(path).ok()?;
    let value = parse_json(&String::from_utf8_lossy(&bytes)).ok()?;
    if ![1.0, 2.0].contains(&number(&value, "version"))
        || !is(&value, "transactionId", id)
        || required(&value, "sessionKey", "replace_invalid_transaction").is_err()
        || required(&value, "replacementTurnId", "replace_invalid_transaction").is_err()
        || field(&value, "preparedAt").as_string().is_none()
    {
        return None;
    }
    if number(&value, "version") == 2.0 {
        let owner = field(&value, "owner");
        let pid = field(owner, "pid").as_number()?;
        if owner.as_object().is_none()
            || required(owner, "instanceId", "replace_invalid_transaction").is_err()
            || !(1.0..=9_007_199_254_740_991.0).contains(&pid)
            || pid.fract() != 0.0
        {
            return None;
        }
    }
    Some(value)
}

fn recovery_accepted_turn_ids(path: &Path) -> io::Result<HashSet<JsString>> {
    let bytes = fs::read(path)?;
    let mut ids = HashSet::new();
    for line in String::from_utf8_lossy(&bytes).split('\n') {
        if !line.contains("\"type\":\"accepted_input\"") || !line.contains("\"turnId\"") {
            continue;
        }
        if let Ok(value) = parse_json(line)
            && is(&value, "type", "accepted_input")
            && let Some(id) = field(&value, "turnId").as_string()
        {
            ids.insert(id.clone());
        }
    }
    Ok(ids)
}

fn metadata_entry(
    entries: &[JsValue],
    session: &str,
    turn: String,
    meta: JsObject,
    now: &str,
) -> JsValue {
    object([
        ("type", "session_metadata".into()),
        ("sessionId", session.into()),
        ("turnId", js(turn)),
        (
            "sequence",
            JsValue::Number(
                entries
                    .iter()
                    .map(|e| number(e, "sequence"))
                    .fold(0.0, f64::max)
                    + 1.0,
            ),
        ),
        ("createdAt", now.into()),
        ("entryId", js(Uuid::new_v4().to_string())),
        (
            "parentEntryId",
            entries
                .iter()
                .rev()
                .find_map(|e| {
                    e.get("entryId")
                        .filter(|v| v.as_string().is_some_and(|s| !s.is_empty()))
                        .cloned()
                })
                .unwrap_or(JsValue::Null),
        ),
        ("metadata", JsValue::Object(meta)),
    ])
}
fn prompt(entry: &JsValue) -> String {
    array(field(entry, "messages"))
        .iter()
        .flat_map(|m| array(field(m, "content")))
        .filter(|b| is(b, "type", "text"))
        .map(|b| {
            text(b, "text")
                .trim_matches(crate::metadata::js_space)
                .to_owned()
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}
fn set(value: &JsValue, key: &str, new: JsValue) -> JsValue {
    let mut o = value.as_object().cloned().unwrap_or_default();
    o.insert(key.into(), new);
    JsValue::Object(o)
}
fn without(value: &JsValue, keys: &[&str]) -> JsValue {
    let mut out = JsObject::new();
    if let Some(o) = value.as_object() {
        for (key, value) in o.entries() {
            if !keys.iter().any(|k| key.equals_str(k)) {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    JsValue::Object(out)
}
fn retarget(
    entry: &JsValue,
    source_key: &str,
    target_key: &str,
    source: &SessionStorage,
    dest: &SessionStorage,
) -> JsValue {
    // TS `markTranscriptEntryAsForkCarryover(..., entry.sessionId)` uses the
    // entry's session id before retarget, not the fork request key.
    let source_session = text(entry, "sessionId");
    let source_session = if source_session.is_empty() {
        source_key
    } else {
        source_session.as_str()
    };
    let source_turn = field(entry, "turnId").clone();
    let marks_carryover = is(entry, "type", "accepted_input")
        || is(entry, "type", "assistant_message")
        || is(entry, "type", "tool_result_message")
        || is(entry, "type", "durable_message")
        || (is(entry, "type", "control_boundary")
            && is(field(entry, "boundary"), "kind", "compact")
            && is(field(entry, "boundary"), "subtype", "compact_boundary"));
    let carryover = |message: &JsValue| {
        let mut meta = field(message, "metadata")
            .as_object()
            .cloned()
            .unwrap_or_default();
        meta.insert(
            "forkCarryover".into(),
            object([
                ("sourceSessionId", source_session.into()),
                ("sourceTurnId", source_turn.clone()),
            ]),
        );
        set(message, "metadata", JsValue::Object(meta))
    };
    let mut entry = set(entry, "sessionId", target_key.into());
    if marks_carryover {
        for key in ["messages", "message"] {
            if let Some(value) = entry.get(key).cloned() {
                let value = match &value {
                    JsValue::Array(items) => {
                        JsValue::Array(items.iter().map(&carryover).collect())
                    }
                    value => carryover(value),
                };
                entry = set(&entry, key, value);
            }
        }
        if is(&entry, "type", "control_boundary")
            && is(field(&entry, "boundary"), "kind", "compact")
            && is(field(&entry, "boundary"), "subtype", "compact_boundary")
        {
            let mut boundary = field(&entry, "boundary")
                .as_object()
                .cloned()
                .unwrap_or_default();
            let mut snapshot = field(field(&entry, "boundary"), "snapshot")
                .as_object()
                .cloned()
                .unwrap_or_default();
            if let Some(JsValue::Array(messages)) = snapshot.get("messages") {
                snapshot.insert(
                    "messages".into(),
                    JsValue::Array(messages.iter().map(&carryover).collect()),
                );
                boundary.insert("snapshot".into(), JsValue::Object(snapshot));
                entry = set(&entry, "boundary", JsValue::Object(boundary));
            }
        }
    }
    retarget_paths(&entry, source, dest)
}
fn retarget_paths(value: &JsValue, source: &SessionStorage, dest: &SessionStorage) -> JsValue {
    match value {
        JsValue::Array(a) => {
            JsValue::Array(a.iter().map(|v| retarget_paths(v, source, dest)).collect())
        }
        JsValue::Object(o) => {
            let mut out = JsObject::new();
            for (key, v) in o.entries() {
                let mut new = retarget_paths(v, source, dest);
                if key.equals_str("path")
                    && (is(value, "type", "tool_result_reference")
                        || is(value, "type", "media_reference"))
                {
                    let path = PathBuf::from(text(value, "path"));
                    for (from, to) in [
                        (
                            source.tool_results_dir.as_path(),
                            dest.tool_results_dir.as_path(),
                        ),
                        (
                            source.subagents_dir.parent().unwrap(),
                            dest.subagents_dir.parent().unwrap(),
                        ),
                    ] {
                        if let Ok(relative) = path.strip_prefix(from)
                            && !relative.as_os_str().is_empty()
                        {
                            new = to.join(relative).to_string_lossy().as_ref().into();
                            break;
                        }
                    }
                }
                if key.equals_str("transcriptRelativePath") && is(value, "type", "subagent_started")
                {
                    let path = PathBuf::from(text(value, "transcriptRelativePath"));
                    let safe = source.transcript_path.file_stem().unwrap();
                    if let Ok(relative) = path.strip_prefix(Path::new(safe)) {
                        new = Path::new(dest.transcript_path.file_stem().unwrap())
                            .join(relative)
                            .to_string_lossy()
                            .as_ref()
                            .into();
                    }
                }
                out.insert(key.clone(), new);
            }
            JsValue::Object(out)
        }
        _ => value.clone(),
    }
}
fn create_fork(
    source: &SessionStorage,
    dest: &SessionStorage,
    entries: &[JsValue],
    source_key: &str,
    target_key: &str,
    transform_auxiliary_entry: Option<&(dyn Fn(JsValue) -> JsValue + Send + Sync)>,
) -> Result<(), ReplacementError> {
    let session_dir = dest.subagents_dir.parent().unwrap();
    private_dir(&dest.chat_dir, true)?;
    if dest.transcript_path.exists() || session_dir.exists() || dest.tool_results_dir.exists() {
        return Err(error(
            "fork_target_exists",
            "Fork target session already exists.",
        ));
    }
    private_dir(session_dir, false)?;
    let mut target_reserved = false;
    let mut tools_created = false;
    let copy = ForkCopyContext {
        source,
        dest,
        source_key,
        target_key,
        transform_auxiliary_entry,
    };
    let result = (|| {
        if source.tool_results_dir.exists() {
            let parent = dest.tool_results_dir.parent().unwrap();
            private_dir(parent, true)?;
            private_dir(&dest.tool_results_dir, false)?;
            tools_created = true;
            copy_dir(
                &source.tool_results_dir,
                &dest.tool_results_dir,
                false,
                &copy,
            )?;
        }
        for (from, to, transcripts) in [
            (&source.file_history_dir, &dest.file_history_dir, false),
            (&source.subagents_dir, &dest.subagents_dir, true),
        ] {
            if from.exists() {
                private_dir(to, false)?;
                copy_dir(from, to, transcripts, &copy)?;
            }
        }
        write_new(&dest.transcript_path, &[])?;
        target_reserved = true;
        replace(dest, entries)?;
        sync_dir(&dest.chat_dir)
    })();
    if result.is_err() {
        // Never remove a visible transcript after an uncertain publication.
        if target_reserved && fs::metadata(&dest.transcript_path).is_ok_and(|m| m.len() > 0) {
            return result;
        }
        if target_reserved {
            let _ = remove(&dest.transcript_path);
        }
        let _ = fs::remove_dir_all(session_dir);
        if tools_created {
            let _ = fs::remove_dir_all(&dest.tool_results_dir);
        }
    }
    result
}

struct ForkCopyContext<'a> {
    source: &'a SessionStorage,
    dest: &'a SessionStorage,
    source_key: &'a str,
    target_key: &'a str,
    transform_auxiliary_entry: Option<&'a (dyn Fn(JsValue) -> JsValue + Send + Sync)>,
}

fn copy_dir(
    from: &Path,
    to: &Path,
    transcripts: bool,
    copy: &ForkCopyContext<'_>,
) -> Result<(), ReplacementError> {
    if fs::symlink_metadata(from)
        .map_err(failure)?
        .file_type()
        .is_symlink()
    {
        return Err(failure(
            "Fork auxiliary paths must not traverse symbolic links.",
        ));
    }
    for entry in fs::read_dir(from).map_err(failure)? {
        let entry = entry.map_err(failure)?;
        let ty = entry.file_type().map_err(failure)?;
        let target = to.join(entry.file_name());
        if ty.is_dir() {
            private_dir(&target, false)?;
            copy_dir(&entry.path(), &target, transcripts, copy)?;
        } else if ty.is_file() {
            if transcripts && entry.path().extension().is_some_and(|v| v == "jsonl") {
                let read = copy
                    .source
                    .read_transcript_at_path(&entry.path(), None)
                    .map_err(failure)?;
                if read.diagnostics.iter().any(|d| d.severity == "error") {
                    return Err(error(
                        "fork_invalid_transcript",
                        "Cannot fork a corrupt subagent transcript.",
                    ));
                }
                let entries = read
                    .entries
                    .iter()
                    .map(|v| retarget(v, copy.source_key, copy.target_key, copy.source, copy.dest))
                    .map(|entry| match copy.transform_auxiliary_entry {
                        Some(transform) => transform(entry),
                        None => entry,
                    })
                    .collect::<Vec<_>>();
                let body = entries
                    .iter()
                    .map(json_bytes)
                    .collect::<Result<Vec<_>, _>>()?
                    .concat();
                write_new(&target, &body)?;
            } else {
                write_new(&target, &fs::read(entry.path()).map_err(failure)?)?;
            }
        } else {
            return Err(failure(
                "Fork auxiliary paths must contain only regular files and directories.",
            ));
        }
    }
    Ok(())
}
fn replace(storage: &SessionStorage, entries: &[JsValue]) -> Result<(), ReplacementError> {
    storage
        .replace_transcript(entries)
        .ok_or_else(|| {
            error(
                "fork_unsupported_storage",
                "The configured session storage does not support atomic transcript replacement.",
            )
        })?
        .map_err(failure)
}
fn validate_transaction(id: &str) -> Result<(), ReplacementError> {
    let valid = Uuid::parse_str(id).ok().is_some_and(|id| {
        (1..=5).contains(&id.get_version_num()) && id.get_variant() == uuid::Variant::RFC4122
    });
    if !valid
        || id.len() != 36
        || id.as_bytes().iter().enumerate().any(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                *c != b'-'
            } else {
                !c.is_ascii_hexdigit()
            }
        })
    {
        return Err(error(
            "replace_invalid_transaction",
            "The replacement transaction is invalid.",
        ));
    }
    Ok(())
}
fn paths(storage: &SessionStorage, id: &str) -> (PathBuf, PathBuf) {
    let safe = storage
        .transcript_path
        .file_stem()
        .unwrap()
        .to_string_lossy();
    (
        storage.chat_dir.join(format!(".{safe}.{id}.replace.bak")),
        storage.chat_dir.join(format!(".{safe}.{id}.replace.json")),
    )
}
fn artifacts(
    storage: &SessionStorage,
) -> Result<Vec<(String, PathBuf, PathBuf)>, ReplacementError> {
    let safe = storage
        .transcript_path
        .file_stem()
        .unwrap()
        .to_string_lossy();
    let prefix = format!(".{safe}.");
    let entries = match fs::read_dir(&storage.chat_dir) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(failure(e)),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(failure)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(id) = rest
            .strip_suffix(".replace.bak")
            .or_else(|| rest.strip_suffix(".replace.json"))
        else {
            continue;
        };
        if validate_transaction(id).is_err() || out.iter().any(|(seen, _, _)| seen == id) {
            continue;
        }
        let (backup, journal) = paths(storage, id);
        confined(&storage.chat_dir, &backup)?;
        confined(&storage.chat_dir, &journal)?;
        out.push((id.to_owned(), backup, journal));
    }
    Ok(out)
}
fn read_journal(path: &Path, id: &str) -> Result<JsValue, ReplacementError> {
    let value = parse_json(&fs::read_to_string(path).map_err(failure)?).map_err(failure)?;
    if ![1.0, 2.0].contains(&number(&value, "version"))
        || text(&value, "transactionId") != id
        || text(&value, "replacementTurnId").trim().is_empty()
        || text(&value, "sessionKey").trim().is_empty()
    {
        return Err(error(
            "replace_invalid_transaction",
            "The replacement journal is invalid; backup retained.",
        ));
    }
    Ok(value)
}
fn process_alive(pid: i32) -> bool {
    #[cfg(unix)]
    {
        rustix::process::Pid::from_raw(pid).is_some_and(|pid| {
            match rustix::process::test_kill_process(pid) {
                Ok(()) => true,
                Err(e) => e == rustix::io::Errno::PERM,
            }
        })
    }
    #[cfg(not(unix))]
    {
        pid as u32 == std::process::id()
    }
}
fn json_bytes(value: &JsValue) -> Result<Vec<u8>, ReplacementError> {
    let mut value = stringify_json(value)
        .map_err(failure)?
        .ok_or_else(|| failure("Value is not JSON serializable."))?;
    value.push('\n');
    Ok(value.into_bytes())
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), ReplacementError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(failure)?;
    file.write_all(bytes).map_err(failure)?;
    file.sync_all().map_err(failure)
}
fn atomic_bytes(path: &Path, bytes: &[u8]) -> Result<(), ReplacementError> {
    let temporary = path.with_extension(format!("{}.rollback.tmp", Uuid::new_v4()));
    let result = (|| {
        write_new(&temporary, bytes)?;
        fs::rename(&temporary, path).map_err(failure)?;
        sync_dir(path.parent().unwrap())
    })();
    if result.is_err() {
        let _ = remove(&temporary);
    }
    result
}
fn remove(path: &Path) -> Result<(), ReplacementError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(failure(e)),
    }
}
fn sync_dir(path: &Path) -> Result<(), ReplacementError> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(failure)
}

fn private_dir(path: &Path, recursive: bool) -> Result<(), ReplacementError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(failure)
}
