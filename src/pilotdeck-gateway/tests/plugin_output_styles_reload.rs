use pilotdeck_gateway::runtime::plugins::{
    PluginOutputStyleContribution, PluginRuntime, PluginRuntimeOptions,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write(path: &Path, value: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, value).unwrap();
}
fn styles(items: Vec<PluginOutputStyleContribution>) -> Value {
    Value::Array(items.into_iter().map(|style| {
        let mut value=json!({"name":style.name,"content":style.content,"plugin":style.plugin,"source":style.source.as_str()});
        if let Some(description)=style.description {value["description"]=json!(description);}
        value
    }).collect())
}
async fn actual(runtime: &PluginRuntime, result: Value) -> Value {
    json!({"result":result,"styles":styles(runtime.list_output_styles().await),
        "view":styles(runtime.create_view(vec![]).await.list_output_styles()),
        "commands":runtime.all_commands().await.into_iter().map(|c|json!({"name":c.name,"content":c.content})).collect::<Vec<_>>(),
        "generation":runtime.generation().await})
}
#[tokio::test]
async fn style_only_reload_matches_fixed_typescript_and_leaves_other_contributions_untouched() {
    let oracle: Value =
        serde_json::from_str(include_str!("fixtures/styles-reload/oracle.json")).unwrap();
    let root = Scratch(std::env::temp_dir().join(format!("pdrs-style-{}", uuid::Uuid::new_v4())));
    let project = root.0.join("project");
    let home = root.0.join("home");
    let plugin = project.join(".pilotdeck/plugins/p");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(plugin.join("output-styles")).unwrap();
    write(
        &plugin.join("plugin.json"),
        r#"{"name":"p","version":"1.0.0"}"#,
    );
    write(
        &plugin.join("commands/a.md"),
        "---\nname: helper\ndescription: old\n---\nOriginal command\n",
    );
    let runtime = PluginRuntime::new(PluginRuntimeOptions {
        project_root: project,
        pilot_home: home,
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: HashMap::new(),
    });
    let check = |mut got: Value, index: usize| {
        got["name"] = oracle["results"][index]["name"].clone();
        assert_eq!(got, oracle["results"][index]);
    };
    runtime.refresh().await.unwrap();
    check(actual(&runtime, Value::Null).await, 0);
    let style = plugin.join("output-styles/test.md");
    write(
        &style,
        "---\nname: concise\ndescription: first\n---\nFirst version\n",
    );
    let changed = runtime.reload_output_styles().await.unwrap();
    check(actual(&runtime, json!({"changed":changed})).await, 1);
    write(
        &plugin.join("commands/a.md"),
        "---\nname: helper\ndescription: new\n---\nChanged command\n",
    );
    write(
        &style,
        "---\nname: concise\ndescription: second\n---\nSecond version\n",
    );
    let changed = runtime.reload_output_styles().await.unwrap();
    check(actual(&runtime, json!({"changed":changed})).await, 2);
    let changed = runtime.reload_output_styles().await.unwrap();
    check(actual(&runtime, json!({"changed":changed})).await, 3);
    fs::remove_file(style).unwrap();
    let changed = runtime.reload_output_styles().await.unwrap();
    check(actual(&runtime, json!({"changed":changed})).await, 4);
    runtime.refresh().await.unwrap();
    check(actual(&runtime, Value::Null).await, 5);
    runtime.dispose().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn style_reload_refuses_fifo_and_oversized_input_without_publishing_partial_state() {
    use std::{os::unix::fs::OpenOptionsExt, time::Duration};
    let root =
        Scratch(std::env::temp_dir().join(format!("pdrs-style-bounds-{}", uuid::Uuid::new_v4())));
    let project = root.0.join("project");
    let home = root.0.join("home");
    let plugin = project.join(".pilotdeck/plugins/p");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(plugin.join("output-styles")).unwrap();
    let manifest = plugin.join("plugin.json");
    write(&manifest, r#"{"name":"p","version":"1.0.0"}"#);
    let style = plugin.join("output-styles/a.md");
    write(&style, "before");
    let runtime = PluginRuntime::new(PluginRuntimeOptions {
        project_root: project,
        pilot_home: home,
        builtin_skills_root: None,
        builtin_plugins: vec![],
        builtin_plugins_enabled: HashMap::new(),
    });
    runtime.refresh().await.unwrap();
    let before = runtime.list_output_styles().await;
    fs::remove_file(&manifest).unwrap();
    let status = std::process::Command::new("/usr/bin/mkfifo")
        .arg(&manifest)
        .status()
        .unwrap();
    assert!(status.success());
    let result = tokio::time::timeout(Duration::from_secs(2), runtime.reload_output_styles()).await;
    if result.is_err() {
        // Unblock a broken reader before failing so runtime shutdown cannot hang.
        let _ = fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&manifest);
    }
    assert!(result.expect("FIFO manifest blocked a worker").is_ok());
    fs::remove_file(&manifest).unwrap();
    write(&manifest, r#"{"name":"p","version":"1.0.0"}"#);
    // Sparse file exercises the cap without a large test fixture.
    fs::OpenOptions::new()
        .write(true)
        .open(&style)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    let error = runtime.reload_output_styles().await.unwrap_err();
    assert_eq!(error.code, Some("STYLE_RELOAD_LIMIT"));
    // FIFO omission was a completed reload; restore original then assert failed
    // over-limit reload does not alter the immediately prior registry.
    write(&style, "before");
    runtime.reload_output_styles().await.unwrap();
    let retained = runtime.list_output_styles().await;
    fs::OpenOptions::new()
        .write(true)
        .open(&style)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(runtime.reload_output_styles().await.is_err());
    assert_eq!(runtime.list_output_styles().await, retained);
    assert_eq!(before, retained);
    runtime.dispose().await.unwrap();
}
