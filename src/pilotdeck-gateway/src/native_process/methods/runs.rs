use super::super::requests::Requests;
use super::string;
use crate::{
    runtime::{
        connection::{Call, RequestError},
        turn::read::{self, RunEventsInput},
    },
    storage::RunRef,
};
use pilotdeck_protocol::JsValue;

pub(super) const METHODS: &[&str] = &["run_get", "run_events", "run_reattach"];
pub(super) const HANDLER: super::Handler = super::Handler::AfterAdmission(Requests::run_request);

fn read_error(error: read::TurnReadError) -> RequestError {
    RequestError {
        code: error
            .code
            .unwrap_or_else(|| "gateway_request_failed".into()),
        message: error.message,
        details: None,
        validation: None,
    }
}
impl Requests {
    pub(in crate::native_process) fn run_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            let run = RunRef {
                project_key: Some(self.options.project.to_string_lossy().as_ref().into()),
                session_key: string(&params, "sessionKey")?,
                run_id: string(&params, "runId")?,
            };
            // TS parity: an unconfigured runRegistry makes every run read
            // fail with CAPABILITY_UNAVAILABLE before touching a file.
            let registry: Option<&dyn read::TurnRunReader> = self
                .runs
                .as_deref()
                .map(|runs| runs as &dyn read::TurnRunReader);
            if method == "run_events" {
                read::run_events(
                    registry,
                    &RunEventsInput {
                        run,
                        after_seq: params.get("afterSeq").cloned(),
                        limit: params.get("limit").cloned(),
                    },
                )
                .await
                .map_err(read_error)
            } else if method == "run_reattach" {
                read::run_reattach(registry, &run)
                    .await
                    .map(|v| v.unwrap_or(JsValue::Null))
                    .map_err(read_error)
            } else {
                read::run_get(registry, &run)
                    .await
                    .map(|v| v.unwrap_or(JsValue::Null))
                    .map_err(read_error)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::object;
    use crate::runtime::turn::{gateway::GatewayRequests, publish::TurnRunRegistry};
    use crate::storage::RunAcceptInput;
    use pilotdeck_protocol::JsString;

    #[tokio::test]
    async fn durable_run_reads_share_canonical_submit_identity_for_omitted_and_aliased_projects() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let a = requests.options.project.clone();
        let b = a.parent().unwrap().join("durable-B");
        std::fs::create_dir_all(&b).unwrap();
        let mut b_options = requests.options.clone();
        b_options.project = b.clone();
        crate::native_process::assembly::register_project(&b_options).unwrap();
        let registry = requests.runs.as_ref().unwrap();
        for (root, marker) in [(&a, "only-A"), (&b, "only-B")] {
            let run = RunRef {
                project_key: Some(root.to_string_lossy().as_ref().into()),
                session_key: "same".into(),
                run_id: "same-run".into(),
            };
            registry
                .accept(&RunAcceptInput {
                    run: run.clone(),
                    request_material: marker.into(),
                })
                .await
                .unwrap();
            registry
                .append(
                    &run,
                    &object([
                        ("type", "agent_status".into()),
                        ("runId", "same-run".into()),
                        ("text", marker.into()),
                    ]),
                )
                .await
                .unwrap();
        }
        for (project, expected, marker, foreign) in [
            (None, &a, "only-A", "only-B"),
            (Some(a.join("child/..")), &a, "only-A", "only-B"),
            (Some(b.join("child/..")), &b, "only-B", "only-A"),
        ] {
            let mut params = object([("sessionKey", "same".into()), ("runId", "same-run".into())]);
            if let Some(project) = project
                && let JsValue::Object(values) = &mut params
            {
                values.insert(
                    "projectKey".into(),
                    JsValue::String(project.to_string_lossy().as_ref().into()),
                );
            }
            for method in ["run_get", "run_reattach"] {
                let result = requests
                    .request(method.into(), params.clone())
                    .await
                    .unwrap();
                assert_eq!(
                    result.get("projectKey"),
                    Some(&JsValue::String(JsString::from(
                        expected.to_string_lossy().as_ref()
                    ))),
                    "durable run read must use selected canonical project"
                );
            }
            let events = requests.request("run_events".into(), params).await.unwrap();
            let text = pilotdeck_protocol::stringify_json(&events)
                .unwrap()
                .unwrap();
            assert!(
                text.contains(marker) && !text.contains(foreign),
                "durable run events must be project-scoped: {text}"
            );
        }
        requests.owner.shutdown().await.unwrap();
        requests.project.dispose().await.unwrap();
    }
}
