use super::*;
use std::os::unix::fs::PermissionsExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn inbound_and_outbound_media_use_authenticated_http_private_cache_and_fallbacks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let uploads = Arc::new(StdMutex::new(Vec::<(String, Vec<u8>)>::new()));
    let recorded = uploads.clone();
    let server = tokio::spawn(async move {
        let mut work = JoinSet::new();
        loop {
            let (mut io, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            work.spawn(async move {
        let mut header=Vec::new();while !header.ends_with(b"\r\n\r\n") {header.push(io.read_u8().await.unwrap());}let header=String::from_utf8(header).unwrap();let path=header.split_whitespace().nth(1).unwrap().to_owned();let length=header.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n|n.trim().parse::<usize>().ok())).unwrap_or(0);let mut body=vec![0;length];io.read_exact(&mut body).await.unwrap();
        let (status,location,mime,response)=if path.ends_with("/tenant_access_token/internal") {(200,None,"application/json",br#"{"code":0,"tenant_access_token":"token","expire":7200}"#.to_vec())}else {
            assert!(header.to_ascii_lowercase().contains("authorization: bearer token"));
            if path.contains("/resources/redirect") {(302,Some("/open-apis/im/v1/messages/m/id3/resources/final?type=image"),"text/plain",b"redirect".to_vec())}else if path.contains("/resources/") {(200,None,"application/octet-stream",b"\x89PNG\r\n\x1a\ndata".to_vec())}else if path.ends_with("/images")||path.ends_with("/files") {assert!(header.to_ascii_lowercase().contains("multipart/form-data; boundary="));recorded.lock().unwrap().push((path.clone(),body));(200,None,"application/json",if path.ends_with("/images") {br#"{"code":0,"data":{"image_key":"uploaded-image"}}"#.to_vec()}else{br#"{"code":0,"data":{"file_key":"uploaded-file"}}"#.to_vec()})}else {recorded.lock().unwrap().push((path,body));(200,None,"application/json",br#"{"code":0,"data":{"message_id":"reply","reaction_id":"r"}}"#.to_vec())}
        };let redirect=location.map(|location|format!("Location: {location}\r\n")).unwrap_or_default();io.write_all(format!("HTTP/1.1 {status} TEST\r\n{redirect}Content-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",response.len()).as_bytes()).await.unwrap();io.write_all(&response).await.unwrap();
    });
            while work.try_join_next().is_some() {}
        }
    });
    let directory =
        std::env::temp_dir().join(format!("pdrs-feishu-media-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let directory = std::fs::canonicalize(directory).unwrap();
    let gateway = Arc::new(super::super::tests::Gateway::default());
    let owner = FeishuChannel::new(
        FeishuOptions {
            app_id: "app".into(),
            app_secret: "secret".into(),
            connection_mode: "webhook".into(),
            pilot_home: directory.clone(),
            api_base: format!("http://{address}/open-apis"),
            ..Default::default()
        },
        gateway.clone(),
    );
    let handle = owner.start().await.unwrap();
    let raw = json!({"header":{"event_id":"event-image","event_type":"im.message.receive_v1"},"event":{"message":{"chat_id":"chat","message_id":"m/id","message_type":"image","content":"{\"image_key\":\"image.key\"}"}}});
    assert_eq!(owner.webhook(&raw.to_string()).await.0, 200);
    let mut second = raw.clone();
    second["header"]["event_id"] = "event-image-2".into();
    second["event"]["message"]["message_id"] = "m/id2".into();
    assert_eq!(owner.webhook(&second.to_string()).await.0, 200);
    let mut redirected = raw.clone();
    redirected["header"]["event_id"] = "event-image-redirect".into();
    redirected["event"]["message"]["message_id"] = "m/id3".into();
    redirected["event"]["message"]["content"] = "{\"image_key\":\"redirect\"}".into();
    assert_eq!(owner.webhook(&redirected.to_string()).await.0, 200);
    sleep(Duration::from_millis(40)).await;
    assert!(gateway.turns.lock().unwrap().is_empty());
    // Text flushes the pending media batch immediately, while it stays a
    // separate following turn (TS groupFeishuInboundMessages).
    assert_eq!(
        owner
            .webhook(r#"{"chatId":"chat","text":"after media","eventId":"text-after"}"#)
            .await
            .0,
        200
    );
    super::super::tests::wait_turn(&gateway, 2).await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0]["attachments"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(gateway.turns.lock().unwrap()[1]["message"], "after media");
    let attachment = gateway.turns.lock().unwrap()[0]["attachments"][0].clone();
    assert_eq!(attachment["type"], "image");
    assert_eq!(attachment["mimeType"], "image/png");
    assert_eq!(attachment["name"], "image.png");
    let path = std::path::Path::new(attachment["path"].as_str().unwrap());
    assert!(path.starts_with(&directory));
    assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\ndata");
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(owner.webhook(&raw.to_string()).await.1["deduped"], true);
    assert!(
        send(
            &owner,
            "chat",
            &json!({"type":"image","name":"a.png","content":"iVBORw0KGgo=","mimeType":"image/png"})
        )
        .await
    );
    assert!(send(&owner, "chat", &json!({"name":"a.docx","content":"ZG9j"})).await);
    assert!(
        !send(
            &owner,
            "chat",
            &json!({"source":"local_path","path":"/not/read/secret"})
        )
        .await
    );
    assert!(!send(&owner, "chat", &json!({"name":"missing"})).await);
    let uploads = uploads.lock().unwrap();
    let image = uploads
        .iter()
        .find(|(path, _)| path.ends_with("/images"))
        .unwrap();
    assert!(String::from_utf8_lossy(&image.1).contains("name=\"image_type\"\r\n\r\nmessage"));
    let file = uploads
        .iter()
        .find(|(path, _)| path.ends_with("/files"))
        .unwrap();
    assert!(String::from_utf8_lossy(&file.1).contains("name=\"file_type\"\r\n\r\ndoc"));
    assert!(
        uploads
            .iter()
            .filter(|(path, _)| path.contains("receive_id_type"))
            .map(|(_, body)| String::from_utf8_lossy(body))
            .any(|body| body.contains("需要授权后才能发送"))
    );
    drop(uploads);
    let mut stopped = raw.clone();
    stopped["header"]["event_id"] = "cancelled".into();
    stopped["event"]["message"]["chat_id"] = "stop-chat".into();
    assert_eq!(owner.webhook(&stopped.to_string()).await.0, 200);
    handle.close().await;
    sleep(Duration::from_millis(270)).await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 2);
    server.abort();
}

#[test]
fn media_filters_size_type_filename_and_multipart_classification_follow_ts() {
    assert_eq!(file_type("a.docx", "application/octet-stream"), "doc");
    assert_eq!(file_type("a.XLSX", "x"), "xls");
    assert_eq!(file_type("a.pptx", "x"), "ppt");
    assert_eq!(file_type("a.bin", "application/pdf"), "pdf");
    assert_eq!(sanitize("a::b/\\c\u{0}.pdf"), "a_b_c_.pdf");
    assert_eq!(decode_base64("aGVs bG8=\n"), b"hello");
    assert!(
        MediaInput::from_raw(
            &json!({"event":{"message":{"message_type":"audio","chat_id":"c"}}}),
            true
        )
        .is_none()
    );
    let file = json!({"header":{"event_type":"im.message.receive_v1","event_id":"header"},"event":{"message":{"message_type":"file","chat_id":"c","message_id":"message","content":"{\"file_key\":\"f\",\"size\":999999999}"}}});
    assert_eq!(
        MediaInput::from_raw(&file, true).unwrap().event_id,
        "message"
    );
    assert_eq!(
        MediaInput::from_raw(&file, false).unwrap().event_id,
        "header"
    );
}
