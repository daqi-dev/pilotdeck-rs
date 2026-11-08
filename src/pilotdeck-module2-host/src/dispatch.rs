//! 派发接缝：`module_call` 请求 → [`HostCall`] → `HostExchange` → response/error 帧。
//!
//! 边界：
//! - 本层不实现具体宿主能力（模型/工具/权限…），只负责**路由与帧转换**；
//! - 未实现的能力**显式拒绝**（`HOST_METHOD_UNSUPPORTED` / `HOST_LIFECYCLE_UNSUPPORTED`），
//!   绝不返回"默认成功"；
//! - `execute` / `cancel` / `status` / `resume` / `ack` / `hello` / `capabilities` 是
//!   loop 生命周期操作，桥接它们需要 kernel 的作业与流机制（后续轮次），本轮显式标未接。

use serde_json::Value;

use crate::host_methods::host_method;
use crate::protocol::{Frame, RequestFrame, ResponseFrame, Retryability};

/// 一次宿主调用（`module_call` 的 module/method/payload）。
#[derive(Clone, Debug, PartialEq)]
pub struct HostCall {
    pub module: String,
    pub method: String,
    pub params: Value,
}

/// 宿主调用错误（映射到帧上的 `error` 与 `code`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    pub code: String,
    pub message: String,
    pub retryability: Retryability,
}

impl HostError {
    pub fn unsupported(call: &HostCall) -> Self {
        Self {
            code: "HOST_METHOD_UNSUPPORTED".to_string(),
            message: format!(
                "host method is not bridged: {}.{}",
                call.module, call.method
            ),
            retryability: Retryability::Unsafe,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "HOST_CALL_INVALID".to_string(),
            message: message.into(),
            retryability: Retryability::Unsafe,
        }
    }
}

/// 宿主实现接缝：kernel 服务（或 L-C 适配）实现它，桥接层只依赖这个 trait。
pub trait HostExchange: Send + Sync {
    fn call(&self, call: &HostCall) -> Result<Value, HostError>;
}

/// 派发一个已解码的请求帧；返回要写回 sidecar 的帧。
///
/// `response_id`：为新帧分配 id（由连接层单调分配）。
pub fn dispatch(
    exchange: &dyn HostExchange,
    request: &RequestFrame,
    response_id: impl FnOnce() -> String,
) -> Frame {
    match request.method.as_str() {
        "module_call" => dispatch_module_call(exchange, request, response_id()),
        "hello" | "capabilities" => Frame::Response(ResponseFrame::failed(
            response_id(),
            request.message_id.clone(),
            "HOST_LIFECYCLE_UNSUPPORTED",
        )),
        _ => Frame::Response(ResponseFrame::failed(
            response_id(),
            request.message_id.clone(),
            "HOST_LIFECYCLE_UNSUPPORTED",
        )),
    }
}

fn dispatch_module_call(
    exchange: &dyn HostExchange,
    request: &RequestFrame,
    response_id: String,
) -> Frame {
    let Some(target) = request.module else {
        return rejected(
            request,
            response_id,
            HostError::invalid("module_call is missing a valid module target"),
        );
    };
    // 原 Module2 dispatcher 以 payload.operation 为权威；顶层扩展不参与路由。
    let Some(method) = request
        .payload
        .as_ref()
        .and_then(|payload| payload.get("operation"))
        .and_then(Value::as_str)
        .filter(|method| !method.is_empty())
    else {
        return rejected(
            request,
            response_id,
            HostError::invalid("module_call is missing payload.operation"),
        );
    };
    let method = method.to_owned();
    if host_method(target.as_str(), &method).is_none() {
        return rejected(
            request,
            response_id,
            HostError {
                code: "HOST_METHOD_UNSUPPORTED".into(),
                message: format!("host method is not bridged: {}.{}", target.as_str(), method),
                retryability: Retryability::Unsafe,
            },
        );
    }
    let call = HostCall {
        module: target.as_str().to_string(),
        method,
        params: request.payload.clone().unwrap_or(Value::Null),
    };
    match exchange.call(&call) {
        Ok(payload) => {
            let mut response = ResponseFrame::accepted(response_id, request.message_id.clone());
            response.request_id = request.request_id.clone();
            response.payload = Some(payload);
            Frame::Response(response)
        }
        Err(error) => rejected(request, response_id, error),
    }
}

fn rejected(request: &RequestFrame, response_id: String, error: HostError) -> Frame {
    let mut response =
        ResponseFrame::failed(response_id, request.message_id.clone(), error.code.clone());
    response.request_id = request.request_id.clone();
    if matches!(
        error.code.as_str(),
        "HOST_RESULT_UNKNOWN" | "CHILD_OUTCOME_UNKNOWN"
    ) {
        response.outcome = Some("result_unknown".into());
    }
    response.error = Some(
        serde_json::json!({"code":error.code,"message":error.message,"retryability":error.retryability.as_str()}),
    );
    Frame::Response(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::decode_line;

    struct EchoExchange;

    impl HostExchange for EchoExchange {
        fn call(&self, call: &HostCall) -> Result<Value, HostError> {
            if call.module == "capability" && call.method == "list_tools" {
                Ok(serde_json::json!({"tools": []}))
            } else {
                Err(HostError::unsupported(call))
            }
        }
    }

    fn module_call_request(module: &str, method: &str) -> RequestFrame {
        let line = format!(
            r#"{{"kind":"request","messageId":"m1","method":"module_call","runId":"r","operationId":"o","requestId":"q","module":"{module}","payload":{{"operation":"{method}"}}}}"#
        );
        match decode_line(&line).expect("decode") {
            Frame::Request(request) => request,
            other => panic!("expected request, got {other:?}"),
        }
    }

    #[test]
    fn routes_bridged_host_call_to_exchange() {
        let exchange = EchoExchange;
        let request = module_call_request("capability", "list_tools");
        let Frame::Response(response) = dispatch(&exchange, &request, || "resp-1".to_string())
        else {
            panic!("expected response")
        };
        assert!(response.ok);
        assert!(!response.final_field);
        assert_eq!(response.in_reply_to, "m1");
        assert_eq!(
            response
                .payload
                .as_ref()
                .and_then(|p| p.get("tools"))
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(0)
        );
    }

    #[test]
    fn explicit_unknown_preserves_request_identity_without_reclassifying_known_failures() {
        struct Reject(&'static str);
        impl HostExchange for Reject {
            fn call(&self, _: &HostCall) -> Result<Value, HostError> {
                Err(HostError {
                    code: self.0.into(),
                    message: "original acceptance unresolved".into(),
                    retryability: Retryability::Unsafe,
                })
            }
        }
        let request = module_call_request("capability", "execute");
        for code in [
            "HOST_RESULT_UNKNOWN",
            "CHILD_OUTCOME_UNKNOWN",
            "POLICY_DENIED",
        ] {
            let Frame::Response(response) = dispatch(&Reject(code), &request, || "response".into())
            else {
                panic!("expected response")
            };
            assert_eq!(response.in_reply_to, "m1");
            assert_eq!(response.request_id.as_deref(), Some("q"));
            assert_eq!(
                response.error.as_ref().unwrap()["message"],
                "original acceptance unresolved"
            );
            assert_eq!(
                response.outcome.as_deref(),
                Some(if code == "POLICY_DENIED" {
                    "failed"
                } else {
                    "result_unknown"
                })
            );
            assert_eq!(response.error.unwrap()["code"], code);
        }
    }

    #[test]
    fn rejects_unknown_host_method_explicitly() {
        let exchange = EchoExchange;
        let request = module_call_request("capability", "bogus");
        let Frame::Response(error) = dispatch(&exchange, &request, || "err-1".to_string()) else {
            panic!("expected response frame")
        };
        assert_eq!(error.code.as_deref(), Some("HOST_METHOD_UNSUPPORTED"));
        assert_eq!(error.in_reply_to, "m1");
    }

    #[test]
    fn lifecycle_requests_are_explicitly_unsupported_not_fake_success() {
        let exchange = EchoExchange;
        let line = r#"{"kind":"request","messageId":"m9","method":"execute","runId":"r","operationId":"o","requestId":"q","payload":{}}"#;
        let Frame::Request(request) = decode_line(line).expect("decode") else {
            panic!("expected request")
        };
        let Frame::Response(response) = dispatch(&exchange, &request, || "resp-9".to_string())
        else {
            panic!("expected response")
        };
        assert!(!response.ok);
        assert!(response.final_field);
        assert_eq!(response.outcome.as_deref(), Some("failed"));
        assert_eq!(response.code.as_deref(), Some("HOST_LIFECYCLE_UNSUPPORTED"));
    }
}
