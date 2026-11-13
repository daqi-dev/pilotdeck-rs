#!/usr/bin/env python3
"""#1231 guarded native-route mutations; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact test, and pass after restoration.
"""
from pathlib import Path
import difflib
import os
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, write_source_atomic
from negative_selection import claim_dir, missing_target, refuse, refuse_unusable_target, registered
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary
# Discovery anchor: fixtures/rust_binding_views.py (the shared binding-scan
# views this table now reads; without the literal a change to the helper never
# schedules this control).
from rust_binding_views import (carries_test_attribute, scanned_views,
                                 verify_structure_matches_shared_scanner)

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
    # #1352: registered-but-unserved methods (history.rs answers
    # CAPABILITY_UNAVAILABLE) must not be promised by any row.
    ('capability-unserved-history-method', CAPABILITIES,
     'Capability { name: "commands_list", methods: &[], wired: compiled_in },',
     'Capability { name: "commands_list", methods: &["export_session_transcript"], wired: compiled_in },',
     'native_process::capabilities::tests::unserved_history_methods_are_not_promised',
     '#1352 export_session_transcript answers CAPABILITY_UNAVAILABLE'),
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

# #1642: the Requests-bound gates in `capabilities.rs` (`runs`/`memory`/`cron`)
# had assertions but no probes — a rename-style row (`name: "memory_list"` →
# "unwired_...") is a no-op for a family whose gate already filters the row
# out of `advertised()`, so the gate itself must be flipped. The host fixture
# binds neither the memory gateway nor a cron runtime, so the two always-true
# gates publish names the host test's exact set refuses. The other direction
# needs the #1642 cases: `runs` is always `Some` here, so the always-true
# `runs` gate is killed by the `runs: None` case, and the always-false
# `memory` gate hides the pair from the enabled-memory case.
mutations.append(('memory-disabled-advertised', CAPABILITIES,
                  'fn memory(requests: &Requests) -> bool {\n    requests.memory.is_enabled()\n}',
                  'fn memory(_requests: &Requests) -> bool {\n    true\n}', HOST, '#1231 exact wired capability set'))
mutations.append(('cron-unconfigured-advertised', CAPABILITIES,
                  'fn cron(requests: &Requests) -> bool {\n    requests.cron.is_some()\n}',
                  'fn cron(_requests: &Requests) -> bool {\n    true\n}', HOST, '#1231 exact wired capability set'))
mutations.append(('runs-disabled-advertised', CAPABILITIES,
                  'fn runs(requests: &Requests) -> bool {\n    requests.runs.is_some()\n}',
                  'fn runs(_requests: &Requests) -> bool {\n    true\n}',
                  'native_process::management_tests::absent_run_registry_drops_the_run_reads',
                  '#1642 the absent run registry drops the run reads'))
mutations.append(('memory-enabled-omitted', CAPABILITIES,
                  'fn memory(requests: &Requests) -> bool {\n    requests.memory.is_enabled()\n}',
                  'fn memory(_requests: &Requests) -> bool {\n    false\n}',
                  'native_process::management_tests::enabled_memory_gateway_advertises_the_memory_pair',
                  '#1642 the enabled memory gateway advertises the memory pair'))

# #1642 fix-r1（PR #2931 评审）：恒假门的另一侧。上面四行把门翻成恒真，恒假侧
# 此前只有 memory 有启用态见证（memory-enabled-omitted）。cron 启用侧补了精确
# 集用例（configured_cron_runtime_advertises_the_six_names，复用 cron_tests 的
# configured-but-unstarted manager 形态）；always_on / snapshots / runs 的恒假
# 门各绑一个既有启用态见证——staged manager 三名（always_on.rs）、--storage-root
# 启用态精确集（management_tests.rs）、registry switch 的 on 分支（tests.rs）。
# cron 恒假门对两个既有绑定测试先实测存活（HOST 与 requests_registry_switch_
# gates_run_reads 均仍绿，见 fix-r1 RESULT），其余三门的见证在变异下失败于
# 各自专属消息。
mutations.append(('cron-configured-omitted', CAPABILITIES,
                  'fn cron(requests: &Requests) -> bool {\n    requests.cron.is_some()\n}',
                  'fn cron(_requests: &Requests) -> bool {\n    false\n}',
                  'native_process::management_tests::configured_cron_runtime_advertises_the_six_names',
                  '#1642 the configured cron runtime advertises the six names'))
mutations.append(('always-on-staged-omitted', CAPABILITIES,
                  'fn always_on(requests: &Requests) -> bool {\n    requests.always_on.is_some()\n}',
                  'fn always_on(_requests: &Requests) -> bool {\n    false\n}',
                  'native_process::methods::always_on::tests::a_staged_manager_advertises_the_three_names',
                  ['#2294 the configured runtime advertises always_on_apply',
                   '#2294 the configured runtime advertises always_on_abort',
                   '#2294 the configured runtime advertises always_on_rerun_plan']))
mutations.append(('snapshots-enabled-omitted', CAPABILITIES,
                  'fn snapshots(requests: &Requests) -> bool {\n    requests.snapshot_enabled\n}',
                  'fn snapshots(_requests: &Requests) -> bool {\n    false\n}',
                  'native_process::management_tests::storage_root_cli_arg_wires_snapshot_capabilities',
                  '#1323 --storage-root CLI path advertises snapshot-enabled capabilities'))
mutations.append(('runs-bound-omitted', CAPABILITIES,
                  'fn runs(requests: &Requests) -> bool {\n    requests.runs.is_some()\n}',
                  'fn runs(_requests: &Requests) -> bool {\n    false\n}',
                  'native_process::tests::requests_registry_switch_gates_run_reads',
                  '#1369 describe_server gates run capabilities'))

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
# and the run still reported "{n}/{n}" and read green. #1642 fix-r2 (review
# sibling P2#2): the frozen roster is now the shared negative_selection
# registered + refuse (rc 2 = "never started", the exit-code four-way table
# in specs/negative-controls/README.md) instead of a local ordered-tuple
# compare raising SystemExit (rc 1). registered compares the whole ordered
# name list, so the row count is implied and the EXPECTED_TOTAL literal is
# gone; every migrated sibling (utf16-residual-sort, issue-2495 fix-r2, ...)
# uses this form.
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
    'memory-disabled-advertised',
    'cron-unconfigured-advertised',
    'runs-disabled-advertised',
    'memory-enabled-omitted',
    'cron-configured-omitted',
    'always-on-staged-omitted',
    'snapshots-enabled-omitted',
    'runs-bound-omitted',
)
try:
    registered(mutations, EXPECTED_NAMES)
except ValueError as error:
    refuse(str(error))


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


# #1642 fix-r2 (review boundary P2#1): --check validated anchors only, while
# the same-family native-registry-switch runner has also machine-checked the
# binding (its `--check` refuses a renamed bound test) since #1369. A renamed
# or moved bound test here kept printing "105 mutation anchors OK" and only
# surfaced as INVALID after the host's full guarded round. The three rules
# below close that gap; they are fail-closed, so any shape they cannot resolve
# is reported as stale, never skipped.
NATIVE_PROCESS = ROOT / 'src/pilotdeck-gateway/src/native_process'
# `crate::test_baseline::…` is the one module whose file is not under
# `src/pilotdeck-gateway/src/` (lib.rs declares it with
# `#[path = "../tests/common/baseline.rs"]`). The `#[path]` attribute is not
# parsed here: a hand-written Rust lexer over `#[path]`/`mod` is exactly the
# trap #449 kept getting bypassed on, so the exception is an explicit map —
# when it rots, the oracle-corruption assertion stops resolving and --check
# goes red instead of quietly passing.
CRATE_MODULE_FILES = {
    'test_baseline': ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
}

_LITERALS = re.compile(r'"(?:[^"\\]|\\.)*"', re.S)
_FUNCTION = (r'^(?P<indent>[ \t]*)(?:pub(?:\([^)]*\))? )?(?:async )?fn %s'
             r'\([^\n]*\).*?^(?P=indent)}')


# #1642 fix-r4 (review P1-C, MM boundary#1): the scan used to read a single
# comment-stripped text that kept string literals, so an `fn` (and its
# `#[test]`) that only existed inside a raw string was a live candidate and a
# deleted bound test kept passing `--check` on the literal's prose. The two
# concerns are now two texts from the shared `scanned_views`: candidates,
# attributes and call edges come from the literal-blanked structure view,
# while `_anchored` reads the literal-bearing kept view (the assertion itself
# lives in a `panic!` literal payload). Both views share every offset, so a
# span found in one slices the same bytes out of the other.
def _functions_of(text):
    """Every function body in the text, by name (a shared name may repeat).

    The candidate scan runs over the literal-blanked structure view (review
    P1-C), so an `fn` inside a block comment or a string literal is not a
    live function. Each value is ``(structure body, kept body,
    declared-as-test)``: reachability walks the structure bodies (a call
    spelled inside a literal is not an edge) while anchoring reads the kept
    bodies, and the candidate scan in `binding_failures` only accepts test
    functions (review a5).
    """
    kept, structure = scanned_views(text)
    found = {}
    for match in re.finditer(r'^[ \t]*(?:pub(?:\([^)]*\))? )?(?:async )?fn '
                             r'([A-Za-z_][A-Za-z0-9_]*)\([^\n]*\)', structure, re.M):
        name = match.group(1)
        body = re.search(_FUNCTION % re.escape(name), structure[match.start():], re.M | re.S)
        if body:
            start = match.start() + body.start()
            end = match.start() + body.end()
            found.setdefault(name, []).append(
                (structure[start:end], kept[start:end],
                 carries_test_attribute(structure, match.start())))
    return found


def _module_file(module):
    """The file a `crate::<module>::…` call resolves to, or None."""
    candidate = CRATE_MODULE_FILES.get(module)
    if candidate is None:
        candidate = ROOT / ('src/pilotdeck-gateway/src/' + module.replace('::', '/') + '.rs')
    # #1642 fix-r3 (review a4): the mapped branch used to return without an
    # existence check, so a rotted `#[path]` entry surfaced as a FileNotFoundError
    # traceback; returning None routes it through the same "cannot resolve ->
    # stale" fail-closed path as an unparsable module.
    return candidate if candidate.is_file() else None


_QUALIFIED_CALL = re.compile(r'crate::([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)'
                             r'::([A-Za-z_][A-Za-z0-9_]*)\s*\(')
_MODULE_FUNCTIONS = {}


def _reachable_text(entry, functions):
    """The entry's kept body plus every kept body it transitively calls.

    ``entry`` is a ``(structure body, kept body)`` pair: call edges are walked
    on the structure body (a call spelled inside a string literal is not an
    edge, review P1-C) while the anchored text collects the literal-bearing
    kept bodies -- the assertion itself lives in a `panic!` literal payload.

    Bare calls resolve within the same file only, so a sibling test's helper
    cannot satisfy this test's binding; `crate::<module>::<name>(` calls
    resolve through `_module_file` (the shared regeneration guard's panic
    literals live that way). Each callee body is walked with its own file's
    scope, so a qualified helper's own bare calls still resolve.

    #1642 fix-r3 (review a2/a3): the visited key is ``(id(scope), name)``, so
    a same-named helper in another module is walked in its own scope instead
    of being skipped by a cross-file `seen` set (missing reachable text read
    as a false stale), and the `crate::` branch carries the same guard, so
    mutual recursion between qualified helpers cannot loop `--check` forever
    (`id`, because the scope itself is a dict and cannot sit in a set).
    """
    parts = [entry[1]]
    visited = set()
    pending = [(entry[0], functions)]
    while pending:
        body, scope = pending.pop()
        for name in re.findall(r'([A-Za-z_][A-Za-z0-9_]*)\s*\(', body):
            if (id(scope), name) in visited:
                continue
            visited.add((id(scope), name))
            for callee, callee_kept, _is_test in scope.get(name, []):
                parts.append(callee_kept)
                pending.append((callee, scope))
        for module, name in _QUALIFIED_CALL.findall(body):
            source = _module_file(module)
            if source is None:
                continue
            if source not in _MODULE_FUNCTIONS:
                _MODULE_FUNCTIONS[source] = _functions_of(source.read_text())
            callees = _MODULE_FUNCTIONS[source]
            if (id(callees), name) in visited:
                continue
            visited.add((id(callees), name))
            for callee, callee_kept, _is_test in callees.get(name, []):
                parts.append(callee_kept)
                pending.append((callee, callees))
    return '\n'.join(parts)


def _anchored(text, assertion):
    """True when the assertion (or every alternative in a list) is anchored.

    A row's assertion is the text `classify` requires in the bound test's own
    panic payload, so the binding check accepts the three spellings the table
    already uses: the literal itself, a substring of a literal's static text
    (helper messages interpolate around it), and an instantiation of a
    `{placeholder}` template (`#2294 … advertises {method}`, where the table
    records the three concrete names).
    """
    if isinstance(assertion, list):
        return all(_anchored(text, one) for one in assertion)
    for literal in _LITERALS.findall(text):
        content = literal[1:-1]
        if assertion in content:
            return True
        if '{' in content and _template_covers(content, assertion):
            return True
    return False


def _template_covers(template, assertion):
    """True when the assertion is a prefix of some instantiation of template.

    Static chunks must appear in order; a `{placeholder}` absorbs anything
    between them, and the assertion may stop inside a chunk (the table's
    `'#1323 legacy slug of'` is a cut of `"…legacy slug of {path:?}"`).
    #1642 fix-r5 (review boundary P1#1): the cut itself must be non-empty --
    `remainder.endswith(chunk[:length])` used to include `length == 0`, and
    `str.endswith('')` is always True, so the fallback accepted any tail and
    the rule degenerated into "contains the first static chunk".
    """
    position = 0
    for index, chunk in enumerate(re.split(r'\{[^{}]*\}', template)):
        if not chunk:
            continue
        found = assertion.find(chunk, position)
        if found < 0:
            remainder = assertion[position:]
            if index == 0:
                return bool(remainder) and chunk.startswith(remainder)
            return any(remainder.endswith(chunk[:length])
                       for length in range(1, len(chunk) + 1))
        position = found + len(chunk)
    return True


def binding_failures(cases, sources=None):
    """Names of cases whose bound test or assertion binding drifted.

    Each bound test must resolve through its module path to exactly one file
    (`mod tests` inline in that file is the only nesting) and appear there
    exactly once as a `#[test]`/`#[tokio::test]` function (review a5: a
    same-named non-test function cannot stand in for a deleted test), and
    the assertion must be anchored in the text reachable from that test's
    own body. `sources` overrides the file texts, so the binding checker's
    discrimination can be exercised without the tree.
    """
    stale = []
    files = {}
    for case in cases:
        try:
            segments = case.test.split('::')
            if segments[0] != 'native_process' or len(segments) < 2:
                raise ValueError('not a native_process test identity')
            module = segments[1:-1]
            # `…::tests::name` names an inline `mod tests` in the parent module's
            # file; both spellings are tried and the function must appear exactly
            # once across them (`native_process.rs` and `native_process/tests.rs`
            # both exist, so file existence alone cannot disambiguate).
            candidates = [NATIVE_PROCESS.joinpath(*module)]
            if module and module[-1] == 'tests':
                candidates.append(NATIVE_PROCESS.joinpath(*module[:-1]))
            found = []
            for candidate in candidates:
                path = candidate.with_suffix('.rs')
                if sources is None:
                    if not path.is_file():
                        continue
                elif path not in sources:
                    continue
                if path not in files:
                    files[path] = path.read_text() if sources is None else sources[path]
                # #1642 fix-r4 (review P1-C): the candidate pair is
                # (structure body, kept body) -- the test-attribute flag
                # already came from the structure view, so a `#[test] fn`
                # that only a string literal carries never reaches `found`.
                for structure_body, kept_body, is_test in \
                        _functions_of(files[path]).get(segments[-1], []):
                    if is_test:
                        found.append((path, structure_body, kept_body))
            if len(found) != 1:
                raise ValueError('bound test does not resolve to exactly one '
                                 'test function')
            path, structure_body, kept_body = found[0]
            if not _anchored(_reachable_text((structure_body, kept_body),
                                             _functions_of(files[path])),
                             case.assertion):
                raise ValueError('assertion not anchored in the bound test')
        except ValueError:
            # The stripper's fail-closed raises (an unterminated comment or
            # literal) report as stale here too: --check goes red, never green,
            # on a shape the scan cannot resolve.
            stale.append(case.mutation)
    return stale


def check_binding_scope():
    """Self-test the binding checker's discrimination on synthetic sources."""
    management = NATIVE_PROCESS / 'management_tests.rs'
    fixture = {
        management: '#[test]\n'
                    'fn target() {\n    helper();\n    own_template();\n'
                    '    tail_template();\n'
                    '    // panic!("#1642 comment carrier");\n'
                    '}\n'
                    '#[test]\n'
                    'fn sibling_test() {\n    panic!("#1642 marker of another test");\n}\n'
                    'fn helper() {\n    panic!("#1642 marker via helper");\n}\n'
                    'fn unrelated() {\n    panic!("#1642 marker of another test");\n}\n'
                    'fn own_template() {\n    panic!("#1642 covers {method}");\n}\n'
                    'fn tail_template() {\n    panic!("#1642 head {method} tail");\n}\n'
                    'fn plain_carrier() {\n    panic!("#1642 plain non-test carrier");\n}\n'
                    '/*\n'
                    '#[test]\n'
                    'fn ghost() {\n'
                    '    panic!("#1642 block comment carrier");\n'
                    '}\n'
                    '*/\n',
    }

    def rows(test, assertion):
        return [Case('row', management, 'a', 'b', test, assertion)]

    if binding_failures(rows('native_process::management_tests::target',
                             '#1642 marker via helper'), sources=fixture):
        raise RuntimeError('#1642 binding checker lost its own assertion')
    if binding_failures(rows('native_process::management_tests::target',
                             '#1642 covers apply'), sources=fixture):
        raise RuntimeError('#1642 binding checker lost a template assertion')
    # #1642 fix-r5 (review boundary P1#1): a template with static text after
    # the placeholder must not anchor an assertion that diverges inside the
    # placeholder -- `str.endswith('')` used to make the fallback accept any
    # tail, so "#1642 head XXX" read as an instantiation of
    # "#1642 head {method} tail".
    if binding_failures(rows('native_process::management_tests::target',
                             '#1642 head apply tail'), sources=fixture):
        raise RuntimeError('#1642 binding checker lost a trailing-template assertion')
    if not binding_failures(rows('native_process::management_tests::target',
                                 '#1642 head XXX'), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted a placeholder-diverging '
                           'assertion as a template instantiation')
    if not binding_failures(rows('native_process::management_tests::target',
                                 '#1642 marker of another test'), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted a sibling-only assertion')
    if not binding_failures(rows('native_process::management_tests::renamed',
                                 '#1642 marker via helper'), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted an unbound test name')
    if not binding_failures(rows('native_process::management_tests::target',
                                 ['#1642 marker via helper',
                                  '#1642 no such alternative']), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted an unanchored alternative')
    # #1642 fix-r3 (review a1): a marker that only a comment carries anchors
    # nothing; the scan blanks comments before any literal is matched, whether
    # the comment is a dead line inside the live body or a whole block
    # function (which must not count as a live `#[test] fn` either).
    if not binding_failures(rows('native_process::management_tests::target',
                                 '#1642 comment carrier'), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted a comment-only carrier')
    if not binding_failures(rows('native_process::management_tests::ghost',
                                 '#1642 block comment carrier'), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted a block-comment function')
    # #1642 fix-r3 (review a5): a same-named plain function is not a test, so
    # a deleted bound test cannot be stood in for by its assertion text.
    if not binding_failures(rows('native_process::management_tests::plain_carrier',
                                 '#1642 plain non-test carrier'), sources=fixture):
        raise RuntimeError('#1642 binding checker accepted a non-test carrier')
    # #1642 fix-r4 (review P1-C): the real `fn target` is deleted here and a
    # raw string is its only remaining carrier -- the candidate scan reads the
    # literal-blanked structure view, so the phantom never resolves and the
    # binding reports stale instead of anchoring the deleted test's marker on
    # the literal's prose. The same text proves the two views stay equal to
    # the shared single-view scanner after whitespace is collapsed.
    literal_carrier = {
        management: '#[test]\n'
                    'fn sibling_test() {\n    panic!("#1642 marker of another test");\n}\n'
                    'fn documented() {\n'
                    '    let sample = r#"\n'
                    '#[test]\n'
                    'fn target() {\n'
                    '    panic!("#1642 literal carrier");\n'
                    '}\n'
                    '"#;\n'
                    '    let _ = sample;\n'
                    '}\n',
    }
    if not binding_failures(rows('native_process::management_tests::target',
                                 '#1642 literal carrier'),
                           sources=literal_carrier):
        raise RuntimeError('#1642 binding checker accepted a literal-only carrier')
    try:
        verify_structure_matches_shared_scanner(fixture[management])
        verify_structure_matches_shared_scanner(literal_carrier[management])
    except ValueError as error:
        raise RuntimeError(f'#1642 binding views drifted from the shared scanner: {error}')


def main():
    check_binding_scope()
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        stale = binding_failures(selected_rows)
        if stale:
            raise SystemExit('#1231 stale test/assertion bindings: ' + ', '.join(stale))
        print(f'#1231 {len(selected_rows)} mutation anchors and assertion bindings: OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    # #1642 fix-r3 (review b, MM concurrency#1 + sibling#2): the four-way
    # table's "never started" refusals all go through negative_selection now
    # -- the same startup contract the migrated family (utf16-residual-sort,
    # issue-2495) carries. The target rule is the one shared rule (absent /
    # empty / blank / dot), not a presence-only check that let a blank value
    # through to die after the evidence directory existed (#1666 fix-r7).
    if missing_target():
        refuse('#1642 set CARGO_TARGET_DIR to the caller-owned build tree before running')
    refuse_unusable_target()
    selected = sys.argv[2:]
    # A repeated name is a caller-side typo (`a a` for `a b`), not a subset;
    # folding it into a set turns a mistyped selection into a green run over a
    # smaller scope, the same false green as an unknown name (#1734). The two
    # sibling entrants fixed this in r6; this table is edited by the same diff.
    repeated = sorted({name for name in selected if selected.count(name) > 1})
    if repeated:
        refuse('repeated mutation name: ' + ', '.join(repeated))
    unknown = sorted(set(selected) - {case.mutation for case in selected_rows})
    if unknown:
        refuse('unknown mutation name: ' + ', '.join(unknown))
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    if any(case.mutation == 'oracle-corruption' for case in selected_rows) and not env.get('PDRS_BASELINE'):
        refuse('PDRS_BASELINE required for oracle-corruption')
    out = claim_dir(sys.argv[1])
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

    def record(row):
        results.append(row)
        print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)

    def invalid(case, reason):
        return dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                    status='INVALID', reason=reason, compiled=False)

    # One lease covers the complete batch, including every restored positive.
    # No summary is published until the guard has verified restoration on exit.
    # #1665: the previous per-case lease paid the whole guard preflight (flock,
    # workspace-wide git cleanliness, baseline bytes) once per row, and a busy
    # lock could stall each row's 900 s budget in turn — one row-set, one wait.
    phase = 'enter'
    # SKIPPED rows were recorded before the loop, so len(results) is not the
    # loop progress; the counter is.
    progress = 0
    try:
        # Protect the executable fixture and judge, as well as product bytes.
        paths = list(dict.fromkeys(case.path for case in selected_rows))
        with mutating_sources(ROOT, [*paths, Path(__file__).resolve(), ORACLE,
                ORACLE.with_name('generate.mts'), ORACLE.with_name('session.jsonl'),
                METADATA,
                ROOT / 'src/pilotdeck-gateway/src/native_process/management_tests.rs',
                ASSEMBLY_MEMORY,
                ROOT / 'src/pilotdeck-gateway/src/native_process/tests.rs']) as originals:
            phase = 'body'
            for case in selected_rows:
                row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                           status='INVALID', reason='', compiled=False)
                args = [case.test, '--', '--exact', '--include-ignored']
                try:
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
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    phase = 'restore'
                    # Restore and re-run inside the source guard: the restored control
                    # compiles the tree, so it must never race another runner that
                    # takes the same .negative-source.lock (#1440).
                    # #1908: one atomic rename; a truncating write_bytes here
                    # has the same SIGKILL half-write window as the mutation
                    # write itself.
                    write_source_atomic(ROOT, case.path, originals[case.path])
                    if case.path.read_bytes() != originals[case.path]:
                        raise RuntimeError('source restoration failed')
                    phase = 'body'
                try:
                    if not passed(run(case.mutation + '-restored', args), case.test):
                        raise RuntimeError('restored positive control failed')
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                record(row)
                progress += 1
            phase = 'exit'
    except BaseException as error:
        # #1642 fix-r3 (review a9, DS r2c#2): a SIGTERM that landed inside the
        # restore window is re-raised by the guard as SystemExit(128+signum)
        # only after the byte verification, and README's signal contract keeps
        # it off the exit-code table; swallowing it here used to publish an
        # all-INVALID summary over a restored tree and rewrite 143 to 1.
        if isinstance(error, SystemExit):
            raise
        reason = f'source guard: {type(error).__name__}: {error}'
        # #1642 fix-r3 (review b): a guard that never started (dirty tree, busy
        # lock, a KeyboardInterrupt from the startup window) is a startup
        # refusal, not a red run: rc 2, no summary, and the empty claim
        # `claim_dir` made is withdrawn so no evidence directory reads as
        # claimed (utf16-residual-sort's startup_refusal precedent; rmdir only
        # removes the empty case -- a run that already wrote logs keeps them).
        if phase == 'enter':
            try:
                out.rmdir()
            except OSError:
                pass
            refuse(f'guard refused to start: {error!r}')
        # #1642 fix-r2 (review boundary P2#2): the SKIPPED rows were recorded
        # before the guarded window opened — their bound tests skip themselves
        # off Linux, so the guard never ran them and a guard failure says
        # nothing about them. README's four-bucket promise keeps them SKIPPED
        # (neither KILLED nor INVALID); every row the guard did run is still
        # revoked by the shared helper below.
        invalidate_completed([row for row in results if row['status'] != 'SKIPPED'],
                             reason)
        for case in selected_rows[progress:]:
            record(invalid(case, reason))
        publish_summary(out, results, states=('KILLED', 'SURVIVED', 'INVALID', 'SKIPPED'))
        # The guard raised after the body ran, so no recorded row is
        # trustworthy and the tree restoration itself is in doubt: a red run
        # (rc 1, the exit-code four-way table in specs/negative-controls/
        # README.md). The enter half above refused instead; #1642 fix-r2 had
        # these two phases swapped before this round moved the enter half to
        # `refuse` (which also withdraws the empty claim).
        return 1
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
