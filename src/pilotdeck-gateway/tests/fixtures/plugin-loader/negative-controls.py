#!/usr/bin/env python3
"""Temporarily mutate the Rust implementation, require red tests, restore bytes.
Run from any directory: python3 <this-file> <evidence-directory> [mutation-id ...].
No expectation values are generated here; tests replay the real TS oracle.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
OUTPUT = Path(sys.argv[1]).resolve()
OUTPUT.mkdir(parents=True, exist_ok=True)
LOADER = 'src/pilotdeck-gateway/src/runtime/plugins/loader.rs'
CONFIG = 'src/pilotdeck-gateway/src/runtime/plugins/loader/config.rs'
MARKDOWN = 'src/pilotdeck-gateway/src/runtime/plugins/loader/markdown.rs'
# Each replacement is unique and compiles; a compiler error is never a kill.
CASES = [
    ('disk-loading', LOADER, 'plugins.push(Arc::new(plugin));', 'let _ = plugin;', 'plugin_loader_staging_oracle'),
    ('discovery-order', LOADER, 'names.sort();', 'names.sort(); names.reverse();', 'plugin_loader_order'),
    ('follow-symlinks', LOADER, 'fs::metadata(&path)', 'fs::symlink_metadata(&path)', 'plugin_loader_staging_oracle'),
    ('unreadable-directory', LOADER, 'sorted_entries(directory).unwrap_or_default()', 'sorted_entries(directory).expect("negative-control: unreadable directory")', 'plugin_loader_staging_oracle'),
    ('manifest-object-diagnostic', CONFIG, 'Plugin manifest must be an object.', 'negative-control object diagnostic', 'plugin_loader_manifest_oracle'),
    ('manifest-name-diagnostic', CONFIG, 'Plugin manifest must contain a name.', 'negative-control name diagnostic', 'plugin_loader_manifest_oracle'),
    ('manifest-trim-name', CONFIG, '!js_trim(&String::from_utf16_lossy(s.units())).is_empty()', '!String::from_utf16_lossy(s.units()).is_empty()', 'plugin_loader_manifest_oracle'),
    ('manifest-optional-types', CONFIG, 'a.iter().all(|v| v.as_string().is_some())', 'a.len() < usize::MAX', 'plugin_loader_manifest_oracle'),
    ('manifest-marketplace-source', CONFIG, 'key != "source"', 'key == "source"', 'plugin_loader_manifest_oracle'),
    ('configured-markdown', LOADER, 'get("commands"), "commands"', 'get("notCommands"), "commands"', 'plugin_loader_contributions'),
    ('directory-namespace', LOADER, 'let plugin_name = basename(path);', 'let plugin_name = string(get("name")).unwrap();', 'plugin_loader_contributions'),
    ('frontmatter-fence', MARKDOWN, 'raw.starts_with("---\\n")', 'raw.starts_with("---\\r\\n")', 'plugin_loader_contributions'),
    ('frontmatter-scalar', MARKDOWN, '"true" => Value::Bool(true)', '"true" => Value::Bool(false)', 'plugin_loader_contributions'),
    ('skill-filter', LOADER, '.filter(|c| fallback != "skills" || c.is_skill)', '.filter(|c| fallback != "skills" || !c.is_skill)', 'plugin_loader_contributions'),
    ('skill-unicode-case', MARKDOWN, '.replace(\'\\u{212a}\', "k")', '.replace(\'\\u{212a}\', "x")', 'plugin_loader_contributions'),
    ('standalone-root-only', MARKDOWN, '.find(|p| is_skill(p))', '.find(|p| !is_skill(p))', 'plugin_loader_skills'),
    ('inline-hooks', LOADER, 'Some(crate::hooks::config::parse_hooks_config(hooks).settings)', 'Some(Vec::new())', 'plugin_loader_contributions'),
    ('file-hooks', LOADER, 'string(hooks).unwrap_or_else(|| "hooks/hooks.json".into())', 'String::from("hooks/hooks.json")', 'plugin_loader_hooks_paths'),
    ('mcp-servers', LOADER, 'mcp_servers: servers("mcpServers")?', 'mcp_servers: None', 'plugin_loader_contributions'),
    ('lsp-servers', LOADER, 'lsp_servers: servers("lspServers")?', 'lsp_servers: None', 'plugin_loader_contributions'),
    ('builtin-disk-reload', LOADER, 'load_plugin_from_path(&p.path, PluginSource::Builtin)', 'Err::<LoadedPlugin, PluginRuntimeError>(PluginRuntimeError::new("negative-control"))', 'plugin_loader_builtins'),
    ('builtin-fallback', LOADER, '.unwrap_or_else(|_| Arc::clone(p))', '.unwrap_or_else(|_| panic!("negative-control: no builtin fallback"))', 'plugin_loader_staging_oracle'),
    ('builtin-disable', LOADER, '.get(&p.name)', '.get("negative-control-never-disabled")', 'plugin_loader_builtins'),
    ('builtin-skills', LOADER, 'skill_dirs.push((path.clone(), PluginSource::Builtin));', 'let _ = path;', 'plugin_loader_skills'),
    ('duplicates-retained', LOADER, 'plugins.push(Arc::new(plugin));', 'if !plugins.iter().any(|p| p.name == plugin.name && p.source == plugin.source) { plugins.push(Arc::new(plugin)); }', 'plugin_loader_staging_oracle'),
    ('source-identity', LOADER, '(paths.project_plugins_dir, PluginSource::Project)', '(paths.project_plugins_dir, PluginSource::Global)', 'plugin_loader_staging_oracle'),
    ('staged-failures', LOADER, 'staged_failures: failures,', 'staged_failures: Vec::new(),', 'plugin_loader_staging_oracle'),
    ('node-join-absolute', CONFIG, 'PathBuf::from(format!("{}/{}", base.display(), part))', 'base.join(part)', 'plugin_loader_hooks_paths'),
    ('source-path-boundary', CONFIG, '.starts_with(&format!("{}/", root.display()))', '.starts_with(&format!("{}", root.display()))', 'plugin_loader_validation_oracle'),
    ('marketplace-name', CONFIG, '|| OFFICIAL.is_match(name)', '|| (OFFICIAL.is_match(name) && false)', 'plugin_loader_validation_oracle'),
    ('marketplace-deferred', CONFIG, '["git", "zip", "mcpb"].iter().any(|v| s.equals_str(v))', '["unused", "unused", "unused"].iter().any(|v| s.equals_str(v))', 'plugin_loader_validation_oracle'),
]
CASES += [
    ('output-styles', LOADER, 'get("outputStyles"), "output-styles"', 'get("notOutputStyles"), "output-styles"', 'plugin_loader_contributions'),
    ('style-only-config', LOADER, '&configured,\n            "output-styles",', '&JsValue::Undefined,\n            "output-styles",', 'plugin_loader_contributions'),
    ('node-json-diagnostic', LOADER, 'json_syntax::parse_error(&text).unwrap_or(e.message)', 'e.message', 'plugin_loader_invalid'),
    ('configured-trailing-slash', CONFIG, 'out.push("");', 'let _ = path;', 'plugin_loader_contributions'),
    ('server-depth', 'src/pilotdeck-gateway/src/runtime/plugins/loader/server_value.rs', 'Ok(raw.clone())', 'Ok(JsValue::Null)', 'plugin_loader_server_values_oracle'),
    # #564 fix 1: the read moved into read_markdown(...)?; swallow its error here.
    ('configured-directory-read-error', MARKDOWN,
     'let Some(raw) = read_markdown(&root, &canonical, checked_identity)? else {\n                continue;\n            };',
     'let Ok(Some(raw)) = read_markdown(&root, &canonical, checked_identity) else {\n                continue;\n            };',
     'plugin_loader_hooks_paths'),
]
PLUGINS = 'src/pilotdeck-gateway/src/runtime/plugins.rs'
CONTRIBUTIONS = 'src/pilotdeck-gateway/src/runtime/plugins/contributions.rs'
# lib: / regressions: / runtime: select a real test target without weakening
# the existing assertion-failure-only kill criterion.
CASES += [
    ('publication-state', PLUGINS, '        self.assert_active("replace")?;', '        // negative control: permit publication', 'lib:registry_rejects_publication_while_draining'),
    ('shutdown-before-refresh', PLUGINS, '                st.disposal_requested = true;', '                st.disposal_requested = true;\n                st.registry.state = RegistryState::Draining;', 'lib:dispose_waits_for_refresh_publication_then_drains_its_generation'),
    ('retired-cleanup', PLUGINS, '.retain(|p| !Arc::ptr_eq(p, &plugin));', '.retain(|_| true);', 'lib:retired_plugin_disposes_once_across_refresh_and_shutdown'),
    ('shared-disposal', PLUGINS, '            if let Some(pending) = registry\n                .pending_disposals\n                .iter()\n                .find(|pending| Arc::ptr_eq(&pending.plugin, &plugin))\n            {\n                return pending.receiver.clone();\n            }', '            if let Some(pending) = registry\n                .pending_disposals\n                .iter()\n                .find(|pending| Arc::ptr_eq(&pending.plugin, &plugin))\n            {\n                if let Some(dispose) = &plugin.dispose { drop(dispose()); }\n                return pending.receiver.clone();\n            }', 'lib:retired_disposal_is_shared_with_shutdown_and_all_lease_flavors'),
    ('release-error', PLUGINS, '    PluginRuntimeError::aggregate("Failed to dispose released plugins.", errors)', '    let _ = errors; Ok(())', 'lib:retired_disposal_is_shared_with_shutdown_and_all_lease_flavors'),
    ('lease-release-error', CONTRIBUTIONS, '        let result = release_lease_plugins(&plugins, &state, &waiters).await;', '        let _ = release_lease_plugins(&plugins, &state, &waiters).await; let result = Ok(());', 'lib:retired_disposal_is_shared_with_shutdown_and_all_lease_flavors'),
    ('skill-insertion-order', CONTRIBUTIONS, 'selected.into_iter().map(|(c, _)| c).collect()', 'selected.into_iter().rev().map(|(c, _)| c).collect()', 'lib:skill_contributions_keep_ts_map_order'),
    ('skill-equal-priority', CONTRIBUTIONS, 'priority >= selected[index].1', 'priority > selected[index].1', 'lib:skill_contributions_keep_ts_map_order'),
    ('utf16-byte-slice', CONTRIBUTIONS,
     '    let mut units = trimmed[..trimmed.len().min(MAX_LEN)].to_vec();',
     '    let decoded = String::from_utf16_lossy(trimmed); let cut = if decoded.len() > MAX_LEN { &decoded[..MAX_LEN] } else { &decoded }; let mut units: Vec<u16> = cut.encode_utf16().collect();',
     'regressions:plugin_mcp_utf16_oracle'),
    ('utf16-lossy-surrogate', CONTRIBUTIONS, 'let mut trimmed = value.units();', 'let decoded = JsString::from(String::from_utf16_lossy(value.units())); let mut trimmed = decoded.units();', 'regressions:plugin_mcp_utf16_oracle'),
    ('utf16-trim', CONTRIBUTIONS, 'char::from_u32(u32::from(unit)).is_some_and(crate::metadata::js_space)', 'char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)', 'regressions:plugin_mcp_utf16_oracle'),
    ('runtime-replay-generation', PLUGINS, 'self.generation += 1;', 'self.generation += 2;', 'runtime:runtime_empty_directories'),
]
# #564 ROOT-001 checked read. Each race test fires on sub/b.md itself and
# compares with the real TS control; each layer has a case only it rejects.
PROJECT_FILES = 'src/pilotdeck-gateway/src/project_files.rs'
TOCTOU = 'lib:runtime::plugins::loader::markdown::tests::'
CASES += [
    # Pre-open regular-file filter: a socket .md would fail the open and the load.
    ('markdown-file-type-skip', MARKDOWN, '        Ok(m) if m.is_file() => m,', '        Ok(m) if true => m,', TOCTOU + 'socket_markdown_is_skipped'),
    # A: the pre-#564 path read (follows any raced-in link).
    ('markdown-path-read', MARKDOWN, '    let mut file = match crate::project_files::open_beneath(root, canonical) {', '    if true {\n        return read_text(canonical).map(Some);\n    }\n    let mut file = match crate::project_files::open_beneath(root, canonical) {', TOCTOU + 'leaf_symlink_raced_after_check_is_skipped'),
    ('markdown-path-read-ancestor', MARKDOWN, '    let mut file = match crate::project_files::open_beneath(root, canonical) {', '    if true {\n        return read_text(canonical).map(Some);\n    }\n    let mut file = match crate::project_files::open_beneath(root, canonical) {', TOCTOU + 'ancestor_symlink_raced_after_check_is_skipped'),
    # B: NOFOLLOW removed from the leaf / the ancestors (shared #508 helper).
    ('markdown-nofollow-leaf', PROJECT_FILES, 'OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK', 'OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK', TOCTOU + 'leaf_moved_outside_behind_symlink_is_skipped'),
    ('markdown-nofollow-ancestor', PROJECT_FILES, 'access | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW', 'access | OFlags::DIRECTORY | OFlags::CLOEXEC', TOCTOU + 'ancestor_moved_outside_behind_symlink_is_skipped'),
    # C: post-open (dev, ino) recheck removed.
    ('markdown-identity-recheck', MARKDOWN, 'if !after.is_file() || (after.dev(), after.ino()) != checked_identity {', 'if !after.is_file() {', TOCTOU + 'leaf_replaced_by_other_inode_is_skipped'),
    # Fix round 2: identity anchor moved to check point; revert to lstat identity.
    ('markdown-identity-anchor', MARKDOWN, 'if !after.is_file() || (after.dev(), after.ino()) != checked_identity {', 'let lstat_identity = (_before.dev(), _before.ino());\n    if !after.is_file() || (after.dev(), after.ino()) != lstat_identity {', TOCTOU + 'hardlink_with_different_checked_identity_is_skipped'),
    # Fix round 2: deletion after check now propagates; revert to skip.
    ('markdown-deletion-skip', MARKDOWN, "        Err(e) => return Err(error(e)), // doesn't exist or other error: propagate", '        Err(_) => return Ok(None), // negative control: deletion silently skipped', TOCTOU + 'deleted_after_check_propagates_error'),
    # Fix round 4: ENOTDIR alone cannot prove a link replaced an ancestor.
    ('markdown-ancestor-file-open-skip', MARKDOWN, '&& has_symlink_ancestor(canonical)', '&& true', TOCTOU + 'ancestor_file_after_lstat_propagates_error'),
    ('markdown-ancestor-file-lstat-skip', MARKDOWN, "        Err(e) => return Err(error(e)), // doesn't exist or other error: propagate", '        Err(_) => return Ok(None), // negative control: ENOTDIR silently skipped', TOCTOU + 'ancestor_file_before_lstat_propagates_error'),
    ('markdown-ancestor-file-directory-skip', MARKDOWN,
     'let Some(raw) = read_markdown(&root, &canonical, checked_identity)? else {\n                continue;\n            };',
     'let Ok(Some(raw)) = read_markdown(&root, &canonical, checked_identity) else {\n                continue;\n            };',
     TOCTOU + 'ancestor_file_rejects_only_its_configured_directory'),
    # Race errors are skipped, not turned into a failed load.
    ('markdown-race-error', MARKDOWN, '        {\n            return Ok(None);\n        }\n        Err(e) => return Err(error(e)),', '        {\n            return Err(error(e));\n        }\n        Err(e) => return Err(error(e)),', TOCTOU + 'ancestor_symlink_raced_after_check_is_skipped'),
    # Ordinary open errors on the unchanged file still propagate like TS readFile.
    ('markdown-open-error-skip', MARKDOWN, '        Err(e) => return Err(error(e)),\n    };', '        Err(_) => return Ok(None),\n    };', 'plugin_loader_hooks_paths'),
]
# Each mutation restores one removed limit, or removes global directory de-dup.
markdown_source = (ROOT/MARKDOWN).read_text()
ENTRY_GUARD = '        if !canonical.starts_with(&root) {\n            continue;\n        }'
DEDUP = 'if !visited.insert(directory_identity(&metadata, &canonical)) {'
def replace_once(text, old, new):
    assert text.count(old) == 1, f'unique repair-3 mutation anchor required: {old!r}'
    return text.replace(old, new)

ENTRY_LIMIT = replace_once(markdown_source, '    let mut output = Vec::new();',
    '    let mut output = Vec::new();\n    let mut remaining = 4096usize;')
ENTRY_LIMIT = replace_once(ENTRY_LIMIT, '            output.push(load_markdown_source(',
    '            if remaining == 0 { return Err(PluginRuntimeError::new("negative-control candidate budget")); }\n            remaining -= 1;\n            output.push(load_markdown_source(')
ALL_ENTRY_LIMIT = replace_once(markdown_source, '    let mut output = Vec::new();',
    '    let mut output = Vec::new();\n    let mut remaining = 4096usize;')
ALL_ENTRY_LIMIT = replace_once(ALL_ENTRY_LIMIT, '    while let Some(path) = pending.pop() {',
    '    while let Some(path) = pending.pop() {\n        if remaining == 0 { return Err(PluginRuntimeError::new("negative-control enumeration budget")); }\n        remaining -= 1;')
DEPTH_LIMIT = replace_once(markdown_source,
    'let mut pending: Vec<_> = markdown_entries(base).into_iter().rev().collect();',
    'let mut pending: Vec<_> = markdown_entries(base).into_iter().rev().map(|p| (p, 1usize)).collect();')
DEPTH_LIMIT = replace_once(DEPTH_LIMIT, 'while let Some(path) = pending.pop() {',
    'while let Some((path, depth)) = pending.pop() {')
DEPTH_LIMIT = replace_once(DEPTH_LIMIT, '            pending.extend(markdown_entries(&path).into_iter().rev());',
    '            if depth > 64 { return Err(PluginRuntimeError::new("negative-control depth budget")); }\n            pending.extend(markdown_entries(&path).into_iter().rev().map(|p| (p, depth + 1)));')
CASES += [
    ('markdown-cycle', MARKDOWN, DEDUP, 'if false && !visited.insert(directory_identity(&metadata, &canonical)) {', 'traversal:plugin_markdown_clique_10'),
    ('markdown-expansion-identity', MARKDOWN, '    Ok(output)\n}', '    if let Some(first) = output.first().map(|c| c.path.clone()) {\n        for command in &mut output { command.path = first.clone(); }\n    }\n    Ok(output)\n}', 'traversal:plugin_markdown_clique_10'),
    ('markdown-base-symlink', MARKDOWN, '    if !canonical.starts_with(&root) {\n        return Ok(Vec::new());\n    }', '    let root = canonical.clone();', 'traversal:plugin_markdown_configured_base_symlink'),
    ('markdown-root-symlink', MARKDOWN, 'plugin_root.canonicalize()', 'Ok::<_, std::io::Error>(plugin_root.to_path_buf())', 'traversal:plugin_markdown_plugin_root_symlink'),
    ('markdown-absolute-configured', CONFIG, 'PathBuf::from(format!("{}/{}", base.display(), part))', 'base.join(part)', 'traversal:plugin_markdown_absolute_configured_path'),
    ('markdown-outside-unreadable', MARKDOWN, ENTRY_GUARD, '', 'traversal:plugin_markdown_outside_unreadable_file'),
    # #564 fix 1: the canonical source is no longer a parameter; pass it as the contribution path.
    ('markdown-alias', MARKDOWN, '                &raw,\n                &path,\n', '                &raw,\n                &canonical,\n', 'traversal:plugin_markdown_alias_first_path'),
    ('markdown-entry-budget', MARKDOWN, markdown_source, ENTRY_LIMIT, 'traversal:plugin_markdown_entries_match_ts'),
    ('markdown-depth-budget', MARKDOWN, markdown_source, DEPTH_LIMIT, 'traversal:plugin_markdown_deep_tree_matches_ts'),
    ('markdown-directory-boundary', MARKDOWN, ENTRY_GUARD, '        if !canonical.starts_with(&root) && !path.components().any(|c| c.as_os_str() == "directory") { continue; }', 'traversal:plugin_markdown_root_boundary'),
    ('markdown-file-boundary', MARKDOWN, ENTRY_GUARD, '        if !canonical.starts_with(&root) && path.file_name().is_none_or(|n| n != "file.md") { continue; }', 'traversal:plugin_markdown_root_boundary'),
    ('markdown-chain-boundary', MARKDOWN, ENTRY_GUARD, '        if !canonical.starts_with(&root) && !path.components().any(|c| c.as_os_str() == "chain") { continue; }', 'traversal:plugin_markdown_root_boundary'),
    ('markdown-configured-root', MARKDOWN, '    if !canonical.starts_with(&root) {\n        return Ok(Vec::new());\n    }', '    let root = canonical.clone();', 'traversal:plugin_markdown_root_boundary'),
    ('mcp-server-name-lossy', CONTRIBUTIONS, '        for (server_name, raw) in obj.own_entries() {', '        for (server_name, raw) in obj.own_entries() {\n            let server_name = &JsString::from(String::from_utf16_lossy(server_name.units()));', 'traversal:plugin_mcp_server_names_utf16'),
    ('markdown-all-entry-budget', MARKDOWN, markdown_source, ALL_ENTRY_LIMIT, 'traversal:plugin_markdown_non_markdown_assets'),
    ('markdown-dag-no-dedup', MARKDOWN, 'if !visited.insert(directory_identity(&metadata, &canonical)) {', 'if false && !visited.insert(directory_identity(&metadata, &canonical)) {', 'traversal:plugin_markdown_alias_dag'),
    ('markdown-simple-cycle-no-dedup', MARKDOWN, 'if !visited.insert(directory_identity(&metadata, &canonical)) {', 'if false && !visited.insert(directory_identity(&metadata, &canonical)) {', 'traversal:plugin_markdown_simple_cycle'),
    ('markdown-real-candidate-budget', MARKDOWN, markdown_source, ENTRY_LIMIT, 'traversal:plugin_markdown_real_node_modules'),
    ('markdown-readme-candidate-budget', MARKDOWN, markdown_source, ENTRY_LIMIT, 'traversal:plugin_markdown_readme_and_js'),
    ('markdown-node-modules-depth-budget', MARKDOWN, markdown_source, DEPTH_LIMIT, 'traversal:plugin_markdown_configured_deep_node_modules'),
]
# Every kill needs its own expected panic text AND the selected test's failed
# outcome. Generic failures, compiler errors, and external timeouts do not count.
EXPECTED_FAILURES = {'builtin-disable': ['assertion `left == right` failed: builtin-loading-and-disable: refresh'],
 'builtin-disk-reload': ['assertion `left == right` failed: builtin-loading-and-disable: refresh'],
 'builtin-fallback': ['negative-control: no builtin fallback'],
 'builtin-skills': ['assertion `left == right` failed: skills-root-only: refresh'],
 'configured-directory-read-error': ['assertion `left == right` failed: '
                                     'hook-files-and-configured-paths: load attempt 2'],
 'configured-markdown': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'configured-trailing-slash': ['assertion `left == right` failed: valid-contributions: load '
                               'attempt 0'],
 'directory-namespace': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'discovery-order': ['assertion `left == right` failed: discovery-unicode-order: discovered '
                     'plugins'],
 'disk-loading': ['assertion `left == right` failed: "valid-contributions": staged plugins'],
 'duplicates-retained': ['assertion `left == right` failed: "duplicates-and-priority": staged '
                         'plugins'],
 'file-hooks': ['assertion `left == right` failed: hook-files-and-configured-paths: load attempt '
                '1'],
 'follow-symlinks': ['assertion `left == right` failed: "symlinks-and-unreadable": staged plugins'],
 'frontmatter-fence': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'frontmatter-scalar': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'inline-hooks': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'lease-release-error': ['called `Result::unwrap_err()` on an `Ok` value: ()'],
 'lsp-servers': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'manifest-marketplace-source': ['assertion `left == right` failed: manifest Object {"agents": '
                                 'String("agent"), "commands": Array [String("a"), String("b")], '
                                 '"description": String("d"), "extra": Number(9), "hooks": Object '
                                 '{}, "lspServers": Object {}, "marketplace": Object {"extra": '
                                 'Number(1), "name": String("m"), "plugin": String("p"), "source": '
                                 'String("git"), "url": String("u"), "version": String("1")}, '
                                 '"mcpServers": Object {}, "mcpb": String("x.mcpb"), "name": '
                                 'String("all"), "outputStyles": String("out"), "settings": Object '
                                 '{"enabled": Bool(false)}, "skills": Array [], "version": '
                                 'String("")}'],
 'manifest-name-diagnostic': ['assertion `left == right` failed: manifest Object {}'],
 'manifest-object-diagnostic': ['assertion `left == right` failed: manifest Null'],
 'manifest-optional-types': ['assertion `left == right` failed: manifest Object {"agents": Array '
                             '[Number(1)], "commands": Number(5), "description": Bool(false), '
                             '"hooks": Array [], "lspServers": Number(3), "marketplace": Object '
                             '{"name": String("m")}, "mcpServers": Array [], "mcpb": '
                             'String("x.MCPB"), "name": String(" kept "), "outputStyles": Array '
                             '[Number(1)], "settings": Array [], "skills": Null, "version": '
                             'Number(3)}'],
 'manifest-trim-name': ['assertion `left == right` failed: manifest Object {"name": String(" '
                        '\\t\\u{feff}")}'],
 'markdown-alias': ['first alias retains TS name and path'],
 'markdown-cycle': ['markdown clique hang guard exceeded 20s; child killed and reaped'],
 'markdown-expansion-identity': ['each physical directory expanded once', '  left: 1', ' right: 10'],
 'markdown-base-symlink': ['configured base symlink stays inside plugin root across four loaders'],
 'markdown-root-symlink': ['plugin root symlink retains TS lexical contributions across four loaders'],
 'markdown-absolute-configured': ['absolute configuration follows TS join across four loaders'],
 'markdown-outside-unreadable': ['ROOT-001 retains safe contributions when outside Markdown is unreadable'],
 'marketplace-deferred': ['assertion `left == right` failed: marketplace reference'],
 'marketplace-name': ['assertion `left == right` failed: {"error":"Marketplace name impersonates '
                      'an official PilotDeck marketplace.","name":"official-pilot"}'],
 'mcp-servers': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'node-join-absolute': ['assertion `left == right` failed: hook-files-and-configured-paths: load '
                        'attempt 1'],
 'node-json-diagnostic': ['assertion `left == right` failed: invalid-and-loose-option-types: '
                          'style-only loader'],
 'output-styles': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'publication-state': ['called `Option::unwrap()` on a `None` value'],
 'release-error': ['called `Result::unwrap_err()` on an `Ok` value: ()'],
 'retired-cleanup': ['assertion failed: '
                     'runtime.state.lock().await.registry.retired_plugins.is_empty()'],
 'runtime-replay-generation': ['assertion `left == right` failed: "empty-directories" step 1: '
                               '{"op":"refresh"}'],
 'server-depth': ['assertion `left == right` failed',
                  '  left: "null"',
                  ' right: "{\\"server\\":{\\"leaf\\":true}}"'],
 'shared-disposal': ['assertion `left == right` failed', '  left: 2', ' right: 1'],
 'shutdown-before-refresh': ['assertion `left == right` failed',
                             '  left: "draining"',
                             ' right: "active"'],
 'skill-equal-priority': ['assertion `left == right` failed',
                          '  left: Array [Object {"content": String("higher"), "description": '
                          'String("higher"), "name": String("z"), "namespace": String("same")}, '
                          'Object {"content": String("a"), "description": String("a"), "name": '
                          'String("a"), "namespace": String("same")}, Object {"content": '
                          'String("m"), "description": String("m"), "name": String("m"), '
                          '"namespace": String("same")}, Object {"content": String("global-q"), '
                          '"description": String("global-q"), "name": String("q"), "namespace": '
                          'String("same")}, Object {"content": String("b"), "description": '
                          'String("b"), "name": String("b"), "namespace": String("same")}, Object '
                          '{"content": String("y"), "description": String("y"), "name": '
                          'String("y"), "namespace": String("same")}]',
                          ' right: Array [Object {"content": String("higher"), "description": '
                          'String("higher"), "name": String("z"), "namespace": String("same")}, '
                          'Object {"content": String("equal-a"), "description": String("equal-a"), '
                          '"name": String("a"), "namespace": String("same")}, Object {"content": '
                          'String("m"), "description": String("m"), "name": String("m"), '
                          '"namespace": String("same")}, Object {"content": String("global-q"), '
                          '"description": String("global-q"), "name": String("q"), "namespace": '
                          'String("same")}, Object {"content": String("b"), "description": '
                          'String("b"), "name": String("b"), "namespace": String("same")}, Object '
                          '{"content": String("y"), "description": String("y"), "name": '
                          'String("y"), "namespace": String("same")}]'],
 'skill-filter': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'skill-insertion-order': ['assertion `left == right` failed',
                           '  left: Array [Object {"content": String("y"), "description": '
                           'String("y"), "name": String("y"), "namespace": String("same")}, Object '
                           '{"content": String("b"), "description": String("b"), "name": '
                           'String("b"), "namespace": String("same")}, Object {"content": '
                           'String("global-q"), "description": String("global-q"), "name": '
                           'String("q"), "namespace": String("same")}, Object {"content": '
                           'String("m"), "description": String("m"), "name": String("m"), '
                           '"namespace": String("same")}, Object {"content": String("equal-a"), '
                           '"description": String("equal-a"), "name": String("a"), "namespace": '
                           'String("same")}, Object {"content": String("higher"), "description": '
                           'String("higher"), "name": String("z"), "namespace": String("same")}]',
                           ' right: Array [Object {"content": String("higher"), "description": '
                           'String("higher"), "name": String("z"), "namespace": String("same")}, '
                           'Object {"content": String("equal-a"), "description": '
                           'String("equal-a"), "name": String("a"), "namespace": String("same")}, '
                           'Object {"content": String("m"), "description": String("m"), "name": '
                           'String("m"), "namespace": String("same")}, Object {"content": '
                           'String("global-q"), "description": String("global-q"), "name": '
                           'String("q"), "namespace": String("same")}, Object {"content": '
                           'String("b"), "description": String("b"), "name": String("b"), '
                           '"namespace": String("same")}, Object {"content": String("y"), '
                           '"description": String("y"), "name": String("y"), "namespace": '
                           'String("same")}]'],
 'skill-unicode-case': ['assertion `left == right` failed: valid-contributions: load attempt 0'],
 'source-identity': ['assertion `left == right` failed: "duplicates-and-priority": staged plugins'],
 'source-path-boundary': ['assertion `left == right` failed: '
                          '{"allowedRoot":"<root>/safe","path":"<root>/safe-other","value":false}'],
 'staged-failures': ['assertion `left == right` failed: "invalid-and-loose-option-types": staged '
                     'failures'],
 'standalone-root-only': ['assertion `left == right` failed: skills-root-only: load attempt 0'],
 'style-only-config': ['assertion `left == right` failed: valid-contributions: style-only loader'],
 'unreadable-directory': ['negative-control: unreadable directory'],
 'utf16-byte-slice': ["end byte index 2048 is not a char boundary; it is inside '中' (bytes "
                      '2046..2049 of string)'],
 'utf16-lossy-surrogate': ['assertion `left == right` failed: Some("lone-high")'],
 'utf16-trim': ['assertion `left == right` failed: Some("trim-ecmascript")'],
 'markdown-entry-budget': ['wide tree succeeds like TS'],
 'markdown-depth-budget': ['deep tree succeeds like TS'],
 # #564: an entry let past the traversal guard now hits the checked open's own
 # root containment (EACCES), which fails the configured directory instead of
 # leaking; the safe sibling is lost, so the boundary replay still turns red.
 'markdown-directory-boundary': ['root boundary skips directory/file/chain leaks: commands',
                                 '  left: Array []'],
 'markdown-file-boundary': ['root boundary skips directory/file/chain leaks: commands',
                            '  left: Array []'],
 'markdown-chain-boundary': ['root boundary skips directory/file/chain leaks: commands',
                             '  left: Array []'],
 'markdown-configured-root': ['configured base cannot escape root'],
 'mcp-server-name-lossy': ['server names retain UTF-16 identity and sort order'],
 'markdown-all-entry-budget': ['non-Markdown assets match TS'],
 'markdown-dag-no-dedup': ['alias DAG retains each physical contribution'],
 'markdown-simple-cycle-no-dedup': ['simple cycle removes TS ELOOP-truncated duplicates'],
 'markdown-real-candidate-budget': ['real node_modules skills match TS'],
 'markdown-readme-candidate-budget': ['5000 README and 54000 JS preserve TS skills'],
 'markdown-node-modules-depth-budget': ['deep node_modules matches TS across configured loaders'],
 'markdown-file-type-skip': ['socket must be skipped, not fail'],
 'markdown-path-read': ['leaf-symlink: raced load keeps only the TS control', 'String("SECRET")'],
 'markdown-path-read-ancestor': ['ancestor-symlink: raced load keeps only the TS control', 'String("SECRET")'],
 'markdown-nofollow-leaf': ['leaf-moved-symlink: raced load keeps only the TS control', 'String("inside")'],
 'markdown-nofollow-ancestor': ['ancestor-moved-symlink: raced load keeps only the TS control', 'String("inside")'],
 'markdown-identity-recheck': ['leaf-hardlink: raced load keeps only the TS control', 'String("SECRET")'],
 'markdown-race-error': ['ancestor-symlink: raced entry must be skipped, not fail'],
 'markdown-open-error-skip': ['assertion `left == right` failed: hook-files-and-configured-paths: load attempt 2'],
 'markdown-identity-anchor': ['leaf-hardlink: raced load keeps only the TS control', 'String("SECRET")'],
 'markdown-deletion-skip': ['deletion after check must propagate error'],
 'markdown-ancestor-file-open-skip': ['ordinary-file ancestor must propagate ENOTDIR'],
 'markdown-ancestor-file-lstat-skip': ['ordinary-file ancestor must propagate ENOTDIR'],
 'markdown-ancestor-file-directory-skip': ['ordinary-file ancestor rejects exactly its configured directory like TS', 'String("safe")']}


assert len(CASES) == len({c[0] for c in CASES}), "mutation ids must be unique"
assert set(EXPECTED_FAILURES) == {c[0] for c in CASES}, "every case needs failure markers"
selected = set(sys.argv[2:])
if selected:
    assert selected <= {c[0] for c in CASES}, selected
    CASES = [c for c in CASES if c[0] in selected]
originals = {p:(ROOT/p).read_bytes() for p in {c[1] for c in CASES}}
assert all(originals[p].decode().count(old) == 1 and old != new for _,p,old,new,_ in CASES), 'mutation anchor must be unique'
env = dict(os.environ)
# The generator's guard was already run twice; replay metadata validation stays
# enabled, but avoid 650 baseline git subprocesses after every recompilation.
env.pop('PDRS_BASELINE', None)
summary = []
try:
    for name, relative, old, new, test in CASES:
        path = ROOT/relative
        assert path.read_bytes() == originals[relative], f'concurrent modification: {relative}'
        gate = wait_for_slot(ROOT/'scripts/cargo-slots.sh', interval=5)
        (OUTPUT/(name+'.slots.log')).write_text(gate.stdout + gate.stderr)
        path.write_text(originals[relative].decode().replace(old,new))
        try:
            if ':' in test:
                target, test_name = test.split(':', 1)
                if target == 'lib':
                    target_args = ['--lib']
                    # Unit tests already have a full module path if `::` appears.
                    if '::' not in test_name:
                        test_name = 'runtime::plugins::tests::' + test_name
                else:
                    target_args = ['--test', {'regressions':'plugin_regressions','runtime':'plugin_runtime','traversal':'plugin_traversal'}[target]]
            else:
                target_args, test_name = ['--test','plugin_loader'], test
            result = run_owned(['cargo','test','--manifest-path',str(ROOT/'src/Cargo.toml'),'-p','pilotdeck-gateway',*target_args,test_name,'--','--exact','--test-threads=4'],cwd=ROOT,env=env,timeout=600 if test_name == 'plugin_markdown_real_node_modules' else 180)
            (OUTPUT/(name+'.log')).write_text(result.stdout + result.stderr)
            output = (OUTPUT/(name+'.log')).read_text()
            markers = EXPECTED_FAILURES[name]
            matched_markers = all(marker in output for marker in markers)
            target_failed = f'test {test_name} ... FAILED' in output
            killed = result.returncode != 0 and 'test result: FAILED.' in output and 'error[E' not in output and matched_markers and target_failed
            row = {'mutation':name,'test':test,'exit':result.returncode,'killed':killed,'expectedMarkers':markers,'matchedMarkers':matched_markers,'targetFailed':target_failed,'failure':next((line for line in output.splitlines() if 'failed:' in line or "panicked at" in line),''),'summary':next((line for line in output.splitlines() if line.startswith('test result:')), '')}
            summary.append(row)
            print(json.dumps(row), flush=True)
            # Finish the suite even if a case is not killed; report all failures
            # together below, without counting infrastructure failures as kills.
        finally:
            path.write_bytes(originals[relative])
finally:
    assert all((ROOT/p).read_bytes() == b for p,b in originals.items()), 'implementation bytes not restored'
    counts = {'configured':len(CASES), 'attempted':len(summary), 'killed':sum(row['killed'] for row in summary)}
    print(json.dumps({'counts':counts}), flush=True)
    (OUTPUT/'summary.json').write_text(json.dumps({'counts':counts,'results':summary,'restoredSha256':{p:hashlib.sha256(b).hexdigest() for p,b in originals.items()}},indent=2)+'\n')

assert len(summary) == len(CASES) and all(row['killed'] for row in summary), 'negative controls not all killed; inspect summary.json and individual logs'
