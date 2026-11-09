//! Existing UI cron endpoints, backed by the same local automation owner.
use super::{Automation, always_on::AlwaysOnError};
use crate::{Body, http_proxy::plain};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming, header};
use serde_json::{Value, json};

pub fn handles(path: &str) -> bool {
    path == "/api/always-on/cron-jobs" || path.starts_with("/api/always-on/cron-jobs/")
}
fn response(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            Full::new(Bytes::from(
                serde_json::to_vec(&value).expect("JSON response"),
            ))
            .map_err(|never| match never {})
            .boxed(),
        )
        .expect("HTTP response")
}
fn fail(status: StatusCode, message: &str) -> Response<Body> {
    response(status, json!({"error":message}))
}
pub async fn serve(
    automation: Option<&Automation>,
    token: &str,
    request: Request<Incoming>,
) -> Response<Body> {
    if request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {token}").as_str())
    {
        return fail(StatusCode::UNAUTHORIZED, "Unauthorized");
    }
    let Some(automation) = automation else {
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            "Native automation is unavailable.",
        );
    };
    let method = request.method().clone();
    let uri = request.uri().clone();
    let mut body = request.into_body();
    let mut bytes = Vec::new();
    while let Some(chunk) = body.frame().await {
        let chunk = match chunk {
            Ok(c) => c,
            Err(_) => return fail(StatusCode::BAD_REQUEST, "Invalid request body."),
        };
        if let Some(data) = chunk.data_ref() {
            if bytes.len() + data.len() > 1024 * 1024 {
                return fail(StatusCode::PAYLOAD_TOO_LARGE, "Request body exceeds 1 MiB.");
            }
            bytes.extend_from_slice(data);
        }
    }
    let mut params = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(p) if p.is_object() => p,
            _ => return fail(StatusCode::BAD_REQUEST, "Invalid JSON object."),
        }
    };
    for (key, value) in url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()) {
        if key == "projectKey" && !params["projectKey"].as_str().is_some_and(|s| !s.is_empty()) {
            params["projectKey"] = value.as_ref().into();
        }
    }
    let (rpc, task) = match endpoint(method.as_str(), uri.path()) {
        Some(p) => p,
        None => return plain(StatusCode::NOT_FOUND, "not found"),
    };
    if let Some(task) = task {
        params["taskId"] = task.into();
    }
    if rpc == "cron_list" {
        params["includeHistory"] = true.into();
        params["limit"] = 50.into();
    }
    if rpc == "cron_create" || rpc == "cron_update" {
        let message = params["message"].as_str().unwrap_or("").trim().to_owned();
        if message.is_empty() {
            return fail(StatusCode::BAD_REQUEST, "Cron message is required.");
        }
        params["message"] = message.into();
        if !params["projectKey"].as_str().is_some_and(|s| !s.is_empty()) {
            return fail(StatusCode::BAD_REQUEST, "Cron projectKey is required.");
        }
        if !params["schedule"].is_object() {
            return fail(
                StatusCode::BAD_REQUEST,
                if rpc == "cron_create" {
                    "Cron schedule is required."
                } else {
                    "Cron schedule is invalid."
                },
            );
        }
        if rpc == "cron_create" {
            params["channelKey"] = "web".into();
        }
        if rpc == "cron_update"
            && !params["expectedRevision"]
                .as_u64()
                .is_some_and(|v| v <= 9_007_199_254_740_991)
        {
            return fail(
                StatusCode::BAD_REQUEST,
                "Cron expectedRevision must be a non-negative integer.",
            );
        }
    }
    if rpc == "cron_delete" {
        params["stopRunning"] = true.into();
    }
    match automation.handle(rpc, &params).await {
        Ok(result) if rpc == "cron_list" => response(StatusCode::OK, overview(&result)),
        Ok(result) if rpc == "cron_update" && result["updated"] != true => {
            let (status, message, code) = match result["reason"].as_str() {
                Some("running") => (
                    StatusCode::CONFLICT,
                    "Cron task is running. Stop it before editing.",
                    "cron_running",
                ),
                Some("conflict") => (
                    StatusCode::CONFLICT,
                    "Cron task changed. Refresh and try again.",
                    "cron_conflict",
                ),
                _ => (
                    StatusCode::NOT_FOUND,
                    "Cron task was not found.",
                    "cron_not_found",
                ),
            };
            response(status, json!({"error":message,"code":code}))
        }
        Ok(result) => response(StatusCode::OK, result),
        Err(AlwaysOnError { code, message }) => {
            let status = if rpc == "cron_update"
                && (code.contains("invalid")
                    || message.starts_with("Invalid Cron")
                    || message.starts_with("Cron "))
            {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            response(status, json!({"error":message}))
        }
    }
}
fn endpoint<'a>(method: &str, path: &'a str) -> Option<(&'static str, Option<&'a str>)> {
    let tail = path.strip_prefix("/api/always-on/cron-jobs")?;
    if tail.is_empty() {
        return match method {
            "GET" => Some(("cron_list", None)),
            "POST" => Some(("cron_create", None)),
            _ => None,
        };
    }
    let mut parts = tail.strip_prefix('/')?.split('/');
    let id = parts.next().filter(|s| !s.is_empty())?;
    let action = parts.next();
    if parts.next().is_some() {
        return None;
    }
    match (method, action) {
        ("PATCH", None) => Some(("cron_update", Some(id))),
        ("DELETE", None) => Some(("cron_delete", Some(id))),
        ("POST", Some("run-now")) => Some(("cron_run_now", Some(id))),
        ("POST", Some("stop")) => Some(("cron_stop", Some(id))),
        _ => None,
    }
}
fn overview(result: &Value) -> Value {
    let jobs=result["tasks"].as_array().into_iter().flatten().map(|task|{
        let latest=result["recentRuns"].as_array().into_iter().flatten().filter(|run|run["taskId"]==task["taskId"]).max_by_key(|run|run["startedAt"].as_str().unwrap_or(""));
        let recurring=task["schedule"]["type"]=="cron";
        let mut job=json!({"id":task["taskId"],"projectKey":task["projectKey"],"cron":if recurring {task["schedule"]["expression"].clone()}else{json!("")},"schedule":task["schedule"],"timezone":task.get("timezone").or(task["schedule"].get("timezone")),"revision":task["revision"].as_u64().unwrap_or(0),"prompt":task["message"],"createdAt":task["createdAt"],"nextRunAt":task["nextRunAt"],"recurring":recurring,"permanent":recurring,"manualOnly":false,"status":if task["status"]=="running"{"running"}else{"scheduled"},"latestRun":null});
        if let Some(run)=latest {
            let status=match run["outcome"].as_str(){Some("completed")=>"completed",Some("stopped"|"aborted")=>"failed",Some("failed")=>"failed",_=>if run["finishedAt"].is_string(){"completed"}else{"running"}};
            job["latestRun"]=json!({"status":status,"runId":run["runId"],"startedAt":run["startedAt"],"taskId":run["taskId"],"sessionId":run["sessionKey"]});
            if let Some(s)=run["startedAt"].as_str().and_then(|s|pilotdeck_transport::system_clock::parse_node22_date(&s.into()).ok().flatten()){job["lastFiredAt"]=json!(s);}
        } job
    }).collect::<Vec<_>>();
    json!({"jobs":jobs})
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_http_mapping_and_latest_run_projection() {
        assert_eq!(
            endpoint("POST", "/api/always-on/cron-jobs/a/run-now"),
            Some(("cron_run_now", Some("a")))
        );
        assert!(endpoint("POST", "/api/always-on/cron-jobs/a/run-now/x").is_none());
        let value = overview(
            &json!({"tasks":[{"taskId":"a","message":"m","revision":1,"schedule":{"type":"cron","expression":"* * * * *"}}],"recentRuns":[{"taskId":"a","runId":"b","startedAt":"2026-10-27T00:00:00Z","outcome":"stopped"}]}),
        );
        assert_eq!(value["jobs"][0]["latestRun"]["status"], "failed");
    }
}
