#!/usr/bin/env python3
"""#1231 guarded native-route mutations; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact test, and pass after restoration.
"""
from pathlib import Path
import difflib
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, write_source_atomic
from negative_selection import refuse
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_process/management.rs'
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
REQUESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/requests.rs'
# #1352: the advertised capability table moved out of `requests.rs`.
CAPABILITIES = ROOT / 'src/pilotdeck-gateway/src/native_process/capabilities.rs'
CONNECTION = ROOT / 'src/pilotdeck-gateway/src/runtime/connection/mod.rs'
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
ASSEMBLY_MEMORY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly/memory.rs'
ASSEMBLY_SNAPSHOT = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly/snapshot.rs'
ENTRY = ROOT / 'src/pilotdeck-gateway/src/native_process.rs'
METHODS_TOOLS = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/tools.rs'
SDK = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/sdk.rs'
REFRESH = ROOT / 'src/pilotdeck-gateway/src/runtime/refresh.rs'
OWNER = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
SNAPSHOT = ROOT / 'src/pilotdeck-gateway/src/runtime/snapshot.rs'
STORAGE = ROOT / 'src/pilotdeck-gateway/src/storage/workspace_snapshot.rs'
IMPORT = ROOT / 'src/pilotdeck-gateway/src/skills/import.rs'
METADATA = ROOT / 'src/pilotdeck-gateway/src/metadata.rs'
ORACLE = Path(__file__).resolve().parent / 'oracle.json'
TEST = 'native_process::management_tests::route_refresh_reread_matches_ts'
ADMISSION = 'native_process::management_tests::admission_and_unwired_families_stay_closed'
HOST = 'native_process::management_tests::production_host_management_and_restart'
SNAPSHOT_TEST = 'native_process::management_tests::snapshot_success_matches_ts'
IMPORT_TEST = 'native_process::management_tests::skill_import_success_conflict_and_overwrite_match_ts'
PRODUCTION = 'native_process::management_tests::production_entry_snapshot_capabilities_are_disabled'
HELLO = 'native_process::management_tests::hello_reports_the_same_capabilities_as_describe_server'
TABLE = 'native_process::capabilities::tests::capability_methods_are_registered'
WIRING = 'native_process::management_tests::submit_snapshot_wiring_matches_management_workspace_id'
ENVIRONMENT = 'native_process::tests::explicit_options_and_environment'
mutations = [
    ('omit-snapshot', SOURCE, 'if method.starts_with("snapshot_") {', 'if method.starts_with("unwired_snapshot_") {', TEST, '#1231 routed first result'),
    ('omit-manager', SOURCE, 'if matches!(method, "manager_sessions" | "manager_browsers") {', 'if matches!(method, "unwired_manager_sessions" | "manager_browsers") {', TEST, '#1231 routed first result'),
    ('omit-commands', SOURCE, 'return Some(self.commands(params).await);', 'return Some(Err(unavailable()));', TEST, '#1231 routed first result'),
    ('omit-skills', SOURCE, 'let skill = SkillMethod::from_wire(method).ok_or_else(unavailable)?;', 'let skill = SkillMethod::from_wire("unwired_skills").ok_or_else(unavailable)?;', TEST, '#1231 routed first result'),
    ('dirty-not-updated', OWNER, 'if record.context.project_key.as_ref() == Some(project) {\n                record.dirty = !reason.is_empty();', 'if record.context.project_key.as_ref() == Some(project) {\n                record.dirty = reason.is_empty();', TEST, '#1231 dirty flag recreates cached session'),
    ('limit-fraction-truncated', SOURCE, 'n.fract() == 0.0 && (1.0..=100.0).contains(n)', '(1.0..=100.0).contains(n)', TEST, '#1231 method wire result'),
    ('missing-project-defaulted', SOURCE, 'validate_command_project(params)?;', 'if false { validate_command_project(params)?; }', TEST, '#1231 method wire result'),
    ('host-boundary-leaked', SOURCE, 'if e.requires_host {\n        return unavailable();\n    }', 'if false && e.requires_host {\n        return unavailable();\n    }', ADMISSION, '#1231 host-dependent skill rejected'),
    ('surrogate-target-accepted', SOURCE, '!s.to_utf8().is_ok_and(|p| self.options.serves_project(&p))', 's.to_utf8().is_ok_and(|p| !self.options.serves_project(&p))', ADMISSION, '#1231 surrogate restore target admission'),
    ('capability-invented-always_on_apply', CAPABILITIES,
     'Capability { name: "commands_list", methods: &[], wired: compiled_in }',
     'Capability { name: "commands_list", methods: &[], wired: compiled_in },' + '\n    ' + 'Capability { name: "always_on_apply", methods: &[], wired: compiled_in }', HOST, '#1231 exact wired capability set'),
    ('capability-snapshot_list', CAPABILITIES, 'name: "snapshot_list"',
     'name: "unwired_snapshot_list"', HOST, '#1231 exact wired capability set'),
    ('capability-snapshot_get', CAPABILITIES, 'name: "snapshot_get"',
     'name: "unwired_snapshot_get"', HOST, '#1231 exact wired capability set'),
    ('capability-snapshot_restore', CAPABILITIES, 'name: "snapshot_restore"',
     'name: "unwired_snapshot_restore"', HOST, '#1231 exact wired capability set'),
    ('capability-manager_sessions', CAPABILITIES, 'name: "manager_sessions"',
     'name: "unwired_manager_sessions"', HOST, '#1231 exact wired capability set'),
    ('capability-commands_list', CAPABILITIES, 'name: "commands_list"',
     'name: "unwired_commands_list"', HOST, '#1231 exact wired capability set'),
    ('capability-unwired-method-advertised', CAPABILITIES,
     'Capability { name: "commands_list", methods: &[], wired: compiled_in },',
     'Capability { name: "commands_list", methods: &["unwired_skill_scan"], wired: compiled_in },', TABLE, '#1352 commands_list promises a method'),
    # #2622: the transcript-archive pair is served since the port, but by
    # exactly one row — session_transcript_archive. A second row promising
    # export_session_transcript (commands_list here) must not survive.
    ('capability-unserved-history-method', CAPABILITIES,
     'Capability { name: "commands_list", methods: &[], wired: compiled_in },',
     'Capability { name: "commands_list", methods: &["export_session_transcript"], wired: compiled_in },',
     'native_process::capabilities::tests::only_the_archive_row_promises_the_transcript_archive_methods',
     '#2622 only the session_transcript_archive row may promise export_session_transcript'),
    # #2622 restore-side symmetric variant: the same single-row rule for
    # restore_session_transcript (output_styles_list carries the mutant here).
    ('capability-unserved-restore-method', CAPABILITIES,
     'Capability { name: "output_styles_list", methods: &[], wired: compiled_in },',
     'Capability { name: "output_styles_list", methods: &["restore_session_transcript"], wired: compiled_in },',
     'native_process::capabilities::tests::only_the_archive_row_promises_the_transcript_archive_methods',
     '#2622 restore_session_transcript is promised only by the session_transcript_archive row'),
    ('capability-registry-filter-ignored', CAPABILITIES,
     '    promised(capability).all(|method| registered.contains(&method))',
     '    let _ = (capability, registered); false', HOST, '#1231 exact wired capability set'),


    ('skills-restart-refused', ASSEMBLY, 'if management && name == "skills" {', 'if management && name == "unwired_skills" {', HOST, '#1231 native management boot/restart'),
    ('snapshot-storage-omitted', ASSEMBLY, 'let project = project_registry(&options, &env);', 'let mut env = env; env.remove("PILOTDECK_STORAGE_ROOT"); let project = project_registry(&options, &env);', HOST, '#1231 snapshot route with explicit storage'),
    ('oracle-corruption', ORACLE, '"node": "v22.22.0"', '"node": "v0.0.0"', 'native_process::management_tests::regenerates_identically', '#280 native-management/oracle.json differs'),
]
mutations.append(('cursor-array-refused', SOURCE, 'JsValue::Array(items) => encode(items),', 'JsValue::Array(_items) => Err(invalid()),', TEST, '#1231 method wire result'))
mutations.append(('pending-io-blocks-host', SOURCE, '        Some(\n            tokio::task::spawn_blocking(move || {\n                runtime.block_on(requests.management(&method, &params))\n            })\n            .await\n            .map_err(|e| RequestError::new(e.to_string()))\n            .and_then(|result| result),\n        )', '        let _ = runtime; Some(requests.management(&method, &params).await)', 'native_process::management_tests::pending_file_read_does_not_block_serial_host', '#1231 pending file IO must not block host'))

mutations.append(('commands-nested-blocking', SOURCE, 'return Some(self.commands(params).await);', 'let requests = self.clone(); let params = params.clone(); let runtime = tokio::runtime::Handle::current();\n            return Some(tokio::task::spawn_blocking(move || runtime.block_on(requests.commands(&params))).await.map_err(|e| RequestError::new(e.to_string())).and_then(|r| r));', 'native_process::management_tests::management_progress_with_one_available_blocking_thread', '#1231 management progress with one blocking slot'))

mutations.append(('skill-execution-enabled', ASSEMBLY, 'validate_profile_inner(options, env, false).await', 'validate_profile_inner(options, env, true).await', HOST, '#1231 skill management does not enable execution'))

mutations.append(('management-nested-acquire', SOURCE, 'let lease = self\n            .project\n            .acquire(None)\n            .map_err(|e| RequestError::new(e.to_string()))?;', 'let project = self.project.clone();\n        let lease = tokio::task::spawn_blocking(move || project.acquire(None)).await.map_err(|e| RequestError::new(e.to_string()))?.map_err(|e| RequestError::new(e.to_string()))?;', 'native_process::management_tests::management_progress_with_one_available_blocking_thread', '#1231 management progress with one blocking slot'))

mutations.append(('query-surrogate-lossy', SOURCE, 'Some(JsValue::String(text)) => Some(text.clone()),', 'Some(JsValue::String(text)) => Some(String::from_utf16_lossy(text.units()).into()),', TEST, '#1231 method wire result'))

# Move the one notification before invalidate/dirty, preserving payload and count.
mutations.append(('notification-before-dirty', REFRESH, '''        // `if (input?.projectKey)`: an empty key is the all-runtimes branch.
        match input.project_key.filter(|key| !key.is_empty()) {''', '''        broadcast(
            &self.inner,
            &ConfigChangeNotice {
                changed_paths: changed_paths.clone(),
                change_classes: vec!["extension-changed".into()],
            },
        );
        // `if (input?.projectKey)`: an empty key is the all-runtimes branch.
        match input.project_key.filter(|key| !key.is_empty()) {''', TEST, '#1231 invalidate dirty notification order'))

# fix-r1: real unwired methods, contract, storage and successful disk effects.
# #1322 wires memory here (and #1319 wired MCP on main): no family stays
# unwired, so the former `wire-*` / `capability-memory_*` mutants are gone.
mutations.append(('capability-mcp_server_status', CAPABILITIES, 'name: "mcp_server_status"',
     'name: "unwired_mcp_server_status"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-set_mcp_servers', CAPABILITIES, 'name: "set_mcp_servers"',
     'name: "unwired_set_mcp_servers"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-mcp_server_reconnect', CAPABILITIES, 'name: "mcp_server_reconnect"',
     'name: "unwired_mcp_server_reconnect"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-mcp_server_toggle', CAPABILITIES, 'name: "mcp_server_toggle"',
     'name: "unwired_mcp_server_toggle"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-set_mcp_permission_mode_override', CAPABILITIES, 'name: "set_mcp_permission_mode_override"',
     'name: "unwired_set_mcp_permission_mode_override"', HOST, '#1231 exact wired capability set'))
mutations.append(('snapshot-get-list-instead', SOURCE, '"snapshot_get" => gateway.get(params),', '"snapshot_get" => gateway.list(params),', SNAPSHOT_TEST, '#1231 snapshot get success and workspace filter'))
mutations.append(('snapshot-get-workspace-ignored', SNAPSHOT, 'field(manifest, "workspaceId")\n                .as_string()\n                .is_some_and(|id| id.equals_str(&workspace_id))', 'field(manifest, "workspaceId")\n                .as_string()\n                .is_some_and(|id| id.equals_str(&workspace_id) || !workspace_id.is_empty())', SNAPSHOT_TEST, '#1231 snapshot get success and workspace filter'))
mutations.append(('snapshot-restore-list-instead', SOURCE, '"snapshot_restore" => gateway.restore(params),', '"snapshot_restore" => gateway.list(params),', SNAPSHOT_TEST, '#1231 snapshot restore success'))
mutations.append(('snapshot-restore-wrong-target', SNAPSHOT, 'let target_root = make_temp_restore_dir(&target_project)?;', 'let target_root = make_temp_restore_dir(&format!("{target_project}/.pilotdeck"))?;', SNAPSHOT_TEST, '#1231 snapshot restore target parent'))
mutations.append(('skill-import-attachment-skipped', IMPORT, 'for entry in entries.into_iter().rev() {\n            let name = entry.file_name().to_string_lossy().into_owned();', 'for entry in entries.into_iter().rev() {\n            let name = entry.file_name().to_string_lossy().into_owned();\n            if name == "assets" { continue; }', IMPORT_TEST, ['#1231 skill import read list scan and conflict results', '#1231 skill import copies files and preserves conflict target']))
mutations.append(('skill-import-conflict-overwritten', IMPORT, 'if exists && !truthy(field(input, "force")?) {', 'if false && exists && !truthy(field(input, "force")?) {', IMPORT_TEST, '#1231 skill import read list scan and conflict results'))
mutations.append(('skill-import-force-refused', IMPORT, 'if exists && !truthy(field(input, "force")?) {', 'if exists && (truthy(field(input, "force")?) || exists) {', IMPORT_TEST, '#1231 skill import read list scan and conflict results'))

mutations.append(('snapshot-restore-file-empty', STORAGE, 'write_file(Path::new(&output), &contents)?;', 'write_file(Path::new(&output), &contents[..0])?;', SNAPSHOT_TEST, '#1231 snapshot restored file bytes'))

# fix-r2: exact wired set and independently disabled missing/empty roots.
mutations.append(('capability-duplicate-upload_create', CAPABILITIES,
     'Capability { name: "upload_create", methods: &[], wired: compiled_in }',
     'Capability { name: "upload_create", methods: &[], wired: compiled_in },' + '\n    ' + 'Capability { name: "upload_create", methods: &[], wired: compiled_in }', HOST, '#1231 exact wired capability set'))
# #1440: rename-to-unwired controls for the upload_* family, matching the
# snapshot_*/manager_* family rows above.
mutations.append(('capability-upload_create', CAPABILITIES, 'name: "upload_create"',
     'name: "unwired_upload_create"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-upload_get', CAPABILITIES, 'name: "upload_get"',
     'name: "unwired_upload_get"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-upload_part', CAPABILITIES, 'name: "upload_part"',
     'name: "unwired_upload_part"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-upload_complete', CAPABILITIES, 'name: "upload_complete"',
     'name: "unwired_upload_complete"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-upload_cancel', CAPABILITIES, 'name: "upload_cancel"',
     'name: "unwired_upload_cancel"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-duplicate-run_get', CAPABILITIES,
     'Capability { name: "run_get", methods: &[], wired: runs }',
     'Capability { name: "run_get", methods: &[], wired: runs },' + '\n    ' + 'Capability { name: "run_get", methods: &[], wired: runs }', HOST, '#1231 exact wired capability set'))

# #1437: describe_server advertises the two #1309 family capabilities
# (requests.rs user_dialog_list / permission_list); an unwired duplicate
# breaks the exact wired capability set the HOST test pins.
mutations.append(('capability-user_dialog_list', CAPABILITIES, 'name: "user_dialog_list"',
     'name: "unwired_user_dialog_list"', HOST, '#1231 exact wired capability set'))
mutations.append(('capability-permission_list', CAPABILITIES, 'name: "permission_list"',
     'name: "unwired_permission_list"', HOST, '#1231 exact wired capability set'))

# #1456: the three memory-wiring P1s, one control each. The judged behavior is
# removed without breaking compilation; the restored tree must pass again.
mutations.append((
    'memory-error-as-success',
    SOURCE,
    '            Some(MemoryDispatch::Error(cause)) => Err(error(cause.code(), cause.message().clone())),',
    '            Some(MemoryDispatch::Error(cause)) => Ok(cause.to_js()),',
    'native_process::management_tests::memory_management_request_disabled_gateway_returns_unavailable',
    '#1456 a failed memory_list must not be an ok body'))
mutations.append((
    'memory-project-key-exact-only',
    ASSEMBLY_MEMORY,
    '        self.metadata\n'
    '            .resolve_dialog_project(project_key)\n'
    '            .map(JsString::from)\n'
    '            .ok_or_else(|| MemoryProviderError::unknown_project_key(project_key.units()))',
    '        let root = self.options.project.to_str().unwrap_or_default();\n'
    '        if project_key.equals_str(root) {\n'
    '            Ok(project_key.clone())\n'
    '        } else {\n'
    '            Err(MemoryProviderError::unknown_project_key(project_key.units()))\n'
    '        }',
    'native_process::assembly::memory::tests::resolve_project_key_accepts_the_keys_the_snapshot_route_accepts',
    'resolves to the registered project in the TS baseline'))
mutations.append((
    'memory-lease-dropped',
    ASSEMBLY_MEMORY,
    '        Ok(Some(Arc::new(LeasedMemoryManagement {\n'
    '            management: resource.management().clone(),\n'
    '            _lease: lease,\n'
    '        })))',
    '        drop(lease);\n'
    '        Ok(Some(resource.management().clone()))',
    'native_process::assembly::memory::tests::memory_management_holds_the_generation_lease_through_the_call',
    '#1456 the returned service must own a generation lease'))
# #1456 review (two lineages): the widened catalog resolution also accepts
# every *other* registered project and the virtual General workspace; without
# the isolation gate `memory_wipe` with a foreign key clears the served
# project's memory.
mutations.append((
    'memory-foreign-project-served',
    ASSEMBLY_MEMORY,
    '        if !self.serves_project(project_key) {',
    '        if false && !self.serves_project(project_key) {',
    'native_process::assembly::memory::tests::resolve_project_key_accepts_the_keys_the_snapshot_route_accepts',
    'must not reach the served project'))
# #1456 r2 (codex sibling P2): the gate must be *project*-exact. Degrading the
# shared predicate to a textual prefix admits a registered sibling whose
# spelling starts with the served root (`<tmp>/project-2`) and would wipe the
# served project's memory; the prefix-sharing fixture cell must kill it.
mutations.append((
    'memory-foreign-project-served-by-prefix',
    ASSEMBLY_MEMORY,
    '            .is_ok_and(|key| self.options.serves_project(&key))',
    '            .is_ok_and(|key| key\n'
    '                .starts_with(self.options.project.to_string_lossy().as_ref()))',
    'native_process::assembly::memory::tests::resolve_project_key_accepts_the_keys_the_snapshot_route_accepts',
    'must not reach the served project'))
# #1320 fix-r2: resolve_settings is advertised (fix-r1 F7), so the
# rename-to-unwired control keeps the exact wired capability set pinned.
mutations.append(('capability-resolve_settings', CAPABILITIES, 'name: "resolve_settings"',
     'name: "unwired_resolve_settings"', HOST, '#1231 exact wired capability set'))

# #1431 P2-1/P2-2: the submit-side snapshot wiring and the environment branches
# that feed it were merged without paired mutations. Each anchor is unique in
# its file and every mutant must be killed by the bound assertion.
mutations.append(('submit-workspace-id-unwired', SERVICES, 'if self.snapshot_recorder.is_some() {', 'if false {', WIRING, '#1431 submit snapshots enabled'))
mutations.append(('submit-snapshots-unwired', SERVICES, 'self.snapshot_recorder.as_ref().map(|recorder| {', 'self.snapshot_recorder.as_ref().filter(|_| false).map(|recorder| {', WIRING, '#1431 snapshots() exposes the recorder'))
mutations.append(('environment-keeps-snapshot-roots', ENTRY, '                inherited.remove("PILOTDECK_STORAGE_ROOT");\n                inherited.remove("PILOTDECK_SNAPSHOT_ROOT");', '                let _ = ();', ENVIRONMENT, '#1071 inherited authority removed'))
mutations.append(('environment-storage-not-injected', ENTRY, 'inherited.insert("PILOTDECK_STORAGE_ROOT".into(), root.clone());', 'let _ = root;', ENVIRONMENT, '#1431 storage root injected from --storage-root'))
# #1431 review: the id source itself and the two remaining environment
# semantics (ambient storage must lose to --storage-root; the separate
# snapshot root override must survive) had assertions but no mutants.
mutations.append(('submit-workspace-id-legacy-slug', SERVICES, 'projects.workspace_id(&project).as_str()', 'crate::metadata::project_id(std::path::Path::new(&*project)).as_str()', WIRING, '#1431 marker spelling selects the registered workspace id'))
mutations.append(('environment-storage-ambient-wins', ENTRY, 'inherited.insert("PILOTDECK_STORAGE_ROOT".into(), root.clone());', 'inherited.entry("PILOTDECK_STORAGE_ROOT".into()).or_insert(root.clone());', ENVIRONMENT, '#1431 storage root injected from --storage-root'))
mutations.append(('environment-snapshot-root-dropped', ENTRY, 'inherited.insert("PILOTDECK_STORAGE_ROOT".into(), root.clone());', 'inherited.insert("PILOTDECK_STORAGE_ROOT".into(), root.clone());\n                inherited.remove("PILOTDECK_SNAPSHOT_ROOT");', ENVIRONMENT, '#1431 snapshot root override preserved'))
# #1431 review: the recorder extraction in `assembly::snapshot` is the last
# production step the WIRING test calls; without this row it could return
# `None` with every assertion still green.
mutations.append(('submit-recorder-unwired', ASSEMBLY_SNAPSHOT, 'Ok(recorder) => Some(recorder),', 'Ok(_) => None,', WIRING, '#1431 staged snapshot recorder'))
# #1761 item 4: pair for the settings capability guard's "real delegate but
# never advertised" half. `production_host_management_and_restart` also fails
# on this advertisement (exact wired set), but the settings guard owns the
# assertion that names the rule — TS publishes neither reload in any config
# state, so advertising one breaks the TS-subset contract — so the mutant is
# bound to that test and marker.
SETTINGS_CAPABILITIES = 'native_process::management_tests::settings_capabilities_match_served_delegates'
mutations.append(('capability-add-reload_config', CAPABILITIES,
     'Capability { name: "resolve_settings", methods: &[], wired: compiled_in }',
     'Capability { name: "resolve_settings", methods: &[], wired: compiled_in },' + '\n    ' + 'Capability { name: "reload_config", methods: &[], wired: compiled_in }', SETTINGS_CAPABILITIES, '#1320 settings method reload_config must stay unadvertised'))
# #1761 review: the guard loops over BOTH reloads, so the advertisement half
# needs a reload_extensions mutant too; otherwise the second iteration has a
# positive assertion with no negative control under it.
mutations.append(('capability-add-reload_extensions', CAPABILITIES,
     'Capability { name: "resolve_settings", methods: &[], wired: compiled_in }',
     'Capability { name: "resolve_settings", methods: &[], wired: compiled_in },' + '\n    ' + 'Capability { name: "reload_extensions", methods: &[], wired: compiled_in }', SETTINGS_CAPABILITIES, '#1320 settings method reload_extensions must stay unadvertised'))
mutations.append(('missing-storage-defaulted', ASSEMBLY, '    register_project(&options)?;', '    let mut env = env; env.entry("PILOTDECK_STORAGE_ROOT".into()).or_insert_with(|| options.home.join("storage").to_string_lossy().into_owned());\n    register_project(&options)?;', 'native_process::management_tests::missing_storage_environment_disables_snapshots', '#1231 disabled storage snapshot result'))
mutations.append(('empty-storage-defaulted', ASSEMBLY, '    register_project(&options)?;', '    let mut env = env; if env.get("PILOTDECK_STORAGE_ROOT").is_some_and(|value| value.is_empty()) { env.insert("PILOTDECK_STORAGE_ROOT".into(), options.home.join("storage").to_string_lossy().into_owned()); }\n    register_project(&options)?;', 'native_process::management_tests::empty_storage_environment_disables_snapshots', '#1231 disabled storage snapshot result'))
mutations.append(('missing-storage-advertised', CAPABILITIES,
                  'fn snapshots(requests: &Requests) -> bool {\n    requests.snapshot_enabled\n}',
                  'fn snapshots(_requests: &Requests) -> bool {\n    true\n}', 'native_process::management_tests::missing_storage_environment_disables_snapshots', '#1231 disabled storage exact capability set'))
mutations.append(('empty-storage-advertised', CAPABILITIES,
                  'fn snapshots(requests: &Requests) -> bool {\n    requests.snapshot_enabled\n}',
                  'fn snapshots(_requests: &Requests) -> bool {\n    true\n}', 'native_process::management_tests::empty_storage_environment_disables_snapshots', '#1231 disabled storage exact capability set'))


# fix-r3: provider-derived advertisement, explicit filter and production boundary.
mutations.append(('snapshot-state-disabled', ASSEMBLY_SNAPSHOT, 'Ok(persistence.snapshots.is_some())', 'Ok(persistence.snapshots.is_some() && false)', HOST, '#1231 exact wired capability set'))
mutations.append(('snapshot-state-enabled', ASSEMBLY_SNAPSHOT, 'Ok(persistence.snapshots.is_some())', 'Ok(persistence.snapshots.is_some() || true)', PRODUCTION, '#1231 production entry starts without snapshots'))
mutations.append(('production-storage-authority-accepted', ENTRY, '        "PILOTDECK_STORAGE_ROOT",', '        "UNWIRED_PILOTDECK_STORAGE_ROOT",', PRODUCTION, '#1231 production entry rejects inherited snapshot authority'))
mutations.append(('production-snapshot-authority-accepted', ENTRY, '        "PILOTDECK_SNAPSHOT_ROOT",', '        "UNWIRED_PILOTDECK_SNAPSHOT_ROOT",', PRODUCTION, '#1231 production entry rejects inherited snapshot authority'))
mutations.append(('production-blank-authority-accepted', ENTRY, 'env.get(key).is_some_and(|v| !v.is_empty())', 'env.get(key).is_some_and(|v| !v.trim().is_empty())', PRODUCTION, '#1231 production entry rejects inherited snapshot authority'))
mutations.append(('snapshot-list-provider-ignored', CAPABILITIES,
                  'Capability { name: "snapshot_list", methods: &[], wired: snapshots }',
                  'Capability { name: "snapshot_list", methods: &[], wired: compiled_in }', PRODUCTION, '#1231 production entry exact capability set'))
mutations.append(('snapshot-get-provider-ignored', CAPABILITIES,
                  'Capability { name: "snapshot_get", methods: &[], wired: snapshots }',
                  'Capability { name: "snapshot_get", methods: &[], wired: compiled_in }', PRODUCTION, '#1231 production entry exact capability set'))
mutations.append(('snapshot-restore-provider-ignored', CAPABILITIES,
                  'Capability { name: "snapshot_restore", methods: &[], wired: snapshots }',
                  'Capability { name: "snapshot_restore", methods: &[], wired: compiled_in }', PRODUCTION, '#1231 production entry exact capability set'))
# #1352: `expected` is now derived from `contract`, so a rename inside
# `contract` moves both sides of the old subset comparison. This test's guard
# is the exact-set assert (it also owns the `#1231 exact wired capability set`
# marker); the disabled-state subset check belongs to the two tests below.
mutations.append(('wired-capabilities-outside-ts', ORACLE, '\n      "run_get",', '\n      "unwired_run_get",', HOST, '#1231 exact wired capability set'))
# #1869 item 2: `assert_ts_capability_subset` also checks
# `contract.emptyStorage` / `contract.missingStorage`, but the only ORACLE
# mutant touched the default `contract` array — deleting either disabled
# branch kept every existing mutant killed. One mutant per disabled state,
# anchored on that state's own header so the default contract stays intact.
# #1352: the subset check no longer reads `contract` at all (a tautology now
# that `expected` derives from it), so these two are its only remaining
# witnesses.
mutations.append(('empty-storage-capabilities-outside-ts', ORACLE,
                  '    "emptyStorage": {\n      "config": null,\n      "capabilities": [\n        "upload_create",',
                  '    "emptyStorage": {\n      "config": null,\n      "capabilities": [\n        "unwired_upload_create",',
                  'native_process::management_tests::empty_storage_environment_disables_snapshots',
                  '#1231 wired capabilities are TS subset (contract.emptyStorage)'))
mutations.append(('missing-storage-capabilities-outside-ts', ORACLE,
                  '    "missingStorage": {\n      "config": null,\n      "capabilities": [\n        "upload_create",',
                  '    "missingStorage": {\n      "config": null,\n      "capabilities": [\n        "unwired_upload_create",',
                  'native_process::management_tests::missing_storage_environment_disables_snapshots',
                  '#1231 wired capabilities are TS subset (contract.missingStorage)'))

# #1352: `hello_ok.serverInfo` must be the `describe_server` result itself; a
# second, independently built advertisement fails the same-source test.
mutations.append(('hello-serverinfo-independent', CONNECTION,
                  'out.insert("serverInfo".into(), info);',
                  'out.insert("serverInfo".into(), JsValue::Object(JsObject::new()));',
                  HELLO, '#1352 hello_ok.serverInfo equals the describe_server result'))

# fix-r2 (#1322): memory IO must stay off the serial host.
mutations.append(('memory-io-blocks-host', SOURCE, 'tokio::task::spawn_blocking(move || requests.memory_dispatch(&method, &params))\n                    .await\n                    .map_err(|e| RequestError::new(e.to_string()))\n                    .and_then(|result| result),', 'std::future::ready(requests.memory_dispatch(&method, &params))\n                    .await,', 'native_process::management_tests::pending_memory_io_does_not_block_serial_host', '#1322 pending memory IO must not block host'))
# fix-r3 (#1322): inline-sync variant — calls memory_dispatch directly on the
# calling thread (no spawn_blocking), which blocks the serial host.
mutations.append(('memory-inline-sync', SOURCE, 'tokio::task::spawn_blocking(move || requests.memory_dispatch(&method, &params))\n                    .await\n                    .map_err(|e| RequestError::new(e.to_string()))\n                    .and_then(|result| result),', 'requests.memory_dispatch(&method, &params),', 'native_process::management_tests::pending_memory_io_does_not_block_serial_host', '#1322 pending memory IO must not block host'))

# #1431 P2-3 + 收尾自查残差: two production steps had assertions but no paired
# mutation. `non-utf8-project-accepted` reopens the unreachable-workspace path a
# non-UTF-8 `--project` used to take; `snapshot-list-resolver-bypassed` proves the
# management route really resolves the workspace id through `SnapshotProjects`
# (the marker branch), which is what makes the submit-captured snapshot visible.
# #2036 moved the `--project` rejection into the shared `utf8_argument` helper,
# so the anchor follows the call site; dropping the call reopens the
# unreachable-workspace path exactly as before.
mutations.append(('non-utf8-project-accepted', ENTRY,
                  '        utf8_argument(project.as_os_str(), "project")?;',
                  '        let _ = project.as_os_str();',
                  'native_process::tests::non_utf8_project_path_is_refused',
                  '#1431 non-UTF-8 --project must be rejected'))
mutations.append(('snapshot-list-resolver-bypassed', SNAPSHOT,
                  '        let project_key = self.resolve_source_project(input)?;\n        let workspace_id = self.resolver.workspace_id(&project_key);\n        let limit = snapshot_limit(field(input, "limit"))?;',
                  '        let project_key = self.resolve_source_project(input)?;\n        let workspace_id = crate::metadata::project_id(std::path::Path::new(&project_key));\n        let limit = snapshot_limit(field(input, "limit"))?;',
                  WIRING, '#1431 snapshot_list returns the submit-captured snapshot'))
# #1431 review (both lineages): `snapshot_get` / `snapshot_restore` resolve the
# same management-side workspace id (same call, `source_project` name in
# restore) and had assertions but no paired mutation, so swapping those two for
# the legacy slug would only be caught by the oracle replay.
mutations.append(('snapshot-get-resolver-bypassed', SNAPSHOT,
                  '        let project_key = self.resolve_source_project(input)?;\n        let workspace_id = self.resolver.workspace_id(&project_key);\n        let snapshot_id = field(input, "snapshotId");',
                  '        let project_key = self.resolve_source_project(input)?;\n        let workspace_id = crate::metadata::project_id(std::path::Path::new(&project_key));\n        let snapshot_id = field(input, "snapshotId");',
                  SNAPSHOT_TEST, '#1231 snapshot get success and workspace filter'))
mutations.append(('snapshot-restore-resolver-bypassed', SNAPSHOT,
                  '        let workspace_id = self.resolver.workspace_id(&source_project);\n        let snapshot_id = field(input, "snapshotId");',
                  '        let workspace_id = crate::metadata::project_id(std::path::Path::new(&source_project));\n        let snapshot_id = field(input, "snapshotId");',
                  SNAPSHOT_TEST, '#1231 snapshot restore success'))
# #1431 review r2 (GLM sibling): the `--storage-root` rejection already had a
# direct test (`storage_root_non_utf8_is_hard_parse_error`) but no paired
# mutation, so deleting the check was only caught by an ordinary test — the same
# "assertion without a mutation" gap this PR closed for `--project`. The mutant
# makes the UTF-8 check fall back instead of failing, so `Options::parse`
# returns Ok and the test dies on its own `expect_err` message. (#2036 moved the
# check into `utf8_argument`, so the anchor follows the call.)
mutations.append(('storage-root-nonutf8-accepted', ENTRY,
                  '                .map(|v| utf8_argument(v, "storage root"))',
                  '                .map(|v| utf8_argument(v, "storage root").or(Ok::<&str, ProxyError>("lost")))',
                  'native_process::management_tests::storage_root_non_utf8_is_hard_parse_error',
                  '#1323 non-UTF-8 --storage-root must be rejected'))

# #2036: `--home` and `--sidecar` now share `utf8_argument` with `--project` /
# `--storage-root`. Each call site needs its own paired mutation (deleting the
# call accepts the raw bytes and `Options` then hands children a lossy spelling
# of a different directory), plus one mutant that bypasses the helper at a call
# site entirely — the lossy fallback compiles and is exactly the old bug, so it
# proves the helper is what does the work there.
mutations.append(('home-non-utf8-accepted', ENTRY,
                  '        utf8_argument(home.as_os_str(), "home")?;',
                  '        let _ = home.as_os_str();',
                  'native_process::tests::non_utf8_home_and_sidecar_are_refused',
                  '#2036 non-UTF-8 --home must be rejected'))
mutations.append(('sidecar-non-utf8-accepted', ENTRY,
                  '        utf8_argument(sidecar.as_os_str(), "sidecar")?;',
                  '        let _ = sidecar.as_os_str();',
                  'native_process::tests::non_utf8_home_and_sidecar_are_refused',
                  '#2036 non-UTF-8 --sidecar must be rejected'))
mutations.append(('argv-utf8-helper-bypassed', ENTRY,
                  '                .map(|v| utf8_argument(v, "storage root"))\n                .transpose()?\n                .map(str::to_owned),',
                  '                .map(|v| v.to_string_lossy().into_owned()),',
                  'native_process::management_tests::storage_root_non_utf8_is_hard_parse_error',
                  '#1323 non-UTF-8 --storage-root must be rejected'))

# #2037: `Metadata::project_storage_id` (the turn/management snapshot workspace
# id, `resolveProjectStorageId`) is the resolver behind `SnapshotProjects`: the
# registered `.cwd` marker wins, the legacy path slug is only the fallback.
# `snapshot_workspace_id_prefers_registered_marker` asserted both halves, but no
# row in this table targeted `metadata.rs`: `snapshot-projection` does mutate the
# same marker branch (`storage-id-ignores-marker`), yet it binds `#888 marker
# owns storage id`, not this test — the same "assertion without a bound
# mutation" gap #1431 closed for the three management routes. Two halves, one
# mutant each, plus the trim rule `project_id` applies to the fallback. All
# three anchors occur exactly once in `metadata.rs`, and every marker below is
# the literal in the bound test's panic payload.
mutations.append(('project-storage-id-marker-bypassed', METADATA,
                  '            if marker_path == target\n                && self',
                  '            if false && marker_path == target\n                && self',
                  'native_process::management_tests::snapshot_workspace_id_prefers_registered_marker',
                  '#1323 registered marker wins'))
mutations.append(('project-storage-id-legacy-slug-omitted', METADATA,
                  '\n        project_id(&target)\n',
                  '\n        target.to_string_lossy().into_owned()\n',
                  'native_process::management_tests::snapshot_workspace_id_prefers_registered_marker',
                  '#1323 unregistered project uses the legacy slug'))
# `project_id` is that fallback (and the storage dir of every unregistered
# project); its trim rule had a direct test but no mutation of its own.
mutations.append(('legacy-project-id-trim-omitted', METADATA,
                  "    let result = result.trim_matches('-');",
                  '    let result = result;',
                  'native_process::management_tests::legacy_project_id_trims_leading_dashes',
                  '#1323 legacy slug of'))
# #1432: the status route must read the shared config #1319 writes. Rolling
# the routed body back to the #1311 constant empty list must fail the routed
# set→status test; the capability-name rows above only pin the method being
# wired, not that it returns stored state. Each row's assertion is its own
# bound test's panic payload (`classify` only accepts those); the disabled
# read-back has its own row bound to its own test.
mutations.append(('mcp-status-constant-empty', METHODS_TOOLS,
                  '        self.mcp\n'
                  '            .config()\n'
                  '            .mcp_server_status(params)\n'
                  '            .await\n'
                  '            .map_err(super::mcp::mcp_error)',
                  '        Ok(crate::metadata::object([("servers", JsValue::Array(vec![]))]))',
                  'native_process::mcp_status_tests::set_then_status_reports_the_session_servers',
                  '#1432 the same session must read its stored server back as configured'))
# #1432 r5（boundary P2）：上一行的常量空表变异对两个绑定测试都杀；disabled 行
# 改为互异变异——status_result 的 enabled→status 投影被硬编成恒 "configured"，
# 由 disabled 回读测试单独判杀（set→status 回读仍由上行覆盖）。
mutations.append(('mcp-status-disabled-spelling-flattened', SDK,
                  '                            if *configured {\n'
                  '                                "configured"\n'
                  '                            } else {\n'
                  '                                "disabled"\n'
                  '                            }',
                  '                            "configured"',
                  'native_process::mcp_status_tests::a_toggled_server_reports_disabled',
                  '#1432 a disabled server must read back as disabled'))

# #2036: `--home` / `--sidecar` are canonicalised before the rejection, so their
# only bound test has to create a raw-named entry on disk. APFS / HFS+ refuse
# those bytes with EILSEQ, the test skips itself there (#1400 probe), and a
# skipped test reads exactly like a passing one to `classify` — the two rows
# below would be SURVIVED on every darwin host. Same shape as cron-store's
# `LINUX_ONLY` rows: off Linux they are recorded SKIPPED (neither KILLED nor
# INVALID) so a default full run does not fail on a control this host
# structurally cannot kill; the real kill evidence comes from Linux (cb8).
LINUX_ONLY = {'home-non-utf8-accepted', 'sidecar-non-utf8-accepted'}


def skipped_on_this_platform(rows, platform=None):
    """#2036: drop the LINUX_ONLY rows where their bound test skips itself.

    Kept as a function so the accounting can be exercised without a filesystem
    that refuses raw names (import the module and call it with `platform`).
    """
    platform = sys.platform if platform is None else platform
    if platform == 'linux':
        return []
    return [case for case in rows if case.mutation in LINUX_ONLY]


# #1538/#1869 review: the table used to be its own denominator — delete a row
# and the run still reported "{n}/{n}" and read green. Pin the row count and the
# roster (order and repeats included) as literals so a dropped, renamed or
# duplicated mutation is refused before anything runs or compiles.
EXPECTED_TOTAL = 98
EXPECTED_NAMES = (
    'omit-snapshot',
    'omit-manager',
    'omit-commands',
    'omit-skills',
    'dirty-not-updated',
    'limit-fraction-truncated',
    'missing-project-defaulted',
    'host-boundary-leaked',
    'surrogate-target-accepted',
    'capability-invented-always_on_apply',
    'capability-snapshot_list',
    'capability-snapshot_get',
    'capability-snapshot_restore',
    'capability-manager_sessions',
    'capability-commands_list',
    'capability-unwired-method-advertised',
    'capability-unserved-history-method',
    'capability-unserved-restore-method',
    'capability-registry-filter-ignored',
    'skills-restart-refused',
    'snapshot-storage-omitted',
    'oracle-corruption',
    'cursor-array-refused',
    'pending-io-blocks-host',
    'commands-nested-blocking',
    'skill-execution-enabled',
    'management-nested-acquire',
    'query-surrogate-lossy',
    'notification-before-dirty',
    'capability-mcp_server_status',
    'capability-set_mcp_servers',
    'capability-mcp_server_reconnect',
    'capability-mcp_server_toggle',
    'capability-set_mcp_permission_mode_override',
    'snapshot-get-list-instead',
    'snapshot-get-workspace-ignored',
    'snapshot-restore-list-instead',
    'snapshot-restore-wrong-target',
    'skill-import-attachment-skipped',
    'skill-import-conflict-overwritten',
    'skill-import-force-refused',
    'snapshot-restore-file-empty',
    'capability-duplicate-upload_create',
    'capability-upload_create',
    'capability-upload_get',
    'capability-upload_part',
    'capability-upload_complete',
    'capability-upload_cancel',
    'capability-duplicate-run_get',
    'capability-user_dialog_list',
    'capability-permission_list',
    'memory-error-as-success',
    'memory-project-key-exact-only',
    'memory-lease-dropped',
    'memory-foreign-project-served',
    'memory-foreign-project-served-by-prefix',
    'capability-resolve_settings',
    'submit-workspace-id-unwired',
    'submit-snapshots-unwired',
    'environment-keeps-snapshot-roots',
    'environment-storage-not-injected',
    'submit-workspace-id-legacy-slug',
    'environment-storage-ambient-wins',
    'environment-snapshot-root-dropped',
    'submit-recorder-unwired',
    'capability-add-reload_config',
    'capability-add-reload_extensions',
    'missing-storage-defaulted',
    'empty-storage-defaulted',
    'missing-storage-advertised',
    'empty-storage-advertised',
    'snapshot-state-disabled',
    'snapshot-state-enabled',
    'production-storage-authority-accepted',
    'production-snapshot-authority-accepted',
    'production-blank-authority-accepted',
    'snapshot-list-provider-ignored',
    'snapshot-get-provider-ignored',
    'snapshot-restore-provider-ignored',
    'wired-capabilities-outside-ts',
    'empty-storage-capabilities-outside-ts',
    'missing-storage-capabilities-outside-ts',
    'hello-serverinfo-independent',
    'memory-io-blocks-host',
    'memory-inline-sync',
    'non-utf8-project-accepted',
    'snapshot-list-resolver-bypassed',
    'snapshot-get-resolver-bypassed',
    'snapshot-restore-resolver-bypassed',
    'storage-root-nonutf8-accepted',
    'home-non-utf8-accepted',
    'sidecar-non-utf8-accepted',
    'argv-utf8-helper-bypassed',
    'project-storage-id-marker-bypassed',
    'project-storage-id-legacy-slug-omitted',
    'legacy-project-id-trim-omitted',
    'mcp-status-constant-empty',
    'mcp-status-disabled-spelling-flattened',
)
if len(mutations) != EXPECTED_TOTAL:
    raise SystemExit(f'#1538 fixed mutation denominator: expected {EXPECTED_TOTAL}, '
                     f'found {len(mutations)}')
if tuple(row[0] for row in mutations) != EXPECTED_NAMES:
    raise SystemExit('#1538 registered mutation roster must equal the frozen list: '
                     f'expected={list(EXPECTED_NAMES)}, '
                     f'found={[row[0] for row in mutations]}')


def mutate(case, text):
    if text.count(case.old) != 1 or case.old == case.new:
        raise ValueError(f'#1231 stale anchor: {case.mutation}: {text.count(case.old)}')
    text = text.replace(case.old, case.new, 1)
    if case.mutation == 'notification-before-dirty':
        block = '''        broadcast(
            &self.inner,
            &ConfigChangeNotice {
                changed_paths: changed_paths.clone(),
                change_classes: vec!["extension-changed".into()],
            },
        );'''
        if text.count(block) != 2:
            raise ValueError('#1231 stale trailing notification anchor')
        index = text.rfind(block)
        text = text[:index] + text[index + len(block):]
    return text


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#1231 {len(selected_rows)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = sys.argv[2:]
    # A repeated name is a caller-side typo (`a a` for `a b`), not a subset;
    # folding it into a set turns a mistyped selection into a green run over a
    # smaller scope, the same false green as an unknown name (#1734). The two
    # sibling entrants fixed this in r6; this table is edited by the same diff.
    repeated = sorted({name for name in selected if selected.count(name) > 1})
    if repeated:
        refuse('repeated mutation name: ' + ', '.join(repeated))
    if set(selected) - {case.mutation for case in selected_rows}:
        raise SystemExit('unknown mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    if any(case.mutation == 'oracle-corruption' for case in selected_rows) and not env.get('PDRS_BASELINE'):
        raise SystemExit('PDRS_BASELINE required for oracle-corruption')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
    results = []
    skipped = skipped_on_this_platform(selected_rows)
    if skipped:
        # #2036: see LINUX_ONLY. Recorded before the loop so the summary keeps
        # one row per selected control; these rows are neither KILLED nor
        # INVALID, and the exit code below does not fail on them.
        for case in skipped:
            results.append(dict(mutation=case.mutation, test=case.test,
                                assertion=case.assertion, status='SKIPPED',
                                compiled=False,
                                reason='bound test skips itself off Linux '
                                       '(APFS/HFS+ reject non-UTF-8 names), so '
                                       'the mutant is never reached'))
            print(f'{case.mutation}: SKIPPED: Linux-only kill, bound test skips off Linux',
                  flush=True)
        selected_rows = [case for case in selected_rows if case.mutation not in LINUX_ONLY]

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    for case in selected_rows:
        row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                   status='INVALID', reason='', compiled=False)
        args = [case.test, '--', '--exact', '--include-ignored']
        try:
            # Protect the executable fixture and judge, as well as product bytes.
            with mutating_sources(ROOT, [case.path, Path(__file__).resolve(), ORACLE,
                    ORACLE.with_name('generate.mts'), ORACLE.with_name('session.jsonl'),
                    METADATA,
                    ROOT / 'src/pilotdeck-gateway/src/native_process/management_tests.rs',
                    ASSEMBLY_MEMORY,
                    ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs']) as originals:
                original = originals[case.path].decode()
                changed = mutate(case, original)
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('positive control failed')
                (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True), fromfile=str(case.path), tofile=str(case.path))))
                write_source_atomic(ROOT, case.path, changed.encode())
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                # Restore and re-run inside the source guard: the restored control
                # compiles the tree, so it must never race another runner that
                # takes the same .negative-source.lock (#1440).
                # #1908: one atomic rename; a truncating write_bytes here
                # has the same SIGKILL half-write window as the mutation
                # write itself.
                write_source_atomic(ROOT, case.path, originals[case.path])
                if not passed(run(case.mutation + '-restored', args), case.test):
                    raise RuntimeError('restored positive control failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
        print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID', 'SKIPPED')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()),
          flush=True)
    publish_summary(out, results, states=('KILLED', 'SURVIVED', 'INVALID', 'SKIPPED'))
    # SKIPPED (platform-absent, recorded above) is not a failure, but a run
    # that killed nothing proved nothing: with zero KILLED rows the exit stays
    # nonzero so a caller cannot read an all-SKIPPED selection as verified.
    # Same rule as cron-store's LINUX_ONLY accounting and negative_selection's
    # `verdict_exit` (#818 fix-r3 P0); the three review roles below caught the
    # first draft dropping it.
    return int(not results
               or not counts['KILLED']
               or any(row['status'] not in ('KILLED', 'SKIPPED') for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
