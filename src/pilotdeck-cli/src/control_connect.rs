//! pilotdeck-connect/1: bounded JSON-RPC 2.0 over stdin/stdout, one authenticated Gateway
//! connection for calls, live turns and interactive decisions. No mutation replay.
use super::{
    ControlError, JsValue, MAX_INPUT, NETWORK_TIMEOUT, Options, Result, Socket, check_numbers,
    encode, object, open_authenticated, redact, stream::Output, string_is,
};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::HashMap,
    io::{self, BufRead, Read},
    time::Duration,
};
use tokio::time::{Instant, timeout};
use tokio_tungstenite::tungstenite::Message;
const MAX_PENDING: usize = 64;
struct Pending {
    id: JsValue,
    initialize: bool,
    stream: bool,
    next: u64,
    error: Option<JsValue>,
    deadline: Option<Instant>,
}
fn rpc_result(id: JsValue, result: JsValue) -> JsValue {
    object(&[("jsonrpc", "2.0".into()), ("id", id), ("result", result)])
}
fn rpc_error(id: JsValue, code: i32, message: &str, data: Option<JsValue>) -> JsValue {
    let mut error = pilotdeck_protocol::JsObject::new();
    error.insert("code".into(), JsValue::Number(f64::from(code)));
    error.insert("message".into(), message.into());
    if let Some(data) = data {
        error.insert("data".into(), data);
    }
    object(&[
        ("jsonrpc", "2.0".into()),
        ("id", id),
        ("error", JsValue::Object(error)),
    ])
}
fn notice(method: &str, params: JsValue) -> JsValue {
    object(&[
        ("jsonrpc", "2.0".into()),
        ("method", method.into()),
        ("params", params),
    ])
}
fn input() -> Result<tokio::sync::mpsc::Receiver<Result<Vec<u8>>>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    // At most one bounded line waits behind the line being processed. A blocked
    // stdin worker is never joined during shutdown and cannot retain the process.
    std::thread::Builder::new()
        .name("control-connect-input".into())
        .spawn(move || {
            let stdin = io::stdin();
            let mut reader = stdin.lock();
            loop {
                let mut line = Vec::new();
                let result = (&mut reader)
                    .take((MAX_INPUT + 1) as u64)
                    .read_until(b'\n', &mut line);
                let value = match result {
                    Ok(0) => break,
                    Ok(_) if line.len() <= MAX_INPUT => Ok(line),
                    Ok(_) => Err(ControlError::new(
                        "input_error",
                        "JSON-RPC input line exceeds 1 MiB.",
                    )),
                    Err(_) => Err(ControlError::new(
                        "input_error",
                        "Cannot read JSON-RPC input.",
                    )),
                };
                let failed = value.is_err();
                if sender.blocking_send(value).is_err() || failed {
                    break;
                }
            }
        })
        .map_err(|_| ControlError::new("input_error", "Cannot start JSON-RPC input."))?;
    Ok(receiver)
}
fn id(value: &JsValue) -> Option<JsValue> {
    match value.get("id")? {
        JsValue::Null => Some(JsValue::Null),
        JsValue::String(v) if v.units().len() <= 256 => Some(JsValue::String(v.clone())),
        JsValue::Number(v)
            if v.is_finite() && v.fract() == 0.0 && v.abs() <= 9_007_199_254_740_991.0 =>
        {
            Some(JsValue::Number(*v))
        }
        _ => None,
    }
}
fn route(value: &JsValue) -> std::result::Result<(String, JsValue), (&'static str, i32)> {
    let method = value
        .get("method")
        .and_then(JsValue::as_string)
        .and_then(|v| v.to_utf8().ok())
        .ok_or(("Invalid Request", -32600))?;
    let params = value.get("params").cloned().unwrap_or_else(|| object(&[]));
    if params.as_object().is_none() {
        return Err(("params must be an object", -32602));
    }
    let target = match method.as_str() {
        "initialize" => {
            if params.get("protocolVersion").is_some_and(|v| {
                v.as_string()
                    .is_none_or(|s| !s.equals_str("pilotdeck-connect/1"))
            }) {
                return Err(("Unsupported protocolVersion", -32602));
            }
            return Ok(("describe_server".into(), object(&[])));
        }
        "gateway.call" => {
            let name = params
                .get("method")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or(("gateway.call requires method and params", -32602))?;
            let args = params
                .get("params")
                .cloned()
                .ok_or(("gateway.call requires method and params", -32602))?;
            return Ok((name, args));
        }
        "run.submit" => "submit_turn",
        "run.cancel" => "abort_turn",
        "permission.respond" => "permission_decide",
        _ => return Err(("Method not found", -32601)),
    };
    Ok((target.into(), params))
}
pub(super) async fn run(options: &Options, token: &str) -> Result<()> {
    let output = Output::new()?;
    let mut input = input()?;
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    let mut socket = timeout(
        NETWORK_TIMEOUT,
        open_authenticated(options, token, deadline),
    )
    .await
    .map_err(|_| ControlError::new("transport_error", "Gateway handshake timed out."))??;
    let mut pending = HashMap::<String, Pending>::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| {
        ControlError::new("transport_error", "Cannot register shutdown signal.")
    })?;
    let result = tokio::select! {
        result=drive(&mut socket,&mut input,&output,&mut pending,token)=>result,
        _=tokio::signal::ctrl_c()=>Ok(()),
        _=terminate.recv()=>Ok(()),
    };
    // Disconnection/EOF never silently resubmits a turn. Consumers can inspect
    // run_get/run_events after reconnecting with a new explicit connection.
    let _ = timeout(Duration::from_secs(1), socket.close(None)).await;
    result
}
async fn drive(
    socket: &mut Socket,
    input: &mut tokio::sync::mpsc::Receiver<Result<Vec<u8>>>,
    output: &Output,
    pending: &mut HashMap<String, Pending>,
    token: &str,
) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            item=input.recv()=>{
                let Some(line)=item else {return Ok(());};let line=line?;
                if line.iter().all(u8::is_ascii_whitespace){continue;}
                let parsed=std::str::from_utf8(&line).ok().and_then(|s|pilotdeck_protocol::parse_json(s).ok());
                let Some(value)=parsed else {output.deliver(&rpc_error(JsValue::Null,-32700,"Parse error",None)).await?;continue;};
                let request_id=id(&value);
                if value.as_object().is_none()||!string_is(&value,"jsonrpc","2.0")||value.get("method").and_then(JsValue::as_string).is_none()||(value.get("id").is_some()&&request_id.is_none()) {
                    output.deliver(&rpc_error(JsValue::Null,-32600,"Invalid Request",None)).await?;continue;
                }
                // Mutations and streams require a response identity in pilotdeck-connect/1.
                // JSON-RPC notifications receive no response and are ignored.
                let Some(request_id)=request_id else {continue;};
                if check_numbers(&value).is_err(){output.deliver(&rpc_error(request_id,-32602,"Numbers must be finite safe JSON integers",None)).await?;continue;}
                if pending.values().any(|p|p.id==request_id){output.deliver(&rpc_error(request_id,-32600,"Request id is already in use",None)).await?;continue;}
                if pending.len()>=MAX_PENDING {output.deliver(&rpc_error(request_id,-32003,"Too many pending requests",None)).await?;continue;}
                let (method,params)=match route(&value){Ok(route)=>route,Err((message,code))=>{output.deliver(&rpc_error(request_id,code,message,None)).await?;continue;}};
                let stream=method=="submit_turn";
                let key=uuid::Uuid::new_v4().to_string();
                let request=encode(&object(&[("type","request".into()),("id",key.as_str().into()),("method",method.as_str().into()),("params",params)]))?;
                pending.insert(key,Pending{id:request_id,initialize:string_is(&value,"method","initialize"),stream,next:0,error:None,deadline:(!stream).then(||Instant::now()+NETWORK_TIMEOUT)});
                timeout(NETWORK_TIMEOUT,socket.send(Message::Text(request.into()))).await.map_err(|_|ControlError::new("transport_error","Gateway send timed out; operation outcome may be unknown."))?.map_err(|_|ControlError::new("transport_error","Gateway connection closed; operation outcome may be unknown."))?;
            },
            message=socket.next()=>{
                let message=message.ok_or_else(||ControlError::new("transport_error","Gateway disconnected; pending outcomes are unknown. Reconnect and inspect run state."))?.map_err(|_|ControlError::new("transport_error","Gateway connection failed; pending outcomes are unknown."))?;
                if message.is_close(){return Err(ControlError::new("transport_error","Gateway closed; pending outcomes are unknown."));}
                let Message::Text(raw)=message else {timeout(NETWORK_TIMEOUT,socket.flush()).await.map_err(|_|ControlError::new("transport_error","Gateway flush timed out."))?.map_err(|_|ControlError::new("transport_error","Gateway flush failed."))?;continue;};
                let frame=pilotdeck_protocol::parse_json(&raw).map_err(|_|ControlError::new("protocol_error","Invalid Gateway JSON."))?;
                if string_is(&frame,"type","notification") {output.deliver(&notice("gateway.notification",redact(&frame,token))).await?;continue;}
                let Some(key)=frame.get("id").and_then(JsValue::as_string).and_then(|s|s.to_utf8().ok()) else {continue;};
                let Some(state)=pending.get_mut(&key) else {continue;};
                if string_is(&frame,"type","response") {
                    let response=if frame.get("ok").and_then(JsValue::as_bool)==Some(true)&&!state.stream {{
                        let result=redact(frame.get("result").unwrap_or(&JsValue::Null),token);
                        let result=if state.initialize {object(&[("protocolVersion","pilotdeck-connect/1".into()),("transport","gateway-ws".into()),("maxPending",JsValue::Number(MAX_PENDING as f64)),("maxInputBytes",JsValue::Number(MAX_INPUT as f64)),("mutationReplay",JsValue::Bool(false)),("globalEventCursor",JsValue::Null),("server",result)])}else{result};
                        rpc_result(state.id.clone(),result)
                    }} else {rpc_error(state.id.clone(),-32000,"Gateway request failed",frame.get("error").map(|v|redact(v,token)))};
                    output.deliver(&response).await?;pending.remove(&key);continue;
                }
                if !state.stream||!string_is(&frame,"type","event"){return Err(ControlError::new("protocol_error","Unexpected Gateway response type."));}
                if frame.get("seq").and_then(JsValue::as_number)!=Some(state.next as f64){return Err(ControlError::new("protocol_error","Gateway stream sequence is missing or out of order."));}
                let final_frame=frame.get("final").and_then(JsValue::as_bool).ok_or_else(||ControlError::new("protocol_error","Gateway stream final flag is invalid."))?;
                let event=frame.get("event").filter(|v|v.get("type").and_then(JsValue::as_string).is_some()).ok_or_else(||ControlError::new("protocol_error","Gateway stream event is invalid."))?;
                let event=super::stream::redact_event(event,token);
                output.deliver(&notice("run.event",object(&[("requestId",state.id.clone()),("seq",JsValue::Number(state.next as f64)),("final",JsValue::Bool(final_frame)),("event",event.clone())]))).await?;
                if string_is(&event,"type","error"){state.error=Some(event.clone());}
                if final_frame {
                    let response=if let Some(error)=state.error.take(){rpc_error(state.id.clone(),-32001,"Turn failed",Some(error))}else if string_is(&event,"type","turn_completed"){rpc_result(state.id.clone(),object(&[("terminal",event),("lastSeq",JsValue::Number(state.next as f64))]))}else{return Err(ControlError::new("protocol_error","Gateway terminal event is invalid."));};
                    output.deliver(&response).await?;pending.remove(&key);
                }else{state.next+=1;}
            },
            _=tick.tick()=>{
                let expired:Vec<_>=pending.iter().filter(|(_,p)|p.deadline.is_some_and(|t|Instant::now()>=t)).map(|(k,_)|k.clone()).collect();
                for key in expired {if let Some(state)=pending.remove(&key){output.deliver(&rpc_error(state.id,-32002,"Gateway request timed out; outcome may be unknown",None)).await?;}}
            }
        }
    }
}
