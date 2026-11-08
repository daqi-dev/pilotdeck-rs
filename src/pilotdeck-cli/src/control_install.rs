//! First installation from an explicitly approved local source checkout.
use pilotdeck_cli::update::{self, Builder, InstallManifest, NativeBuilder};
use std::{collections::HashMap, path::PathBuf};
pub(super) const HELP: &str = "pilotdeck-control install --root <new-private-directory> --source <clean-checkout> --remote <approved-git-remote> [--ref refs/heads/main] [--bundled-skills-root <existing-directory>]\n\nBuilds all four native programs from one fixed source commit on this machine.\nNo prebuilt download is used. The root must not exist; existing installations use pilotdeck update.\nWrites one JSON result to stdout; errors, including an unconfirmed publication, go to stderr.";
pub(super) fn run(
    args: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<serde_json::Value, update::Error> {
    let invalid = || update::Error::new("usage", HELP);
    let mut args = args.into_iter();
    let mut values = HashMap::new();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| invalid())?;
        if !["--root", "--source", "--remote", "--ref", "--bundled-skills-root"].contains(&key.as_str()) {
            return Err(invalid());
        }
        let value = args
            .next()
            .ok_or_else(invalid)?
            .into_string()
            .map_err(|_| invalid())?;
        if value.is_empty() || values.insert(key, value).is_some() {
            return Err(invalid());
        }
    }
    let root = PathBuf::from(values.remove("--root").ok_or_else(invalid)?);
    let source = std::fs::canonicalize(values.remove("--source").ok_or_else(invalid)?)
        .map_err(|e| update::Error::new("invalidSource", e.to_string()))?;
    let remote = values.remove("--remote").ok_or_else(invalid)?;
    let reference = values
        .remove("--ref")
        .unwrap_or_else(|| "refs/heads/main".into());
    let mut builder = NativeBuilder::new("git".into(), "cargo".into())?;
    builder.set_progress(false);
    let sha = builder.git(&source, &["rev-parse", "HEAD"])?;
    let policy = InstallManifest {
        schema_version: 1,
        install_id: uuid::Uuid::new_v4().to_string(),
        approved_remote: remote,
        approved_ref: reference,
    };
    let skills = values.remove("--bundled-skills-root").map(PathBuf::from);
    let installation = update::bootstrap_with_external_skills(
        &root, policy, &source, &sha, &mut builder, skills.as_deref()
    )?;
    Ok(
        serde_json::json!({"version":"pilotdeck-control/1","ok":true,"root":installation.root,"installId":installation.policy.install_id,"generation":installation.current.generation,"sourceSha":installation.current.source_sha,"binaries":update::BINARIES}),
    )
}
