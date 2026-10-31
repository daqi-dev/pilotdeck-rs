#!/usr/bin/env python3
"""#1136 HEAD-only controls: --check | [--compile-only] NEW_OUTPUT_DIR [name ...].
Every mutant compiles, runs its nonce-bound test, restores and reruns positive.
Caller must provide CARGO_TARGET_DIR; dirty/new inputs are never substituted.
"""
import difflib
import json
import os
from pathlib import Path
import subprocess
import sys
sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[5]
shared = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path.insert(0, str(shared))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, judge, passed
run_tests = root / 'src/pilotdeck-cli/tests/sdk_run_exit.rs'
settings = root / 'src/pilotdeck-cli/tests/sdk_settings_exit.rs'
sessions_list = root / 'src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs'
sessions_messages = root / 'src/pilotdeck-cli/tests/sdk_sessions_messages_exit.rs'
turn = root / 'src/pilotdeck-cli/src/sdk/turn.rs'
transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
resume_tests = root / 'src/pilotdeck-cli/tests/sdk_resume_exit.rs'
format_tests = root / 'src/pilotdeck-cli/tests/sdk_format_exit.rs'
oracle = root / 'src/pilotdeck-cli/tests/fixtures/sdk-run-stdin/oracle.json'
mutations = [
    ('run-stdin-synchronous', run_tests, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_run_exit'),
    ('run-stdin-cancel-registry', run_tests, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_run_exit'),
    ('run-stdin-cancel-join', run_tests, '|| writer.finish()', '|| drop(writer)', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_run_exit'),
    ('settings-stdin-synchronous', settings, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_settings_exit'),
    ('settings-stdin-cancel-registry', settings, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_settings_exit'),
    ('settings-stdin-cancel-join', settings, '|| writer.finish()', '|| drop(writer)', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_settings_exit'),
    ('sessions-list-stdin-synchronous', sessions_list, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_sessions_list_exit'),
    ('sessions-list-stdin-cancel-registry', sessions_list, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_sessions_list_exit'),
    ('sessions-list-stdin-cancel-join', sessions_list, '|| writer.finish()', '|| drop(writer)', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_sessions_list_exit'),
    ('sessions-messages-stdin-synchronous', sessions_messages, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_sessions_messages_exit'),
    ('sessions-messages-stdin-cancel-registry', sessions_messages, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_sessions_messages_exit'),
    ('sessions-messages-stdin-cancel-join', sessions_messages, '|| writer.finish()', '|| drop(writer)', 'fixture_stdin_cancel_reaps_writer', '#1136 cancellation joins stdin writer', 'sdk_sessions_messages_exit'),
    ('run-wire-end-connect', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'run_no_late_connection', '#1058 no third connection', 'sdk_run_exit'),
    ('settings-wire-end-connect', transport, '    transport.close().await;\n    result', '    transport.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'settings_hello_matches_ts_bytes', '#1029 no reconnect settings-default--argv', 'sdk_settings_exit'),
    ('sessions-list-wire-end-connect', transport, '    transport.close().await;\n    result', '    transport.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'sessions_list_matches_ts_matrix', '#983 no reconnect sessions-default--argv', 'sdk_sessions_list_exit'),
    ('sessions-messages-wire-end-connect', transport, '    transport.close().await;\n    result', '    transport.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'sessions_messages_matches_ts_matrix', '#1028 no reconnect sessions-messages--argv', 'sdk_sessions_messages_exit'),
    ('large-stdin-empty-cases', oracle, '  "cases": [', '  "cases": [],\n  "discardedCases": [', 'run_large_stdin_matches_ts', '#1136 both large stdin timings', 'sdk_run_exit'),
    ('large-stdin-small-input', oracle, '"delayed": false,\n      "repeat": 131072,', '"delayed": false,\n      "repeat": 1,', 'run_large_stdin_matches_ts', '#1136 stdin exceeds pipe capacity', 'sdk_run_exit'),
    ('large-stdin-input-drift', run_tests, 'let stdin = format!("  {}中文\\n", "x".repeat(repeat));', 'let stdin = format!("  {}中文\\n", "y".repeat(repeat));', 'run_large_stdin_matches_ts', '#1136 input matches TS large stdin', 'sdk_run_exit'),
    ('large-stdin-delayed-truncated', run_tests, '.push(StdinWriter::start(stdin, bytes));', '.push(StdinWriter::start(stdin, bytes[..1024].to_vec()));', 'run_large_stdin_matches_ts', '#1058 complete two-connection wire and cleanup', 'sdk_run_exit'),
    ('large-stdin-oracle-drift', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'run_stdin_oracle_regenerates_identically', '#1058 identical TS fixture sdk-run-stdin', 'sdk_run_exit'),
    ('large-stdin-duplicate-timing', oracle, '"delayed": true,', '"delayed": false,', 'run_large_stdin_matches_ts', '#1136 exact early and delayed stdin coverage', 'sdk_run_exit'),
    ('run-late-owner-cleanup', run_tests, 'drop(child); // final replay owner', 'std::mem::forget(child); // final replay owner', 'fixture_stdin_registered_after_exit_is_joined', '#1136 post-exit writer joined', 'sdk_run_exit'),
    ('resume-stdin-cancel-join', resume_tests, '|| writer.finish()', '|| drop(writer)', 'fixture_stdin_cancel_reaps_writer', '#1097 cancellation joins stdin writer', 'sdk_resume_exit'),
    ('resume-late-owner-cleanup', resume_tests, 'drop(child); // final replay owner', 'std::mem::forget(child); // final replay owner', 'fixture_stdin_registered_after_exit_is_joined', '#1136 post-exit writer joined', 'sdk_resume_exit'),
    ('format-stdin-cancel-join', format_tests, '|| writer.finish()', '|| drop(writer)', 'fixture_stdin_cancel_reaps_writer', '#1097 cancellation joins stdin writer', 'sdk_format_exit'),
    ('format-late-owner-cleanup', format_tests, 'drop(child); // final replay owner', 'std::mem::forget(child); // final replay owner', 'fixture_stdin_registered_after_exit_is_joined', '#1136 post-exit writer joined', 'sdk_format_exit'),
    ('resume-stdin-synchronous', resume_tests, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_resume_exit'),
    ('resume-stdin-cancel-registry', resume_tests, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1097 cancellation joins stdin writer', 'sdk_resume_exit'),
    ('resume-wire-end-connect', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'resume_no_late_connection', '#1086 no third connection', 'sdk_resume_exit'),
    ('format-stdin-synchronous', format_tests, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_format_exit'),
    ('format-stdin-cancel-registry', format_tests, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1097 cancellation joins stdin writer', 'sdk_format_exit'),
    ('format-wire-end-connect', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'format_no_late_connection', '#1097 no third connection', 'sdk_format_exit'),
    ('run-wire-end-rst', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let reset = tokio::net::TcpSocket::new_v4().expect("#1136 RST mutant socket");\n    #[allow(deprecated)]\n    reset.set_linger(Some(std::time::Duration::ZERO)).expect("#1136 RST mutant linger");\n    let reset = reset.connect(std::net::SocketAddr::from(([127, 0, 0, 1], 28999))).await.expect("#1136 RST mutant connects");\n    drop(reset);\n    result', 'run_no_late_connection', '#1058 no third connection', 'sdk_run_exit'),
    ('settings-wire-end-rst', transport, '    transport.close().await;\n    result', '    transport.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let reset = tokio::net::TcpSocket::new_v4().expect("#1136 RST mutant socket");\n    #[allow(deprecated)]\n    reset.set_linger(Some(std::time::Duration::ZERO)).expect("#1136 RST mutant linger");\n    let reset = reset.connect(std::net::SocketAddr::from(([127, 0, 0, 1], 28999))).await.expect("#1136 RST mutant connects");\n    drop(reset);\n    result', 'settings_hello_matches_ts_bytes', '#1029 no reconnect settings-default--argv', 'sdk_settings_exit'),
    ('sessions-list-wire-end-rst', transport, '    transport.close().await;\n    result', '    transport.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let reset = tokio::net::TcpSocket::new_v4().expect("#1136 RST mutant socket");\n    #[allow(deprecated)]\n    reset.set_linger(Some(std::time::Duration::ZERO)).expect("#1136 RST mutant linger");\n    let reset = reset.connect(std::net::SocketAddr::from(([127, 0, 0, 1], 28999))).await.expect("#1136 RST mutant connects");\n    drop(reset);\n    result', 'sessions_list_matches_ts_matrix', '#983 no reconnect sessions-default--argv', 'sdk_sessions_list_exit'),
    ('sessions-messages-wire-end-rst', transport, '    transport.close().await;\n    result', '    transport.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let reset = tokio::net::TcpSocket::new_v4().expect("#1136 RST mutant socket");\n    #[allow(deprecated)]\n    reset.set_linger(Some(std::time::Duration::ZERO)).expect("#1136 RST mutant linger");\n    let reset = reset.connect(std::net::SocketAddr::from(([127, 0, 0, 1], 28999))).await.expect("#1136 RST mutant connects");\n    drop(reset);\n    result', 'sessions_messages_matches_ts_matrix', '#1028 no reconnect sessions-messages--argv', 'sdk_sessions_messages_exit'),
    ('resume-wire-end-rst', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let reset = tokio::net::TcpSocket::new_v4().expect("#1136 RST mutant socket");\n    #[allow(deprecated)]\n    reset.set_linger(Some(std::time::Duration::ZERO)).expect("#1136 RST mutant linger");\n    let reset = reset.connect(std::net::SocketAddr::from(([127, 0, 0, 1], 28999))).await.expect("#1136 RST mutant connects");\n    drop(reset);\n    result', 'resume_no_late_connection', '#1086 no third connection', 'sdk_resume_exit'),
    ('format-wire-end-rst', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let reset = tokio::net::TcpSocket::new_v4().expect("#1136 RST mutant socket");\n    #[allow(deprecated)]\n    reset.set_linger(Some(std::time::Duration::ZERO)).expect("#1136 RST mutant linger");\n    let reset = reset.connect(std::net::SocketAddr::from(([127, 0, 0, 1], 28999))).await.expect("#1136 RST mutant connects");\n    drop(reset);\n    result', 'format_no_late_connection', '#1097 no third connection', 'sdk_format_exit'),
    ('run-spawn-synchronous', run_tests, '.push(StdinWriter::start(stdin, data));', '.push({ let mut stdin = stdin; write_fixture_stdin(&mut stdin, &data); StdinWriter::start(stdin, Vec::new()) });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_run_exit'),
    ('run-spawn-decode', run_tests, 'u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex")', 'u8::from_str_radix(&hex[i..i + 2], 16).map(|byte| byte ^ 1).expect("#1136 stdin hex")', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn decodes complete stdinHex', 'sdk_run_exit'),
    ('run-spawn-registry', run_tests, '.push(StdinWriter::start(stdin, data));', '.extend({ let writer = StdinWriter::start(stdin, data); drop(writer); std::iter::empty::<StdinWriter>() });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn registers stdin writer', 'sdk_run_exit'),
    ('run-blocked-reader-exit', run_tests, '.args(["-c", "read release <&1; exit 23"])', '.args(["-c", "read release <&1; exit 24"])', 'fixture_stdin_writer_yields_before_child_reads', '#1136 blocked reader exit 23 preserved', 'sdk_run_exit'),
    ('run-stdin-hold', run_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["stdinAfterCreate"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_run_exit'),
    ('run-stdin-after-create', run_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["holdStdin"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_run_exit'),
    ('run-stdin-both-flags', run_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if (input["holdStdin"] != true && input["stdinAfterCreate"] != true) || (input["holdStdin"] == true && input["stdinAfterCreate"] == true) {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_run_exit'),
    ('resume-spawn-synchronous', resume_tests, '.push(StdinWriter::start(stdin, data));', '.push({ let mut stdin = stdin; write_fixture_stdin(&mut stdin, &data); StdinWriter::start(stdin, Vec::new()) });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_resume_exit'),
    ('resume-spawn-decode', resume_tests, 'u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex")', 'u8::from_str_radix(&hex[i..i + 2], 16).map(|byte| byte ^ 1).expect("#1136 stdin hex")', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn decodes complete stdinHex', 'sdk_resume_exit'),
    ('resume-spawn-registry', resume_tests, '.push(StdinWriter::start(stdin, data));', '.extend({ let writer = StdinWriter::start(stdin, data); drop(writer); std::iter::empty::<StdinWriter>() });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn registers stdin writer', 'sdk_resume_exit'),
    ('resume-blocked-reader-exit', resume_tests, '.args(["-c", "read release <&1; exit 23"])', '.args(["-c", "read release <&1; exit 24"])', 'fixture_stdin_writer_yields_before_child_reads', '#1136 blocked reader exit 23 preserved', 'sdk_resume_exit'),
    ('resume-stdin-hold', resume_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["stdinAfterCreate"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_resume_exit'),
    ('resume-stdin-after-create', resume_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["holdStdin"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_resume_exit'),
    ('resume-stdin-both-flags', resume_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if (input["holdStdin"] != true && input["stdinAfterCreate"] != true) || (input["holdStdin"] == true && input["stdinAfterCreate"] == true) {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_resume_exit'),
    ('format-spawn-synchronous', format_tests, '.push(StdinWriter::start(stdin, data));', '.push({ let mut stdin = stdin; write_fixture_stdin(&mut stdin, &data); StdinWriter::start(stdin, Vec::new()) });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_format_exit'),
    ('format-spawn-decode', format_tests, 'u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex")', 'u8::from_str_radix(&hex[i..i + 2], 16).map(|byte| byte ^ 1).expect("#1136 stdin hex")', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn decodes complete stdinHex', 'sdk_format_exit'),
    ('format-spawn-registry', format_tests, '.push(StdinWriter::start(stdin, data));', '.extend({ let writer = StdinWriter::start(stdin, data); drop(writer); std::iter::empty::<StdinWriter>() });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn registers stdin writer', 'sdk_format_exit'),
    ('format-blocked-reader-exit', format_tests, '.args(["-c", "read release <&1; exit 23"])', '.args(["-c", "read release <&1; exit 24"])', 'fixture_stdin_writer_yields_before_child_reads', '#1136 blocked reader exit 23 preserved', 'sdk_format_exit'),
    ('format-stdin-hold', format_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["stdinAfterCreate"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_format_exit'),
    ('format-stdin-after-create', format_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["holdStdin"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_format_exit'),
    ('format-stdin-both-flags', format_tests, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if (input["holdStdin"] != true && input["stdinAfterCreate"] != true) || (input["holdStdin"] == true && input["stdinAfterCreate"] == true) {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_format_exit'),
    ('settings-spawn-synchronous', settings, '.push(StdinWriter::start(stdin, data));', '.push({ let mut stdin = stdin; write_fixture_stdin(&mut stdin, &data); StdinWriter::start(stdin, Vec::new()) });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_settings_exit'),
    ('settings-spawn-decode', settings, 'u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex")', 'u8::from_str_radix(&hex[i..i + 2], 16).map(|byte| byte ^ 1).expect("#1136 stdin hex")', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn decodes complete stdinHex', 'sdk_settings_exit'),
    ('settings-spawn-registry', settings, '.push(StdinWriter::start(stdin, data));', '.extend({ let writer = StdinWriter::start(stdin, data); drop(writer); std::iter::empty::<StdinWriter>() });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn registers stdin writer', 'sdk_settings_exit'),
    ('settings-blocked-reader-exit', settings, '.args(["-c", "read release <&1; exit 23"])', '.args(["-c", "read release <&1; exit 24"])', 'fixture_stdin_writer_yields_before_child_reads', '#1136 blocked reader exit 23 preserved', 'sdk_settings_exit'),
    ('settings-stdin-hold', settings, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["stdinAfterCreate"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_settings_exit'),
    ('settings-stdin-after-create', settings, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["holdStdin"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_settings_exit'),
    ('settings-stdin-both-flags', settings, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if (input["holdStdin"] != true && input["stdinAfterCreate"] != true) || (input["holdStdin"] == true && input["stdinAfterCreate"] == true) {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_settings_exit'),
    ('settings-late-owner-cleanup', settings, 'drop(child); // final replay owner', 'std::mem::forget(child); // final replay owner', 'fixture_stdin_registered_after_exit_is_joined', '#1136 post-exit writer joined', 'sdk_settings_exit'),
    ('sessions-list-spawn-synchronous', sessions_list, '.push(StdinWriter::start(stdin, data));', '.push({ let mut stdin = stdin; write_fixture_stdin(&mut stdin, &data); StdinWriter::start(stdin, Vec::new()) });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_sessions_list_exit'),
    ('sessions-list-spawn-decode', sessions_list, 'u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex")', 'u8::from_str_radix(&hex[i..i + 2], 16).map(|byte| byte ^ 1).expect("#1136 stdin hex")', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn decodes complete stdinHex', 'sdk_sessions_list_exit'),
    ('sessions-list-spawn-registry', sessions_list, '.push(StdinWriter::start(stdin, data));', '.extend({ let writer = StdinWriter::start(stdin, data); drop(writer); std::iter::empty::<StdinWriter>() });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn registers stdin writer', 'sdk_sessions_list_exit'),
    ('sessions-list-blocked-reader-exit', sessions_list, '.args(["-c", "read release <&1; exit 23"])', '.args(["-c", "read release <&1; exit 24"])', 'fixture_stdin_writer_yields_before_child_reads', '#1136 blocked reader exit 23 preserved', 'sdk_sessions_list_exit'),
    ('sessions-list-stdin-hold', sessions_list, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["stdinAfterCreate"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_sessions_list_exit'),
    ('sessions-list-stdin-after-create', sessions_list, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["holdStdin"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_sessions_list_exit'),
    ('sessions-list-stdin-both-flags', sessions_list, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if (input["holdStdin"] != true && input["stdinAfterCreate"] != true) || (input["holdStdin"] == true && input["stdinAfterCreate"] == true) {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_sessions_list_exit'),
    ('sessions-list-late-owner-cleanup', sessions_list, 'drop(child); // final replay owner', 'std::mem::forget(child); // final replay owner', 'fixture_stdin_registered_after_exit_is_joined', '#1136 post-exit writer joined', 'sdk_sessions_list_exit'),
    ('sessions-messages-spawn-synchronous', sessions_messages, '.push(StdinWriter::start(stdin, data));', '.push({ let mut stdin = stdin; write_fixture_stdin(&mut stdin, &data); StdinWriter::start(stdin, Vec::new()) });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 stdin writer yields before child reads', 'sdk_sessions_messages_exit'),
    ('sessions-messages-spawn-decode', sessions_messages, 'u8::from_str_radix(&hex[i..i + 2], 16).expect("#1136 stdin hex")', 'u8::from_str_radix(&hex[i..i + 2], 16).map(|byte| byte ^ 1).expect("#1136 stdin hex")', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn decodes complete stdinHex', 'sdk_sessions_messages_exit'),
    ('sessions-messages-spawn-registry', sessions_messages, '.push(StdinWriter::start(stdin, data));', '.extend({ let writer = StdinWriter::start(stdin, data); drop(writer); std::iter::empty::<StdinWriter>() });', 'fixture_stdin_writer_yields_before_child_reads', '#1136 spawn registers stdin writer', 'sdk_sessions_messages_exit'),
    ('sessions-messages-blocked-reader-exit', sessions_messages, '.args(["-c", "read release <&1; exit 23"])', '.args(["-c", "read release <&1; exit 24"])', 'fixture_stdin_writer_yields_before_child_reads', '#1136 blocked reader exit 23 preserved', 'sdk_sessions_messages_exit'),
    ('sessions-messages-stdin-hold', sessions_messages, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["stdinAfterCreate"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_sessions_messages_exit'),
    ('sessions-messages-stdin-after-create', sessions_messages, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if input["holdStdin"] != true {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_sessions_messages_exit'),
    ('sessions-messages-stdin-both-flags', sessions_messages, 'if input["holdStdin"] != true && input["stdinAfterCreate"] != true {', 'if (input["holdStdin"] != true && input["stdinAfterCreate"] != true) || (input["holdStdin"] == true && input["stdinAfterCreate"] == true) {', 'fixture_stdin_deferred_until_requested', '#1136 hold or delayed stdin stays with child', 'sdk_sessions_messages_exit'),
    ('sessions-messages-late-owner-cleanup', sessions_messages, 'drop(child); // final replay owner', 'std::mem::forget(child); // final replay owner', 'fixture_stdin_registered_after_exit_is_joined', '#1136 post-exit writer joined', 'sdk_sessions_messages_exit'),

]

class RestoreError(RuntimeError):
    """Do not run successors on unverified source bytes."""


def check(row):
    name, path, old, new, test, assertion, target = row
    count = path.read_text().count(old)
    if count != 1 or old == new or not test or not assertion:
        raise ValueError(f'{name}: stale/invalid anchor: expected=1 actual={count}')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        errors = []
        for row in mutations:
            try:
                check(row)
            except Exception as error:
                errors.append(str(error))
        for error in errors:
            print('INVALID ' + error)
        print(f'#1136 anchors: {len(mutations) - len(errors)}/{len(mutations)} valid')
        return int(bool(errors))
    if '--check' in args:
        raise SystemExit('--check must be the only argument')
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
    if not args:
        raise SystemExit('usage: --check | [--compile-only] NEW_OUTPUT_DIR [name ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation selection')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)

    def run(label, target, test, run_env, *, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-cli', '--test', target]
        command += (['--no-run'] if compile else
                    [test, '--', '--exact', '--include-ignored', '--show-output', '--test-threads=1'])
        result = run_owned(command, cwd=root, env=run_env, timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    # Prove every tracked Rust build/test input and every judge module is HEAD.
    # Include new files explicitly so a patch-only tree refuses before mutation.
    tracked = subprocess.check_output(['git', 'ls-tree', '-r', '--name-only', 'HEAD', '--', 'src'], cwd=root, text=True).splitlines()
    guarded = list(dict.fromkeys([*(root / name for name in tracked),
        *(row[1] for row in mutations), Path(__file__).resolve(),
        root / 'src/pilotdeck-cli/tests/fixtures/sdk-run-stdin/generate.mjs']))
    results = []
    try:
        # One HEAD-verified window for the entire suite avoids repeatedly
        # rewriting every guarded input and invalidating Cargo fingerprints.
        with mutating_sources(root, guarded) as originals:
            for row in selected:
                name, path, old, new, test, assertion, target = row
                item = dict(name=name, test=test, status='INVALID', reason='not run', compiled=False)
                try:
                    check(row)
                    original = originals[path].decode()
                    try:
                        if not compile_only and not passed(run(name + '-positive', target, test, env), test):
                            raise RuntimeError('positive bound test failed')
                        changed = original.replace(old, new, 1)
                        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                            original.splitlines(True), changed.splitlines(True),
                            fromfile=str(path.relative_to(root)), tofile=str(path.relative_to(root)))))
                        path.write_text(changed)
                        if run(name + '-compile', target, test, env, compile=True).returncode != 0:
                            raise RuntimeError('mutant compile failed; not evidence')
                        item['compiled'] = True
                        if compile_only:
                            item.update(status='COMPILED', reason='mutant no-run passed')
                        else:
                            receipts = ReceiptRun(out, name + '-negative', [(test, assertion)])
                            result = run(name + '-negative', target, test, receipts.environment(env))
                            killed, reason, _ = judge(result, '', [(test, assertion)], receipts)
                            item.update(status='KILLED' if killed else
                                        ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason)
                    finally:
                        # Only restore bytes supplied by the official HEAD guard.
                        try:
                            path.write_bytes(originals[path])
                            if path.read_bytes() != originals[path]:
                                raise RestoreError('restoration mismatch')
                        except Exception as error:
                            raise RestoreError(f'unverified restoration: {error}') from error
                    if not compile_only and not passed(run(name + '-restored', target, test, env), test):
                        item.update(status='INVALID', reason=item['reason'] + '; restored bound test failed')
                except RestoreError:
                    raise
                except Exception as error:
                    item.update(status='INVALID', compiled=False, reason=f'{type(error).__name__}: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        # Enter/restore failures invalidate ALL rows; no stale green summary.
        results = [dict(name=row[0], test=row[4], status='INVALID', compiled=False,
                        reason=f'{type(error).__name__}: {error}') for row in selected]
        for item in results:
            print(f'{item["name"]}: INVALID: {item["reason"]}', flush=True)
    counts = {state: sum(item['status'] == state for item in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts), flush=True)
    return int(any(item['status'] != ('COMPILED' if compile_only else 'KILLED') for item in results))


if __name__ == '__main__':
    raise SystemExit(main())
