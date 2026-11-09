//! Native update command policy from `ui/server/services/webUpdateCommand.js`.
//!
//! Release discovery and application are injected; the production offline Web
//! deployment backend lives in `update_release`. Runtime restart ownership
//! stays with the running authenticated Web service.
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateTarget {
    pub tag_name: String,
    pub source_sha: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateStatus {
    pub can_update: bool,
    pub reason: Option<String>,
    pub latest: Option<UpdateTarget>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCommandError {
    pub reason: String,
    pub message: String,
}

impl UpdateCommandError {
    pub fn new(reason: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for UpdateCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UpdateCommandError {}

pub trait UpdateBackend {
    fn check(&mut self) -> Result<UpdateStatus, UpdateCommandError>;
    fn can_restart(&self) -> bool;
    fn apply(
        &mut self,
        target: &UpdateTarget,
        output: &mut dyn FnMut(&str),
    ) -> Result<(), UpdateCommandError>;
    fn restart(&mut self) -> Result<(), UpdateCommandError>;
}

/// Shared CLI/IM policy. A result of 2 means that the installation is already
/// at the latest Release; all other successful outcomes return 0.
pub fn run_web_update_command(
    backend: &mut dyn UpdateBackend,
    check_only: bool,
    restart: bool,
    output: &mut dyn FnMut(&str),
) -> Result<i32, UpdateCommandError> {
    let status = backend.check()?;
    if status.reason.as_deref() == Some("upToDate") {
        output("Already at the latest Release.");
        return Ok(2);
    }
    if !status.can_update {
        let reason = status.reason.as_deref().unwrap_or("checkFailed");
        return Err(UpdateCommandError::new(reason, describe_reason(reason)));
    }
    let latest = status
        .latest
        .ok_or_else(|| UpdateCommandError::new("checkFailed", describe_reason("checkFailed")))?;
    output(&format!(
        "Release available: {} ({})",
        latest.tag_name,
        latest.source_sha.chars().take(8).collect::<String>()
    ));
    if check_only {
        return Ok(0);
    }
    if restart && !backend.can_restart() {
        return Err(UpdateCommandError::new(
            "restartUnavailable",
            "No running Web service was found. Run update without --restart, then start PilotDeck manually.",
        ));
    }
    backend.apply(&latest, output)?;
    if restart {
        if let Err(error) = backend.restart() {
            return Err(UpdateCommandError::new(
                "restartFailed",
                format!(
                    "Update prepared, but restart was not confirmed: {} Restart manually; do not repeat the update.",
                    error.message
                ),
            ));
        }
        output("Update prepared; the running Web service accepted the restart request.");
    } else {
        output("Update prepared. Restart PilotDeck manually to load the new version.");
    }
    Ok(0)
}

pub fn describe_reason(reason: &str) -> String {
    let message = match reason {
        "development" => {
            "Developer branches/worktrees and development runtimes do not support self-update."
        }
        "localChanges" => "Local changes are present. Update this workspace manually.",
        "container" => "Container deployments must update their image manually.",
        "desktop" => "Use the desktop application settings to update this installation.",
        "notGit" => "Self-update requires a standard Git deployment.",
        "unofficial" => "This checkout does not use the official repository.",
        "shallow" => "Shallow Git checkouts do not support self-update.",
        "unknownVersion" => "The current build version cannot be identified.",
        "runtimeMismatch" => {
            "The running version does not match the checkout/build. Restart it before checking again."
        }
        "ahead" => {
            "This deployment is ahead of the latest Release. Automatic downgrades are disabled."
        }
        "diverged" => "This deployment has diverged from the Release. Update it manually.",
        "lockBusy" => "Another update or an unconfirmed cleanup holds the update lock.",
        "restartRequired" => "An update is already prepared. Restart the service first.",
        _ => "The Release update is unavailable. See settings or update manually.",
    };
    format!("{reason}: {message}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Backend {
        statuses: VecDeque<Result<UpdateStatus, UpdateCommandError>>,
        restart_available: bool,
        apply_error: Option<UpdateCommandError>,
        restart_error: Option<UpdateCommandError>,
        calls: Vec<String>,
    }

    impl Backend {
        fn with(status: UpdateStatus) -> Self {
            Self {
                statuses: VecDeque::from([Ok(status)]),
                restart_available: true,
                ..Self::default()
            }
        }
    }

    impl UpdateBackend for Backend {
        fn check(&mut self) -> Result<UpdateStatus, UpdateCommandError> {
            self.calls.push("check".into());
            self.statuses.pop_front().unwrap()
        }

        fn can_restart(&self) -> bool {
            self.restart_available
        }

        fn apply(
            &mut self,
            target: &UpdateTarget,
            output: &mut dyn FnMut(&str),
        ) -> Result<(), UpdateCommandError> {
            self.calls.push(format!("apply:{}", target.tag_name));
            output("Prepared");
            match self.apply_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }

        fn restart(&mut self) -> Result<(), UpdateCommandError> {
            self.calls.push("restart".into());
            match self.restart_error.take() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
    }

    fn available() -> UpdateStatus {
        UpdateStatus {
            can_update: true,
            reason: None,
            latest: Some(UpdateTarget {
                tag_name: "v2026.09.07".into(),
                source_sha: "a".repeat(40),
            }),
        }
    }

    #[test]
    fn command_policy_covers_check_noop_apply_and_restart() {
        let mut backend = Backend::with(UpdateStatus {
            can_update: false,
            reason: Some("upToDate".into()),
            latest: None,
        });
        let mut output: Vec<String> = Vec::new();
        assert_eq!(
            run_web_update_command(&mut backend, false, true, &mut |line| output
                .push(line.into()))
            .unwrap(),
            2
        );
        assert_eq!(output, ["Already at the latest Release."]);
        assert_eq!(backend.calls, ["check"]);

        let mut backend = Backend::with(available());
        output.clear();
        assert_eq!(
            run_web_update_command(&mut backend, true, true, &mut |line| output
                .push(line.into()))
            .unwrap(),
            0
        );
        assert_eq!(backend.calls, ["check"]);
        assert!(output[0].contains("v2026.09.07 (aaaaaaaa)"));

        let mut backend = Backend::with(available());
        output.clear();
        assert_eq!(
            run_web_update_command(&mut backend, false, true, &mut |line| output
                .push(line.into()))
            .unwrap(),
            0
        );
        assert_eq!(backend.calls, ["check", "apply:v2026.09.07", "restart"]);
        assert!(output.last().unwrap().contains("accepted the restart"));

        let mut backend = Backend::with(available());
        output.clear();
        assert_eq!(
            run_web_update_command(&mut backend, false, false, &mut |line| output
                .push(line.into()))
            .unwrap(),
            0
        );
        assert_eq!(backend.calls, ["check", "apply:v2026.09.07"]);
        assert!(output.last().unwrap().contains("manually"));
    }

    #[test]
    fn command_policy_refuses_before_apply_and_preserves_failure_order() {
        for reason in [
            "development",
            "container",
            "localChanges",
            "ahead",
            "runtimeMismatch",
            "desktop",
        ] {
            let mut backend = Backend::with(UpdateStatus {
                can_update: false,
                reason: Some(reason.into()),
                latest: None,
            });
            let error = run_web_update_command(&mut backend, false, true, &mut |_| {}).unwrap_err();
            assert_eq!(error.reason, reason);
            assert_eq!(backend.calls, ["check"]);
        }

        let mut backend = Backend::with(available());
        backend.restart_available = false;
        let error = run_web_update_command(&mut backend, false, true, &mut |_| {}).unwrap_err();
        assert_eq!(error.reason, "restartUnavailable");
        assert_eq!(backend.calls, ["check"]);

        let mut backend = Backend::with(available());
        backend.apply_error = Some(UpdateCommandError::new("processStopFailed", "stop failed"));
        let error = run_web_update_command(&mut backend, false, true, &mut |_| {}).unwrap_err();
        assert_eq!(error.reason, "processStopFailed");
        assert_eq!(backend.calls, ["check", "apply:v2026.09.07"]);

        let mut backend = Backend::with(available());
        backend.restart_error = Some(UpdateCommandError::new("restartUnconfirmed", "rejected"));
        let error = run_web_update_command(&mut backend, false, true, &mut |_| {}).unwrap_err();
        assert_eq!(error.reason, "restartFailed");
        assert!(error.message.contains("Update prepared"));
        assert_eq!(backend.calls, ["check", "apply:v2026.09.07", "restart"]);
    }
}
