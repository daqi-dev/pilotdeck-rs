use super::*;
use crate::runtime::{project::SandboxMode, tools::sandbox::NativeSandbox};

#[tokio::test]
async fn enforcing_filesystem_provider_keeps_bypass_inside_its_mode() {
    // Keep the unavailable-provider tests intact. Here the explicit native
    // provider enables the actual write tools and enforces their mutation path.
    let readonly = Arc::new(NativeSandbox::new(
        SandboxMode::ReadOnly,
        std::env::temp_dir(),
    ));
    let h = harness_with_provider(
        "allow",
        "bypassPermissions",
        Some("read-only"),
        None,
        Some(readonly),
    );
    let result = invoke(
        &h,
        "write_file",
        "readonly-write",
        crate::metadata::object([
            ("file_path", "blocked.txt".into()),
            ("content", "BLOCKED".into()),
        ]),
    )
    .await;
    assert_ne!(result.get("type"), Some(&"success".into()));
    assert!(
        format!("{result:?}").contains("permission_denied"),
        "{result:?}"
    );
    assert!(!h.root.join("blocked.txt").exists());
    let workspace = Arc::new(NativeSandbox::new(
        SandboxMode::WorkspaceWrite,
        std::env::temp_dir(),
    ));
    let h = harness_with_provider(
        "allow",
        "bypassPermissions",
        Some("workspace-write"),
        None,
        Some(workspace),
    );
    success(
        &invoke(
            &h,
            "write_file",
            "allowed-write",
            crate::metadata::object([
                ("file_path", "allowed.txt".into()),
                ("content", "ALLOWED".into()),
            ]),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(h.root.join("allowed.txt")).unwrap(),
        "ALLOWED"
    );
    success(
        &invoke(
            &h,
            "read_file",
            "read-allowed",
            crate::metadata::object([("file_path", "allowed.txt".into())]),
        )
        .await,
    );
    success(
        &invoke(
            &h,
            "edit_file",
            "edit-allowed",
            crate::metadata::object([
                ("file_path", "allowed.txt".into()),
                ("old_string", "ALLOWED".into()),
                ("new_string", "UPDATED".into()),
            ]),
        )
        .await,
    );
    assert_eq!(
        std::fs::read_to_string(h.root.join("allowed.txt")).unwrap(),
        "UPDATED"
    );

    std::os::unix::fs::symlink("/pdrs-never-create-outside", h.root.join("link")).unwrap();
    let result = invoke(
        &h,
        "write_file",
        "escape-write",
        crate::metadata::object([
            ("file_path", "link/blocked.txt".into()),
            ("content", "BLOCKED".into()),
        ]),
    )
    .await;
    assert_ne!(result.get("type"), Some(&"success".into()));
    assert!(
        format!("{result:?}").contains("permission_denied"),
        "{result:?}"
    );
}

#[tokio::test]
async fn background_provider_identity_cannot_be_replaced_by_a_scheduler_marker() {
    let sandbox = Arc::new(NativeSandbox::new(
        SandboxMode::WorkspaceWrite,
        std::env::temp_dir(),
    ));
    let h = harness_with_provider(
        "allow",
        "bypassPermissions",
        Some("workspace-write"),
        None,
        Some(sandbox.clone()),
    );
    let unconfined = crate::runtime::background::BackgroundTaskRuntime::default();
    let tools = crate::runtime::tools::builtin::task::task_tools_with_sandbox(
        Some(unconfined.clone()),
        sandbox,
    );
    let context = h.context.prepare(JsValue::Undefined).await.unwrap();
    let context = context
        .runtime
        .downcast::<crate::host::modules::tool_context::ToolRuntimeContext>()
        .unwrap_or_else(|_| panic!("expected actual host tool context"));
    let result = tools[0]
        .execute(
            crate::metadata::object([("command", "printf BAD > forbidden.txt".into())]),
            crate::runtime::tools::ToolRunContext::new(context),
        )
        .await;
    let disposed = unconfined.dispose().await;
    assert!(disposed.is_ok());
    assert_eq!(result.unwrap_err().code, "sandbox_unavailable");
    assert!(unconfined.list(&Default::default(), None).is_empty());
    assert!(!h.root.join("forbidden.txt").exists());
}
