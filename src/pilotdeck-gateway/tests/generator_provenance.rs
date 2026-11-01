//! Batch migration acceptance: all real TS generators keep their committed bytes.
#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/retry_after_dialects_guard.rs"]
mod retry_after_dialects_guard;

#[test]
fn builtin_tools_glob_transport_keeps_exit_and_error_paths() {
    let output = baseline::node_command()
        .arg("--test")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/_shared/ripgrep-subprocess.test.mjs"
        ))
        .output()
        .expect("#280 run glob subprocess contract");
    assert!(
        output.status.success(),
        "#280 glob subprocess contract failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sqlite_byte_canonicaliser_is_version_insensitive_and_content_strict() {
    let output = baseline::node_command()
        .arg("--test")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/_shared/sqlite_bytes.test.mjs"
        ))
        .output()
        .expect("#979 run SQLite byte canonicaliser tests");
    assert!(
        output.status.success(),
        "#979 SQLite byte canonicaliser failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn provider_timing_deadlines_reject_wrong_order() {
    use baseline::regeneration_masks::validate_provider_timing;
    use serde_json::{Value, json};
    let bytes = include_bytes!("fixtures/provider-timing/oracle.json");
    validate_provider_timing(bytes).expect("#280 all 196 committed timing cells are valid");
    let original: Value = serde_json::from_slice(bytes).expect("#280 parse timing matrix");
    let rows = original["cases"]
        .as_array()
        .expect("#280 timing cases array");
    let mut checked = 0;
    for (index, row) in rows.iter().enumerate() {
        if row["order"] == "synchronous" || row["reachable"] == false {
            continue;
        }
        let timers = row["timers"].as_array().expect("#280 reachable timers");
        let target = if row["event"] != "idle" {
            "completion"
        } else if row["phase"] == "response_text" && row["protocol"] != "google" {
            "body-timeout"
        } else {
            "watchdog"
        };
        let p = timers
            .iter()
            .position(|t| t["kind"] == "parent")
            .expect("#280 parent timer");
        let t = timers
            .iter()
            .position(|t| t["kind"] == target)
            .expect("#280 target timer");
        let target_deadline = timers[t]["start"].as_u64().expect("#280 target start")
            + timers[t]["delay"].as_u64().expect("#280 target delay");
        let parent_delay = timers[p]["delay"].as_u64().expect("#280 parent delay");
        // Each reachable matrix cell rejects both wrong directions (or a tie).
        let offsets: &[i64] = match row["order"].as_str().expect("#280 timing order") {
            "continuation-first" => &[0, -1],
            "parent-first" => &[0, 1],
            _ => &[-1, 1],
        };
        for offset in offsets {
            let mut changed = original.clone();
            changed["cases"][index]["timers"][p]["start"] =
                json!((target_deadline - parent_delay) as i64 + offset);
            let encoded = serde_json::to_vec(&changed).expect("#280 encode wrong deadline");
            let error = validate_provider_timing(&encoded)
                .expect_err("#280 wrong actual deadline must be rejected");
            assert!(
                error.contains("#280 provider-timing actual deadline order:"),
                "#280 deadline failure diagnostic: {error}"
            );
        }
        let mut changed = original.clone();
        changed["cases"][index]["timers"]
            .as_array_mut()
            .expect("#280 mutable timer order")
            .swap(p, t);
        assert!(
            validate_provider_timing(
                &serde_json::to_vec(&changed).expect("#280 encode wrong registration order")
            )
            .is_err(),
            "#280 wrong actual registration order must be rejected"
        );
        checked += 1;
    }
    assert_eq!(checked, 168, "#280 all reachable timer cells checked");
    let mut changed = original.clone();
    changed["cases"][0]["timers"][1]["start"] = json!(0);
    assert!(
        validate_provider_timing(
            &serde_json::to_vec(&changed).expect("#280 encode backwards watchdog")
        )
        .is_err(),
        "#280 other timer cannot start before its predecessor"
    );
}

macro_rules! regeneration {
    ($name:ident, $fixture:literal) => {
        #[test]
        #[ignore = "requires fixed PilotDeck baseline and Node v22.22.0"]
        fn $name() {
            let version = baseline::node_command()
                .arg("--version")
                .output()
                .expect("#280 Node version");
            assert!(version.status.success(), "#280 Node version command failed");
            assert_eq!(
                String::from_utf8_lossy(&version.stdout).trim(),
                "v22.22.0",
                "#280 pinned Node runtime"
            );
            baseline::regenerates_identically($fixture);
        }
    };
}

regeneration!(
    agent_event_projector_regenerates_identically,
    "agent-event-projector"
);
regeneration!(async_hooks_regenerates_identically, "async-hooks");
regeneration!(background_clock_regenerates_identically, "background-clock");
regeneration!(
    background_output_regenerates_identically,
    "background-output"
);
regeneration!(
    background_dispose_flush_regenerates_identically,
    "background-dispose-flush"
);
regeneration!(background_panic_regenerates_identically, "background-panic");
regeneration!(background_tasks_regenerates_identically, "background-tasks");
regeneration!(bash_session_regenerates_identically, "bash-session");
regeneration!(bash_tool_regenerates_identically, "bash-tool");
regeneration!(builtin_tools_regenerates_identically, "builtin-tools");
regeneration!(commands_catalog_regenerates_identically, "commands-catalog");
regeneration!(compaction_regenerates_identically, "compaction");
regeneration!(
    compaction_engine_regenerates_identically,
    "compaction-engine"
);
regeneration!(compaction_input_regenerates_identically, "compaction-input");
regeneration!(
    compaction_orchestrator_regenerates_identically,
    "compaction-orchestrator"
);
regeneration!(
    compaction_snip_micro_regenerates_identically,
    "compaction-snip-micro"
);
regeneration!(config_changes_regenerates_identically, "config-changes");
regeneration!(config_snapshot_regenerates_identically, "config-snapshot");
regeneration!(config_store_regenerates_identically, "config-store");
regeneration!(
    connection_context_regenerates_identically,
    "connection-context"
);
regeneration!(context_prepare_regenerates_identically, "context-prepare");
regeneration!(diff_backfill_regenerates_identically, "diff-backfill");
regeneration!(failure_guard_regenerates_identically, "failure-guard");
regeneration!(file_history_cwd_regenerates_identically, "file-history-cwd");
regeneration!(grep_session_regenerates_identically, "grep-session");
regeneration!(grep_tool_regenerates_identically, "grep-tool");
regeneration!(
    hook_router_timers_regenerates_identically,
    "hook-router-timers"
);
regeneration!(hooks_config_regenerates_identically, "hooks-config");
regeneration!(hooks_https_regenerates_identically, "hooks-https");
regeneration!(hooks_output_regenerates_identically, "hooks-output");
regeneration!(hooks_runtime_regenerates_identically, "hooks-runtime");
regeneration!(host_budget_regenerates_identically, "host-budget");
regeneration!(
    host_cancel_timeout_regenerates_identically,
    "host-cancel-timeout"
);
regeneration!(host_capability_regenerates_identically, "host-capability");
regeneration!(host_context_regenerates_identically, "host-context");
regeneration!(host_event_regenerates_identically, "host-event");
regeneration!(host_event_pump_regenerates_identically, "host-event-pump");
regeneration!(host_factory_regenerates_identically, "host-factory");
regeneration!(host_manifest_regenerates_identically, "host-manifest");
regeneration!(host_model_regenerates_identically, "host-model");
regeneration!(host_model_port_regenerates_identically, "host-model-port");
regeneration!(
    host_module_binding_regenerates_identically,
    "host-module-binding"
);
regeneration!(host_modules_regenerates_identically, "host-modules");
regeneration!(
    host_tool_context_regenerates_identically,
    "host-tool-context"
);
regeneration!(html_markdown_regenerates_identically, "html-markdown");
regeneration!(interaction_bus_regenerates_identically, "interaction-bus");
regeneration!(
    interaction_events_regenerates_identically,
    "interaction-events"
);
regeneration!(
    interaction_permission_regenerates_identically,
    "interaction-permission"
);
regeneration!(
    interaction_values_regenerates_identically,
    "interaction-values"
);
regeneration!(jsonrepair_regenerates_identically, "jsonrepair");
regeneration!(live_history_regenerates_identically, "live-history");
regeneration!(live_recording_regenerates_identically, "live-recording");
regeneration!(mcp_client_regenerates_identically, "mcp-client");
regeneration!(mcp_gateway_regenerates_identically, "mcp-gateway");
regeneration!(mcp_hooks_spawn_regenerates_identically, "mcp-hooks-spawn");
regeneration!(
    mcp_http_encoding_regenerates_identically,
    "mcp-http-encoding"
);
regeneration!(mcp_https_regenerates_identically, "mcp-https");
regeneration!(
    mcp_image_sandbox_regenerates_identically,
    "mcp-image-sandbox"
);
regeneration!(mcp_proxy_regenerates_identically, "mcp-proxy");
regeneration!(
    mcp_proxy_schemes_regenerates_identically,
    "mcp-proxy-schemes"
);
regeneration!(mcp_runtime_regenerates_identically, "mcp-runtime");
regeneration!(
    mcp_worker_boundary_regenerates_identically,
    "mcp-worker-boundary"
);
regeneration!(memory_regenerates_identically, "memory");
regeneration!(
    memory_maintenance_regenerates_identically,
    "memory-maintenance"
);
regeneration!(
    metadata_from_env_regenerates_identically,
    "metadata-from-env"
);
regeneration!(
    metadata_process_cwd_regenerates_identically,
    "metadata-process-cwd"
);
regeneration!(mkdir_boundary_regenerates_identically, "mkdir-boundary");
regeneration!(
    model_catalog_cwd_regenerates_identically,
    "model-catalog-cwd"
);
regeneration!(model_complete_regenerates_identically, "model-complete");
regeneration!(
    model_complete_timeout_regenerates_identically,
    "model-complete-timeout"
);
regeneration!(
    model_complete_transport_regenerates_identically,
    "model-complete-transport"
);
regeneration!(model_cost_regenerates_identically, "model-cost");
regeneration!(model_factory_regenerates_identically, "model-factory");
regeneration!(
    model_http_encoding_regenerates_identically,
    "model-http-encoding"
);
regeneration!(model_stream_regenerates_identically, "model-stream");
regeneration!(
    model_stream_google_regenerates_identically,
    "model-stream-google"
);
regeneration!(
    model_stream_openai_regenerates_identically,
    "model-stream-openai"
);
regeneration!(
    model_stream_responses_regenerates_identically,
    "model-stream-responses"
);
regeneration!(
    model_stream_retry_words_regenerates_identically,
    "model-stream-retry-words"
);
regeneration!(native_judge_regenerates_identically, "native-judge");
regeneration!(
    network_error_order_regenerates_identically,
    "network-error-order"
);
regeneration!(network_timing_regenerates_identically, "network-timing");
regeneration!(operation_ledger_regenerates_identically, "operation-ledger");
regeneration!(permission_regenerates_identically, "permission");
regeneration!(plugin_loader_regenerates_identically, "plugin-loader");
regeneration!(plugin_runtime_regenerates_identically, "plugin-runtime");
regeneration!(private_ca_regenerates_identically, "private-ca");
regeneration!(project_runtime_regenerates_identically, "project-runtime");
regeneration!(
    project_runtime_factory_regenerates_identically,
    "project-runtime-factory"
);
regeneration!(
    provider_sync_abort_regenerates_identically,
    "provider-sync-abort"
);
regeneration!(provider_timing_regenerates_identically, "provider-timing");
regeneration!(
    provider_http_boundaries_regenerates_identically,
    "provider-http-boundaries"
);
regeneration!(pump_order_cases_regenerates_identically, "pump-order-cases");
regeneration!(recorder_regenerates_identically, "recorder");
regeneration!(replacement_regenerates_identically, "replacement");
// #360-2 regen-r2 (fix C): not the shared macro — the runtime provenance has
// two dimensions with different consequences (decision card 2026-10-31). ICU
// stays a hard pin (a different ICU build can change Date.parse itself) and
// must fail, while a differing tzdata is value-neutral for this corpus
// (every local-resolving case sits in Shanghai's flat +08:00 era; the
// homebrew ICU 78.2 / tzdata 2026a re-record reproduced all 62x2 values,
// regen-r1 review) — but since regen-r3 (review P1) a mismatch fails the
// gate with the `#360-2 BLOCKED` panic instead of the old println-and-return
// skip that left cargo green on a host whose byte comparison never ran. The
// gate lives in `common/retry_after_dialects_guard.rs`, shared with the twin
// `model_stream.rs::retry_after_dialects_regenerate_identically`.
#[test]
#[ignore = "requires fixed PilotDeck baseline and Node v22.22.0 with ICU 78.2 on the gold-recorded tzdata"]
fn retry_after_dialects_regenerates_identically() {
    retry_after_dialects_guard::assert_calibrated_runtime_and_regenerate(None);
}
regeneration!(rewind_regenerates_identically, "rewind");
regeneration!(router_decision_regenerates_identically, "router-decision");
regeneration!(router_execute_regenerates_identically, "router-execute");
regeneration!(
    router_timeout_config_regenerates_identically,
    "router-timeout-config"
);
regeneration!(run_registry_regenerates_identically, "run-registry");
regeneration!(
    run_registry_stale_race_regenerates_identically,
    "run-registry-stale-race"
);
regeneration!(runtime_refresh_regenerates_identically, "runtime-refresh");
regeneration!(
    runtime_spawn_panic_regenerates_identically,
    "runtime-spawn-panic"
);
regeneration!(sdk_mcp_regenerates_identically, "sdk-mcp");
regeneration!(sdk_session_regenerates_identically, "sdk-session");
regeneration!(
    session_abort_contract_regenerates_identically,
    "session-abort-contract"
);
regeneration!(
    session_admission_regenerates_identically,
    "session-admission"
);
regeneration!(session_domain_regenerates_identically, "session-domain");
regeneration!(
    session_overrides_regenerates_identically,
    "session-overrides"
);
regeneration!(session_owner_regenerates_identically, "session-owner");
regeneration!(
    session_owner_hooks_regenerates_identically,
    "session-owner-hooks"
);
regeneration!(settings_regenerates_identically, "settings");
regeneration!(sidecar_payload_regenerates_identically, "sidecar-payload");
regeneration!(skills_regenerates_identically, "skills");
regeneration!(skills_external_regenerates_identically, "skills-external");
regeneration!(
    skills_tree_files_regenerates_identically,
    "skills-tree-files"
);
regeneration!(
    snapshot_projection_regenerates_identically,
    "snapshot-projection"
);
regeneration!(spill_isolation_regenerates_identically, "spill-isolation");
regeneration!(spill_lease_regenerates_identically, "spill-lease");
regeneration!(stats_cwd_regenerates_identically, "stats-cwd");
regeneration!(stats_dateparse_regenerates_identically, "stats-dateparse");
regeneration!(steer_regenerates_identically, "steer");
// #132a fix-r1: not the shared macro — the goldens are ICU-sensitive (decision
// card 2026-10-31). Node v22.22.0 exists with ICU 77.1 (source-built ~/opt
// hosts) and 78.2 (homebrew, the recording baseline), so the guard pins the
// ICU build up front and BLOCKs with its own message instead of failing the
// byte comparison as a vague positive-control miss.
#[test]
#[ignore = "requires fixed PilotDeck baseline and Node v22.22.0 with ICU 78.2"]
fn surrogate_collation_matrix_regenerates_identically() {
    let version = baseline::node_command()
        .arg("--version")
        .output()
        .expect("#132a Node version");
    assert!(
        version.status.success(),
        "#132a Node version command failed"
    );
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        "v22.22.0",
        "#132a pinned Node runtime"
    );
    let icu = baseline::node_command()
        .args(["-p", "process.versions.icu"])
        .output()
        .expect("#132a Node ICU probe");
    assert!(icu.status.success(), "#132a Node ICU probe failed");
    assert_eq!(
        String::from_utf8_lossy(&icu.stdout).trim(),
        "78.2",
        "#132a BLOCKED: surrogate-collation-matrix requires ICU 78.2 (homebrew node@22 baseline)"
    );
    baseline::regenerates_identically("surrogate-collation-matrix");
}
regeneration!(task_budget_regenerates_identically, "task-budget");
regeneration!(token_accounting_regenerates_identically, "token-accounting");
regeneration!(token_stats_regenerates_identically, "token-stats");
regeneration!(
    token_stats_sidecar_regenerates_identically,
    "token-stats-sidecar"
);
regeneration!(tool_budget_regenerates_identically, "tool-budget");
regeneration!(tool_budget_cwd_regenerates_identically, "tool-budget-cwd");
regeneration!(tool_runtime_regenerates_identically, "tool-runtime");
regeneration!(
    tool_runtime_values_regenerates_identically,
    "tool-runtime-values"
);
regeneration!(
    transcript_adapter_regenerates_identically,
    "transcript-adapter"
);
regeneration!(
    transcript_resolve_regenerates_identically,
    "transcript-resolve"
);
regeneration!(turn_gates_regenerates_identically, "turn-gates");
regeneration!(turn_gateway_regenerates_identically, "turn-gateway");
regeneration!(
    turn_lifecycle_matrix_regenerates_identically,
    "turn-lifecycle-matrix"
);
regeneration!(turn_publish_regenerates_identically, "turn-publish");
regeneration!(
    turn_prompt_hooks_regenerates_identically,
    "turn-prompt-hooks"
);
regeneration!(turn_read_regenerates_identically, "turn-read");
regeneration!(turn_replay_regenerates_identically, "turn-replay");
regeneration!(turn_runner_regenerates_identically, "turn-runner");
regeneration!(
    turn_start_notices_regenerates_identically,
    "turn-start-notices"
);
regeneration!(turn_submit_regenerates_identically, "turn-submit");
regeneration!(
    turn_submit_admission_regenerates_identically,
    "turn-submit-admission"
);
regeneration!(upload_regenerates_identically, "upload");
regeneration!(upload_gateway_regenerates_identically, "upload-gateway");
regeneration!(user_dialog_regenerates_identically, "user-dialog");
regeneration!(
    user_dialog_channel_regenerates_identically,
    "user-dialog-channel"
);
regeneration!(
    user_dialog_http_store_regenerates_identically,
    "user-dialog-http-store"
);
regeneration!(web_tools_regenerates_identically, "web-tools");
regeneration!(
    web_conversion_lifecycle_regenerates_identically,
    "web-conversion-lifecycle"
);
regeneration!(
    workspace_snapshot_regenerates_identically,
    "workspace-snapshot"
);
regeneration!(write_tools_regenerates_identically, "write-tools");

regeneration!(
    edgeclaw_accessors_regenerates_identically,
    "edgeclaw-accessors"
);
regeneration!(
    edgeclaw_capture_turn_regenerates_identically,
    "edgeclaw-capture-turn"
);
regeneration!(
    edgeclaw_dangling_links_regenerates_identically,
    "edgeclaw-dangling-links"
);
regeneration!(edgeclaw_dream_regenerates_identically, "edgeclaw-dream");
regeneration!(
    edgeclaw_dream_run_regenerates_identically,
    "edgeclaw-dream-run"
);
regeneration!(
    edgeclaw_dream_runner_regenerates_identically,
    "edgeclaw-dream-runner"
);
regeneration!(edgeclaw_entries_regenerates_identically, "edgeclaw-entries");
regeneration!(edgeclaw_export_regenerates_identically, "edgeclaw-export");
regeneration!(
    edgeclaw_file_memory_regenerates_identically,
    "edgeclaw-file-memory"
);
regeneration!(
    edgeclaw_heartbeat_regenerates_identically,
    "edgeclaw-heartbeat"
);
regeneration!(
    edgeclaw_heartbeat_indexer_regenerates_identically,
    "edgeclaw-heartbeat-indexer"
);
regeneration!(edgeclaw_llm_regenerates_identically, "edgeclaw-llm");
regeneration!(
    edgeclaw_llm_config_regenerates_identically,
    "edgeclaw-llm-config"
);
regeneration!(
    edgeclaw_llm_dream_regenerates_identically,
    "edgeclaw-llm-dream"
);
regeneration!(
    edgeclaw_llm_envelope_regenerates_identically,
    "edgeclaw-llm-envelope"
);
regeneration!(
    edgeclaw_llm_fix_r1_regenerates_identically,
    "edgeclaw-llm-fix-r1"
);
regeneration!(
    edgeclaw_llm_index_regenerates_identically,
    "edgeclaw-llm-index"
);
regeneration!(
    edgeclaw_llm_recall_regenerates_identically,
    "edgeclaw-llm-recall"
);
regeneration!(
    edgeclaw_llm_recall_js_sort_regenerates_identically,
    "edgeclaw-llm-recall-js-sort"
);
regeneration!(
    edgeclaw_memory_write_regenerates_identically,
    "edgeclaw-memory-write"
);
regeneration!(
    edgeclaw_messages_regenerates_identically,
    "edgeclaw-messages"
);
regeneration!(
    edgeclaw_retrieval_helpers_regenerates_identically,
    "edgeclaw-retrieval-helpers"
);
regeneration!(
    edgeclaw_retriever_regenerates_identically,
    "edgeclaw-retriever"
);
regeneration!(
    edgeclaw_snapshots_regenerates_identically,
    "edgeclaw-snapshots"
);
regeneration!(edgeclaw_storage_regenerates_identically, "edgeclaw-storage");
regeneration!(edgeclaw_text_regenerates_identically, "edgeclaw-text");
regeneration!(
    edgeclaw_provider_regenerates_identically,
    "edgeclaw-provider"
);
regeneration!(
    edgeclaw_service_facade_regenerates_identically,
    "edgeclaw-service-facade"
);
regeneration!(
    edgeclaw_service_orchestration_regenerates_identically,
    "edgeclaw-service-orchestration"
);

#[test]
fn regeneration_layout_rejects_missing_ambiguous_and_symlink_outputs() {
    use std::fs;
    let root = std::env::temp_dir().join(format!("pdrs-280-layout-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).expect("#280 create layout scratch");
    let read = |fixture| baseline::generated_oracle_bytes(fixture, &root, b"stdout oracle");
    assert!(
        read("fixture")
            .expect_err("#280 zero outputs must fail")
            .contains("found 0"),
        "#280 missing oracle diagnostic"
    );
    fs::write(root.join("oracle.json"), b"golden").expect("#280 direct output");
    assert_eq!(
        read("fixture").expect("#280 direct output readable"),
        b"golden",
        "#280 direct oracle bytes"
    );
    fs::create_dir(root.join("child")).expect("#280 child output directory");
    fs::write(root.join("child/oracle.json"), b"corrupt").expect("#280 ambiguous output");
    assert!(
        read("fixture")
            .expect_err("#280 multiple outputs must fail")
            .contains("found 2"),
        "#280 ambiguous oracle diagnostic"
    );
    fs::remove_file(root.join("oracle.json")).expect("#280 remove owned direct output");
    let generated = read("fixture").expect("#280 child output readable");
    assert_ne!(
        generated, b"golden",
        "#280 byte corruption remains observable"
    );
    fs::rename(
        root.join("child/oracle.json"),
        root.join("rewind-oracle-1.json"),
    )
    .expect("#280 rewind output");
    assert!(
        read("fixture").is_err(),
        "#280 rewind filename is fixture-specific"
    );
    assert_eq!(
        read("rewind").expect("#280 rewind output readable"),
        b"corrupt",
        "#280 rewind oracle bytes"
    );
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(root.join("rewind-oracle-1.json"), root.join("oracle.json"))
            .expect("#280 symlink fixture");
        assert!(
            read("fixture").is_err(),
            "#280 symlink output must not be followed"
        );
        std::os::unix::fs::symlink(root.join("child"), root.join("linked-child"))
            .expect("#280 directory symlink fixture");
        assert_eq!(
            read("rewind").expect("#280 symlink directory skipped"),
            b"corrupt",
            "#280 linked directory ignored"
        );
    }
    for fixture in ["interaction-events", "turn-lifecycle-matrix"] {
        assert_eq!(
            read(fixture).expect("#280 stdout layout"),
            b"stdout oracle",
            "#280 stdout oracle bytes"
        );
    }
    fs::write(root.join("child/oracle.json.tmp"), b"partial").expect("#280 partial output");
    assert!(
        read("rewind")
            .expect_err("#280 nested partial output must fail")
            .contains("oracle.json.tmp"),
        "#280 temporary oracle diagnostic"
    );
    fs::remove_dir_all(root).expect("#280 remove owned layout scratch");
}

#[test]
fn regeneration_companions_reject_missing_corrupt_and_symlink_files() {
    use std::{fs, path::Path};
    let root = std::env::temp_dir().join(format!("pdrs-280-companions-{}", uuid::Uuid::new_v4()));
    let generated = root.join("generated");
    let committed = root.join("committed");
    fs::create_dir_all(&generated).expect("#280 create generated companion directory");
    fs::create_dir(&committed).expect("#280 create committed companion directory");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for (fixture, count) in [
        ("interaction-events", 1),
        ("interaction-permission", 3),
        ("stats-dateparse", 1),
        // Non-macOS generators omit this kernel-dependent companion entirely.
        ("skills-tree-files", usize::from(cfg!(target_os = "macos"))),
        ("user-dialog", 1),
        ("upload", 5),
        ("operation-ledger", 47),
        ("run-registry", 49),
    ] {
        fs::write(
            committed.join("oracle.json"),
            fs::read(fixtures.join(fixture).join("oracle.json"))
                .expect("#280 read independent manifest"),
        )
        .expect("#280 copy manifest into test input directory");
        let names =
            baseline::companion_names(fixture, &committed).expect("#280 companion manifest");
        assert_eq!(
            names.len(),
            count,
            "#280 exact companion count for {fixture}"
        );
        fs::write(
            generated.join("oracle.json"),
            fs::read(committed.join("oracle.json")).expect("#280 read PID manifest"),
        )
        .expect("#280 generated PID manifest");
        for name in &names {
            let bytes = fs::read(fixtures.join(fixture).join(name))
                .expect("#280 read real companion bytes");
            fs::write(committed.join(name), &bytes).expect("#280 expected companion");
            fs::write(generated.join(name), &bytes).expect("#280 actual companion");
        }
        let check = || baseline::compare_companion_files(fixture, &generated, &committed);
        check().expect("#280 all companion bytes match");
        for name in &names {
            fs::remove_file(generated.join(name)).expect("#280 remove owned companion");
            assert!(
                check()
                    .expect_err("#280 missing companion must fail")
                    .contains(name),
                "#280 missing companion diagnostic"
            );
            fs::write(generated.join(name), b"corrupt").expect("#280 corrupt companion");
            assert!(
                check()
                    .expect_err("#280 corrupted companion must fail")
                    .contains("differs"),
                "#280 corrupted companion diagnostic"
            );
            #[cfg(unix)]
            {
                fs::remove_file(generated.join(name)).expect("#280 remove corrupt companion");
                std::os::unix::fs::symlink(committed.join(name), generated.join(name))
                    .expect("#280 companion symlink");
                assert!(
                    check()
                        .expect_err("#280 companion symlink must fail")
                        .contains("regular file"),
                    "#280 companion symlink diagnostic"
                );
                fs::remove_file(generated.join(name)).expect("#280 remove companion symlink");
            }
            fs::write(
                generated.join(name),
                fs::read(committed.join(name)).expect("#280 read companion for restoration"),
            )
            .expect("#280 restore companion");
            check().expect("#280 restored companion matches");
        }
    }
    let original = fs::read_to_string(fixtures.join("operation-ledger/oracle.json"))
        .expect("#280 source manifest");
    for (bad, message) in [
        ("{}".to_owned(), "missing cases array"),
        (
            // Every declared group stays present but empty, so only the
            // companion count is wrong with the #201 groups in the manifest.
            r#"{"cases":[],"rejectedCases":[],"seedCases":[],"corruptCases":[],"deviationCases":[],"utf8Read":{"file":"tail.jsonl"},"domainScope":{"file":"domain-invalid.jsonl"}}"#
                .to_owned(),
            "manifest count",
        ),
        (
            original.replacen("\"normal.jsonl\"", "null", 1),
            "must be a string",
        ),
        (
            original.replacen("\"normal.jsonl\"", "\"../escape.jsonl\"", 1),
            "invalid or duplicate",
        ),
        (
            original.replacen("\"normal.jsonl\"", "\"unknown.jsonl\"", 1),
            "invalid or duplicate",
        ),
    ] {
        fs::write(committed.join("oracle.json"), bad).expect("#280 invalid manifest input");
        assert!(
            baseline::companion_names("operation-ledger", &committed)
                .expect_err("#280 invalid manifest must fail")
                .contains(message),
            "#280 manifest failure diagnostic: {message}"
        );
    }
    fs::remove_dir_all(root).expect("#280 remove owned companion scratch");
}

#[test]
fn regeneration_masks_accept_only_declared_volatile_values() {
    use baseline::regeneration_masks as masks;
    use std::{collections::BTreeSet, fs, path::Path};
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let table = include_str!("fixtures/_shared/regeneration-masks.tsv");
    let mut entries = BTreeSet::new();
    let mut pointers = BTreeSet::new();
    let mut files = BTreeSet::new();
    for line in table.lines().filter(|line| !line.starts_with('#')) {
        let columns: Vec<_> = line.split('\t').collect();
        assert_eq!(columns.len(), 4, "#280 exact mask table columns");
        assert!(entries.insert(line), "#280 duplicate mask table entry");
        // One kind per pointer: a second kind for the same span would relax
        // it silently (GLM #280 r5 P1).
        assert!(
            pointers.insert((columns[0], columns[1], columns[2])),
            "#280 mask table repeats a pointer with another kind"
        );
        assert!(
            matches!(columns[0], "provider-timing" | "run-registry"),
            "#280 masks limited to two fixtures"
        );
        files.insert((columns[0], columns[1]));
    }
    assert_eq!(entries.len(), 836, "#280 fixed mask inventory");
    let registry =
        fs::read(fixtures.join("run-registry/oracle.json")).expect("#280 read PID oracle");
    let old_pid = masks::pid(&registry).expect("#280 committed PID");
    let new_pid = old_pid + 1;
    for (fixture, file) in files {
        let original =
            fs::read(fixtures.join(fixture).join(file)).expect("#280 read masked artifact");
        let rules = masks::rules(fixture, file);
        let spans = masks::scalar_spans(&original);
        let mut changed = original.clone();
        for (pointer, span) in spans.iter().rev() {
            let Some(kind) = rules.get(pointer.as_str()) else {
                continue;
            };
            let token = match *kind {
                "timer" => b"987654321".to_vec(),
                "pid" => new_pid.to_string().into_bytes(),
                "clock" => b"\"2030-01-02T03:04:05.678Z\"".to_vec(),
                _ => panic!("#280 known mask kinds only"),
            };
            changed.splice(span.clone(), token);
        }
        masks::compare(
            fixture,
            file,
            &changed,
            &original,
            (Some(new_pid), Some(old_pid)),
        )
        .unwrap_or_else(|error| panic!("#280 declared volatility accepted: {error}"));
        for (pointer, span) in &spans {
            if !rules.contains_key(pointer.as_str()) {
                continue;
            }
            let mut wrong_type = original.clone();
            wrong_type.splice(span.clone(), b"null".iter().copied());
            assert!(
                masks::normalized(fixture, file, &wrong_type, Some(old_pid)).is_err(),
                "#280 invalid masked scalar rejected: {fixture}/{file}{pointer}"
            );
            let mut missing = original.clone();
            missing.drain(span.clone());
            assert!(
                masks::normalized(fixture, file, &missing, Some(old_pid)).is_err(),
                "#280 missing masked scalar rejected: {fixture}/{file}{pointer}"
            );
            if rules.get(pointer.as_str()) == Some(&"pid") && file != "oracle.json" {
                let mut foreign_pid = original.clone();
                foreign_pid.splice(span.clone(), b"1".iter().copied());
                assert!(
                    masks::normalized(fixture, file, &foreign_pid, Some(old_pid)).is_err(),
                    "#280 foreign owner PID cannot be masked: {file}{pointer}"
                );
            }
        }
        // A same-name field outside the exact path must never join the mask set.
        let extra =
            b"{\"generatorPid\":123,\"start\":1,\"acceptedAt\":\"2030-01-02T03:04:05.678Z\"}";
        assert_eq!(
            masks::normalized(fixture, "unlisted.json", extra, Some(123))
                .expect("#280 unlisted file"),
            extra,
            "#280 unlisted file is byte exact"
        );
        assert_eq!(
            masks::normalized("other-fixture", file, &changed, Some(new_pid))
                .expect("#280 other fixture"),
            changed,
            "#280 other fixture is byte exact"
        );
    }
}

#[test]
fn regeneration_masks_reject_every_unmasked_scalar() {
    use baseline::regeneration_masks as masks;
    use std::{collections::BTreeSet, fs, path::Path};
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let registry =
        fs::read(fixtures.join("run-registry/oracle.json")).expect("#280 read mutation PID oracle");
    let pid = masks::pid(&registry).expect("#280 mutation PID");
    let mut files = BTreeSet::from([
        ("provider-timing", "oracle.json".to_owned()),
        ("run-registry", "oracle.json".to_owned()),
    ]);
    for file in baseline::companion_names("run-registry", &fixtures.join("run-registry"))
        .expect("#280 complete companion inventory")
    {
        files.insert(("run-registry", file));
    }
    let mut rejected = 0;
    for (fixture, file) in files {
        let original =
            fs::read(fixtures.join(fixture).join(&file)).expect("#280 read unmasked artifact");
        let rules = masks::rules(fixture, &file);
        let expected = masks::normalized(fixture, &file, &original, Some(pid))
            .expect("#280 original maskable");
        for (pointer, span) in masks::scalar_spans(&original) {
            if rules.contains_key(pointer.as_str()) {
                continue;
            }
            let mut changed = original.clone();
            // Change one payload byte, preserving valid JSON for strings/numbers.
            let index = span.start + usize::from(original[span.start] == b'"' && span.len() > 2);
            changed[index] = if changed[index] == b'1' {
                b'2'
            } else if changed[index].is_ascii_digit() {
                b'1'
            } else {
                b'X'
            };
            let actual = masks::normalized(fixture, &file, &changed, Some(pid));
            assert!(
                actual.is_err() || actual.as_ref().is_ok_and(|bytes| bytes != &expected),
                "#280 unmasked one-byte corruption rejected: {fixture}/{file}{pointer}"
            );
            rejected += 1;
        }
        // Formatting and malformed trailing bytes are part of the comparison too.
        let mut changed = original.clone();
        changed.push(b' ');
        assert!(
            masks::compare(fixture, &file, &changed, &original, (Some(pid), Some(pid))).is_err(),
            "#280 trailing byte corruption rejected: {fixture}/{file}"
        );
    }
    println!("#280 rejected {rejected} independent unmasked scalar byte mutations");
    assert!(rejected > 3000, "#280 unmasked corpus coverage");
}

#[test]
fn regeneration_masks_reject_invalid_numbers_duplicates_and_bad_tail_changes() {
    use baseline::regeneration_masks as masks;
    use std::{fs, path::Path};
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let provider = fs::read(fixtures.join("provider-timing/oracle.json"))
        .expect("#280 read lexical provider sample");
    let (_, span) = masks::scalar_spans(&provider)
        .into_iter()
        .find(|(pointer, _)| pointer == "/cases/0/timers/0/start")
        .expect("#280 first timer span");
    for invalid in [b"+753".as_slice(), b"0753", b"18446744073709551616"] {
        let mut changed = provider.clone();
        changed.splice(span.clone(), invalid.iter().copied());
        let error = masks::normalized("provider-timing", "oracle.json", &changed, None)
            .expect_err("#280 noncanonical timer number must fail");
        assert!(
            error.contains("invalid timer value"),
            "#280 invalid timer diagnostic: {error}"
        );
    }
    let mut duplicate = provider.clone();
    duplicate.splice(span.start..span.start, b"0, \"start\": ".iter().copied());
    let error = masks::normalized("provider-timing", "oracle.json", &duplicate, None)
        .expect_err("#280 duplicate masked field must fail");
    assert!(
        error.contains("duplicate masked path"),
        "#280 duplicate masked path diagnostic: {error}"
    );
    let registry =
        fs::read(fixtures.join("run-registry/oracle.json")).expect("#280 read tail PID oracle");
    let pid = masks::pid(&registry).expect("#280 tail PID");
    for file in ["bad-tail.json", "bad-tail.after.json"] {
        let original =
            fs::read(fixtures.join("run-registry").join(file)).expect("#280 read malformed tail");
        let mut changed = original.clone();
        let last = changed.last_mut().expect("#280 nonempty malformed tail");
        *last = if *last == b'X' { b'Y' } else { b'X' };
        assert!(
            masks::compare(
                "run-registry",
                file,
                &changed,
                &original,
                (Some(pid), Some(pid))
            )
            .is_err(),
            "#280 unscanned bad-tail byte rejected: {file}"
        );
    }
}

regeneration!(live_session_regenerates_identically, "live-session");

regeneration!(live_e2e_regenerates_identically, "live-e2e");

#[test]
fn live_e2e_companions_reject_missing_corrupt_and_symlink() {
    use std::{fs, path::Path};
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/live-e2e");
    let root = std::env::temp_dir().join(format!("pdrs-748-companions-{}", uuid::Uuid::new_v4()));
    let output = root.join("expected");
    fs::create_dir_all(&output).expect("#748 companion test directory");
    let names =
        baseline::companion_names("live-e2e", &committed).expect("#748 fixed companion inventory");
    assert_eq!(names.len(), 7, "#748 all seven companion files");
    for name in &names {
        fs::write(
            output.join(name),
            fs::read(committed.join("expected").join(name)).expect("#748 read genuine companion"),
        )
        .expect("#748 companion test input");
    }
    let check = || baseline::compare_companion_files("live-e2e", &root, &committed);
    check().expect("#748 exact companion positive");
    for name in &names {
        let path = output.join(name);
        let original = fs::read(&path).expect("#748 owned companion original");
        fs::remove_file(&path).expect("#748 remove test companion");
        assert!(
            check()
                .expect_err("#748 missing companion rejected")
                .contains(name),
            "#748 missing companion diagnostic"
        );
        fs::write(&path, b"corrupt").expect("#748 corrupt test companion");
        assert!(
            check()
                .expect_err("#748 corrupt companion rejected")
                .contains("differs"),
            "#748 full companion bytes"
        );
        fs::remove_file(&path).expect("#748 remove corrupt test companion");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(committed.join("expected").join(name), &path)
                .expect("#748 symlink test companion");
            assert!(
                check()
                    .expect_err("#748 symlink companion rejected")
                    .contains("regular file"),
                "#748 companion file ownership"
            );
            fs::remove_file(&path).expect("#748 remove test symlink");
        }
        fs::write(&path, original).expect("#748 restore companion");
        check().expect("#748 restored companion positive");
    }
    #[cfg(unix)]
    {
        fs::rename(&output, root.join("saved")).expect("#748 move owned companion directory");
        std::os::unix::fs::symlink(root.join("saved"), &output)
            .expect("#748 symlink expected directory");
        assert!(
            check()
                .expect_err("#748 directory symlink rejected")
                .contains("regular directory"),
            "#748 companion directory ownership"
        );
        fs::remove_file(&output).expect("#748 remove test directory symlink");
        fs::rename(root.join("saved"), &output).expect("#748 restore companion directory");
        check().expect("#748 restored directory positive");
    }
    fs::remove_dir_all(root).expect("#748 remove owned companion test directory");
}

regeneration!(live_write_order_regenerates_identically, "live-write-order");

/// #132 step 1: the surrogate-collation-matrix oracle is the recorded
/// original of how Node/ICU (`localeCompare`) orders ill-formed UTF-16 sort
/// keys, taken through the real TS gateway for `model_catalog_list` and
/// same-sequence `session_model_get`. Step 2 will pick the native strategy;
/// these structural checks pin what any candidate must reproduce.
#[test]
fn surrogate_collation_matrix_covers_locale_and_key_classes() {
    use serde_json::value::RawValue;
    use std::collections::HashMap;

    // Every sort key is compared in the JSON-escaped form the oracle stores:
    // a lone surrogate has no Rust `char`, and parsing it as a `String` would
    // fold it to U+FFFD (exactly the ICU4X behaviour #132 distinguishes), so
    // the surrogate-bearing fields stay raw until the ASCII-only checks.
    const ORACLE: &str = include_str!("fixtures/surrogate-collation-matrix/oracle.json");
    // Provenance first (pinned baseline commit + hash table), then the matrix.
    let _ = baseline::oracle_bounded(ORACLE);
    assert!(
        !ORACLE.contains("pdrs-surrogate-matrix") && !ORACLE.contains("/private/var"),
        "#132 oracle must not leak the generator scratch root"
    );
    fn object(raw: &str, what: &str) -> HashMap<String, Box<RawValue>> {
        serde_json::from_str(raw)
            .unwrap_or_else(|error| panic!("#132 {what} must be an object: {error}"))
    }
    fn array(raw: &str, what: &str) -> Vec<Box<RawValue>> {
        serde_json::from_str(raw)
            .unwrap_or_else(|error| panic!("#132 {what} must be an array: {error}"))
    }
    fn text(raw: &str, what: &str) -> String {
        serde_json::from_str(raw)
            .unwrap_or_else(|error| panic!("#132 {what} must be a string: {error}"))
    }
    fn field<'a>(map: &'a HashMap<String, Box<RawValue>>, key: &str, what: &str) -> &'a RawValue {
        map.get(key)
            .unwrap_or_else(|| panic!("#132 {what} must record {key}"))
    }
    // One string token as the oracle stores it: the raw JSON string minus
    // quotes, keeping any surrogate escape verbatim.
    fn unquoted(raw: &RawValue, what: &str) -> String {
        let token = raw.get();
        token
            .strip_prefix('"')
            .and_then(|token| token.strip_suffix('"'))
            .unwrap_or_else(|| panic!("#132 {what} must be a string: {token}"))
            .to_string()
    }
    // Key tokens as the oracle stores them.
    fn unquoted_list(tokens: &RawValue, what: &str) -> Vec<String> {
        array(tokens.get(), what)
            .iter()
            .map(|token| unquoted(token, what))
            .collect()
    }
    // Item order straight from the recorded frame text, keeping the escaped
    // form: only item ids carry the `parity/` prefix, so the anchor cannot
    // hit the frame id or `defaultSelection`.
    fn catalog_order(frame: &str) -> Vec<String> {
        let anchor = "\"id\":\"parity/";
        let mut order = Vec::new();
        let mut rest = frame;
        while let Some(at) = rest.find(anchor) {
            rest = &rest[at + anchor.len()..];
            let end = rest
                .find('"')
                .unwrap_or_else(|| panic!("#132 unterminated catalog id in frame"));
            order.push(rest[..end].to_string());
            rest = &rest[end..];
        }
        order
    }
    // `saved` and `effective` are the only `"model":"w…` occurrences and
    // must agree; both are ASCII so the folded parse cross-checks them.
    fn session_winners(frame: &str) -> Vec<String> {
        let anchor = "\"model\":\"w";
        let mut winners = Vec::new();
        let mut rest = frame;
        while let Some(at) = rest.find(anchor) {
            rest = &rest[at + anchor.len()..];
            let end = rest
                .find('"')
                .unwrap_or_else(|| panic!("#132 unterminated session model in frame"));
            winners.push(format!("w{}", &rest[..end]));
            rest = &rest[end..];
        }
        winners
    }
    let root = object(ORACLE, "oracle root");
    assert_eq!(
        text(field(&root, "node", "oracle").get(), "oracle node"),
        "v22.22.0",
        "#132 pinned Node runtime"
    );
    assert_eq!(
        text(field(&root, "icu", "oracle").get(), "oracle icu"),
        "78.2",
        "#132 pinned ICU version"
    );
    let locales: [(&str, &str); 5] = [
        ("en_US.UTF-8", "en-US"),
        ("zh_CN.UTF-8", "zh-CN"),
        ("de_DE.UTF-8", "de-DE"),
        ("sv_SE.UTF-8", "sv-SE"),
        ("ja_JP.UTF-8", "ja-JP"),
    ];
    let key_classes: [(&str, &[&str]); 10] = [
        ("lone-high", &[r"\ud800", r"\ud801", r"\ud807", r"\udbff"]),
        ("lone-low", &[r"\udc00", r"\udc01", r"\udc07", r"\udfff"]),
        ("band-boundary", &[r"\udbff", r"\udc00", "\u{FFFD}"]),
        ("fffd-vs-pair", &["\u{FFFD}", "😀", "􏿿"]),
        ("valid-pair", &["😀", "😁", "𠀀", "􏿿"]),
        (
            "embedded-fold",
            &[r"a\ud800b", r"a\udc00b", "a\u{FFFD}b", "ab"],
        ),
        (
            "cjk-embedded-fold",
            &[r"中\ud800", r"中\udc00", "中\u{FFFD}", "中"],
        ),
        (
            "pair-interpretation",
            &[r"𐀀", r"\ud800\ud801", r"\udc00\ud800"],
        ),
        (
            "mixed-uniform",
            &[
                r"\ud800", r"\ud801", r"\udc00", r"\udc01", "\u{FFFD}", "😀", "𠀀", "中", "ä", "a",
            ],
        ),
        ("locale-control", &["中文", "a", "ä", "Z"]),
    ];
    let rows = array(field(&root, "locales", "oracle").get(), "oracle locales");
    assert_eq!(
        rows.len(),
        locales.len(),
        "#132 exactly five recorded locales"
    );
    let mut control_orders: Vec<Vec<String>> = Vec::new();
    let mut control_winners: Vec<String> = Vec::new();
    let mut mixed_orders: Vec<Vec<String>> = Vec::new();
    for (raw_row, (lang, resolved)) in rows.iter().zip(locales) {
        let row = object(raw_row.get(), "locale row");
        assert_eq!(
            text(field(&row, "lang", "locale row").get(), "locale lang"),
            lang,
            "#132 recorded locale order must stay pinned"
        );
        assert_eq!(
            text(
                field(&row, "resolvedLocale", "locale row").get(),
                "locale resolvedLocale"
            ),
            resolved,
            "#132 the child locale must resolve as pinned"
        );
        assert_eq!(
            text(field(&row, "node", "locale row").get(), "locale node"),
            "v22.22.0",
            "#132 child Node version"
        );
        assert_eq!(
            text(field(&row, "icu", "locale row").get(), "locale icu"),
            "78.2",
            "#132 child ICU version"
        );
        let cases: Vec<HashMap<String, Box<RawValue>>> =
            array(field(&row, "cases", "locale row").get(), "locale cases")
                .iter()
                .map(|case| object(case.get(), "case"))
                .collect();
        assert_eq!(
            cases.len(),
            key_classes.len() * 2,
            "#132 both gateway methods per key class"
        );
        for (name, keys) in key_classes {
            let catalog = cases
                .iter()
                .find(|case| {
                    text(field(case, "id", "case").get(), "case id") == format!("catalog-{name}")
                })
                .unwrap_or_else(|| panic!("#132 missing catalog-{name} in {lang}"));
            let session = cases
                .iter()
                .find(|case| {
                    text(field(case, "id", "case").get(), "case id") == format!("session-{name}")
                })
                .unwrap_or_else(|| panic!("#132 missing session-{name} in {lang}"));
            assert_eq!(
                text(field(catalog, "method", "case").get(), "case method"),
                "model_catalog_list",
                "#132 catalog-{name} method"
            );
            assert_eq!(
                text(field(session, "method", "case").get(), "case method"),
                "session_model_get",
                "#132 session-{name} method"
            );
            assert_eq!(
                unquoted_list(field(catalog, "keys", "case"), "catalog keys"),
                keys,
                "#132 {name} keys must match the designed key class"
            );
            let catalog_env = object(field(catalog, "env", "case").get(), "catalog env");
            assert_eq!(
                text(
                    field(&catalog_env, "PILOT_HOME", "catalog env").get(),
                    "catalog env PILOT_HOME"
                ),
                "/fixture/home",
                "#132 catalog-{name} env root mapping"
            );
            let catalog_frame_text = text(
                field(catalog, "response", "case").get(),
                "catalog response frame",
            );
            // The frame text keeps lone-surrogate escapes, so it cannot go
            // through a folded `Value` parse; the ASCII facts are checked on
            // the raw text instead.
            assert!(
                catalog_frame_text.starts_with("{\"type\":\"response\","),
                "#132 catalog-{name} frame type"
            );
            assert!(
                catalog_frame_text.contains("\"ok\":true"),
                "#132 catalog-{name} response ok"
            );
            let order = catalog_order(&catalog_frame_text);
            assert_eq!(
                order.len(),
                keys.len(),
                "#132 catalog-{name} item count in {lang}"
            );
            let mut permutation = order.clone();
            permutation.sort_unstable();
            let mut sorted_keys = keys.to_vec();
            sorted_keys.sort_unstable();
            assert_eq!(
                permutation, sorted_keys,
                "#132 catalog-{name} order must be a permutation of the keys"
            );
            // Same-sequence session rows: reversed input order plus distinct
            // w<i> model tags, so the recorded winner exposes which key the
            // comparator ranked last on both methods.
            let entries = array(
                session
                    .get("entries")
                    .unwrap_or_else(|| panic!("#132 session-{name} must record entries"))
                    .get(),
                "session entries",
            );
            assert_eq!(entries.len(), keys.len(), "#132 session-{name} entry count");
            for (i, entry) in entries.iter().enumerate() {
                let entry = object(entry.get(), "session entry");
                let recorded = unquoted(
                    field(&entry, "createdAt", "session entry"),
                    "session entry createdAt",
                );
                assert_eq!(
                    recorded,
                    keys[keys.len() - 1 - i],
                    "#132 session-{name} entries must keep the reversed input order"
                );
                let metadata = object(
                    field(&entry, "metadata", "session entry").get(),
                    "session entry metadata",
                );
                let selection = object(
                    field(&metadata, "modelSelection", "session entry metadata").get(),
                    "session entry modelSelection",
                );
                assert_eq!(
                    text(
                        field(&selection, "model", "session entry modelSelection").get(),
                        "session entry model"
                    ),
                    format!("w{i}"),
                    "#132 session-{name} entry tag"
                );
            }
            let session_env = object(field(session, "env", "case").get(), "session env");
            assert_eq!(
                text(
                    field(&session_env, "PILOT_HOME", "session env").get(),
                    "session env PILOT_HOME"
                ),
                "/fixture/home",
                "#132 session-{name} env root mapping"
            );
            assert_eq!(
                text(
                    session
                        .get("home")
                        .unwrap_or_else(|| panic!("#132 session-{name} must record home"))
                        .get(),
                    "session home"
                ),
                "/fixture/home",
                "#132 session-{name} home root mapping"
            );
            let session_frame_text = text(
                field(session, "response", "case").get(),
                "session response frame",
            );
            assert!(
                session_frame_text.starts_with("{\"type\":\"response\","),
                "#132 session-{name} frame type"
            );
            assert!(
                session_frame_text.contains("\"ok\":true"),
                "#132 session-{name} response ok"
            );
            let winners = session_winners(&session_frame_text);
            assert_eq!(
                winners.len(),
                2,
                "#132 session-{name} saved and effective must both record the winner"
            );
            assert_eq!(
                winners[0], winners[1],
                "#132 session-{name} saved and effective must agree"
            );
            let winner = winners[0].clone();
            let last = order[order.len() - 1].clone();
            let position = keys
                .iter()
                .position(|k| *k == last)
                .unwrap_or_else(|| panic!("#132 session-{name} winner key"));
            let expected = format!("w{}", keys.len() - 1 - position);
            assert_eq!(
                winner, expected,
                "#132 session-{name} winner must agree with the catalog comparator"
            );
            if name == "lone-high" || name == "lone-low" {
                // #132a fix-r1: the catalog inputs are already in the recorded
                // order, so an U+FFFD fold of every lone surrogate leaves this
                // stable order untouched — the fold is what the session winner
                // above detects, not this catalog comparison.
                assert_eq!(
                    order, keys,
                    "#132 lone surrogate catalog order is the recorded code-point order; a U+FFFD fold is caught by the session winner"
                );
            }
            if name == "band-boundary" {
                assert_eq!(
                    order,
                    vec![r"\udbff", r"\udc00", "\u{FFFD}"],
                    "#132 the surrogate band must sort below U+FFFD"
                );
            }
            if name == "fffd-vs-pair" {
                assert_eq!(
                    order,
                    vec!["😀", "\u{10FFFF}", "\u{FFFD}"],
                    "#132 the replacement character must sort after astral code points"
                );
            }
            if name == "valid-pair" {
                assert_eq!(
                    order,
                    vec!["😀", "😁", "𠀀", "\u{10FFFF}"],
                    "#132 valid surrogate pairs must order by code point"
                );
            }
            if name == "embedded-fold" {
                assert_eq!(
                    order,
                    vec!["ab", r"a\ud800b", r"a\udc00b", "a\u{FFFD}b"],
                    "#132 embedded lone surrogates stay distinct and below U+FFFD in the recorded order; a fold tie is caught by the session winner"
                );
            }
            if name == "cjk-embedded-fold" {
                assert_eq!(
                    order,
                    vec!["中", r"中\ud800", r"中\udc00", "中\u{FFFD}"],
                    "#132 cjk-embedded-fold catalog order is the recorded stable order; a U+FFFD fold is caught by the session winner"
                );
            }
            if name == "pair-interpretation" {
                // A UTF-16 code-unit comparator orders the lone-pair case
                // [\ud800\ud801, 𐀀, \udc00\ud800] — same permutation, same
                // winner, only this exact order catches it (#132a fix-r1).
                assert_eq!(
                    order,
                    vec![r"𐀀", r"\ud800\ud801", r"\udc00\ud800"],
                    "#132 a surrogate pair must order by code point, not by UTF-16 code unit"
                );
            }
            if name == "mixed-uniform" {
                // assigned < lone surrogates (code-point order) < U+FFFD, in
                // every locale. The stable sort also keeps this tail under an
                // U+FFFD fold (the input is already in that order), so the
                // session winner is the fold detector here too.
                let tail: Vec<String> = order[order.len() - 5..].to_vec();
                assert_eq!(
                    tail,
                    vec![r"\ud800", r"\ud801", r"\udc00", r"\udc01", "\u{FFFD}"],
                    "#132 assigned-then-surrogate-then-FFFD tail must hold in {lang}"
                );
                mixed_orders.push(order.clone());
            }
            if name == "locale-control" {
                control_orders.push(order.clone());
                control_winners.push(winner);
            }
        }
    }
    // The surrogate-free control proves the child's LANG reached the sort:
    // the orders and therefore the session winners differ across locales.
    assert_ne!(
        control_orders[0], control_orders[1],
        "#132 en_US vs zh_CN control order must differ"
    );
    assert_ne!(
        control_orders[0], control_orders[3],
        "#132 en_US vs sv_SE control order must differ"
    );
    assert_ne!(
        control_orders[1], control_orders[3],
        "#132 zh_CN vs sv_SE control order must differ"
    );
    assert_ne!(
        control_winners[0], control_winners[1],
        "#132 en_US vs zh_CN control winner must differ"
    );
    assert_ne!(
        control_winners[1], control_winners[3],
        "#132 zh_CN vs sv_SE control winner must differ"
    );
    // #132a fix-r1 (review opus P2): de_DE and ja_JP record the same
    // locale-control order as en_US under ICU 78.2, so those rows carry no
    // divergence evidence of their own — pin that expectation (and ja_JP's
    // mixed-uniform divergence, which is its arrival evidence) so a
    // re-recording on a different ICU fails here loudly instead of silently
    // weakening what the control proves. de_DE diverges nowhere in the matrix;
    // its arrival rests on resolvedLocale alone.
    assert_eq!(
        control_orders[2], control_orders[0],
        "#132 de_DE control order must match en_US; its arrival rests on resolvedLocale alone"
    );
    assert_eq!(
        control_orders[4], control_orders[0],
        "#132 ja_JP control order must match en_US (ICU 78.2)"
    );
    assert_ne!(
        mixed_orders[4], mixed_orders[0],
        "#132 ja_JP vs en_US mixed order must differ (\u{4e2d} before \u{20000})"
    );
    assert_eq!(
        mixed_orders[2], mixed_orders[0],
        "#132 de_DE mixed order must match en_US (ICU 78.2)"
    );
}
