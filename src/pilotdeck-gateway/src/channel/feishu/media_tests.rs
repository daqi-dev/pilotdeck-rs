//! Offline native media consumer comparison to fixed TS real fetch/FormData capture.
use super::*;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn gold() -> serde_json::Value {
    serde_json::from_str(include_str!("media-oracle.json")).unwrap()
}
#[tokio::test]
async fn feishu_resource_cache_and_multipart_delivery_match_fixed_ts() {
    let gold = gold();
    assert_eq!(gold["fixed"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85");
    let root = std::env::temp_dir().join(format!("pdrs-feishu-media-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let wire = Arc::new(Mutex::new(Vec::new()));
    let record = wire.clone();
    let png = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 0, 0, 0, 0, 0];
    let png_server = png.clone();
    let server = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {connection=listener.accept()=>{let(mut socket,_)=connection.unwrap();let wire=record.clone();let png=png_server.clone();children.spawn(async move{
            let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return;}raw.push(byte[0]);}
            let headers=String::from_utf8(raw).unwrap();let path=headers.lines().next().unwrap().split_whitespace().nth(1).unwrap().to_owned();let len=headers.lines().find_map(|s|s.to_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut bytes=vec![0;len];socket.read_exact(&mut bytes).await.unwrap();wire.lock().unwrap().push((path.clone(),headers,bytes));
            let(mime,data)=if path.contains("/resources/"){if path.ends_with("type=image"){("image/png",png)}else{("application/pdf",b"file bytes".to_vec())}}else{let value=if path.contains("tenant_access_token"){serde_json::json!({"code":0,"tenant_access_token":"mock-token","expire":7200})}else if path.ends_with("/images"){serde_json::json!({"code":0,"data":{"image_key":"uploaded-image"}})}else if path.ends_with("/files"){serde_json::json!({"code":0,"data":{"file_key":"uploaded-file"}})}else{serde_json::json!({"code":0,"data":{"message_id":"sent-message"}})};("application/json",value.to_string().into_bytes())};
            let response=format!("HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len());socket.write_all(response.as_bytes()).await.unwrap();socket.write_all(&data).await.unwrap();
            });},Some(_)=children.join_next(),if !children.is_empty()=>{}}
        }
    });
    let channel = FeishuChannel::new(
        FeishuOptions {
            app_id: "cli_0123456789abcdef".into(),
            app_secret: "mock-secret".into(),
            connection_mode: "webhook".into(),
            base_url: origin.clone(),
            ..Default::default()
        },
        Arc::new(super::super::im_interaction::NoInteraction),
    )
    .with_attachment_root(root.join("cache"));
    let handle = channel.start(ChannelStartDeps::default()).await.unwrap();
    for row in gold["inbound"].as_array().unwrap() {
        let input = &row["input"];
        let message = InboundMessage {
            chat_id: input["chatId"].as_str().unwrap().into(),
            text: String::new(),
            event_id: "fixture".into(),
            message_id: input["messageId"].as_str().map(str::to_owned),
            message_type: input["messageType"].as_str().map(str::to_owned),
            content: input["content"].as_str().map(str::to_owned),
        };
        let (items, diagnostics) = media::extract(&channel.runtime, &message).await;
        assert_eq!(serde_json::json!(diagnostics), row["result"]["diagnostics"]);
        assert_eq!(
            items.len(),
            row["result"]["attachments"].as_array().unwrap().len()
        );
        for (item, expected) in items
            .iter()
            .zip(row["result"]["attachments"].as_array().unwrap())
        {
            let path = item
                .get("path")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected["sha256"]);
            for key in ["type", "name", "mimeType", "bytes"] {
                let actual = item.get(key).unwrap();
                let actual: serde_json::Value = serde_json::from_str(
                    &pilotdeck_protocol::stringify_json(actual).unwrap().unwrap(),
                )
                .unwrap();
                assert_eq!(actual, expected[key]);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }
    std::fs::write(root.join("image.png"), &png).unwrap();
    for row in gold["outgoing"].as_array().unwrap() {
        let mut input = row["attachment"].clone();
        if input.get("path").is_some() {
            input["path"] = input["path"]
                .as_str()
                .unwrap()
                .replace("$OUT", &root.to_string_lossy())
                .into();
        }
        let input = pilotdeck_protocol::parse_json(&input.to_string()).unwrap();
        assert_eq!(
            media::send(channel.runtime.clone(), "room", &input).await,
            row["ok"].as_bool().unwrap()
        );
    }
    let wire = wire.lock().unwrap();
    let uploads = wire
        .iter()
        .filter(|(p, _, _)| p.ends_with("/files") || p.ends_with("/images"))
        .collect::<Vec<_>>();
    assert_eq!(uploads.len(), 2);
    for ((path, header, body), expected) in uploads.into_iter().zip(
        gold["wire"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r.get("parts").is_some()),
    ) {
        assert_eq!(path, expected["path"].as_str().unwrap());
        assert!(
            header
                .to_lowercase()
                .contains("authorization: bearer mock-token")
        );
        for part in expected["parts"].as_array().unwrap() {
            assert!(
                String::from_utf8_lossy(body)
                    .contains(&format!("name=\"{}\"", part["name"].as_str().unwrap()))
            );
            if let Some(value) = part["value"].as_str() {
                assert!(String::from_utf8_lossy(body).contains(value));
            }
            if let Some(file) = part["filename"].as_str() {
                assert!(String::from_utf8_lossy(body).contains(&format!("filename=\"{file}\"")));
            }
        }
    }
    drop(wire);
    handle.stop("fixture").await.unwrap();
    server.abort();
    let _ = server.await;
    std::fs::remove_dir_all(root).unwrap();
}
