use super::outbound;
use super::*;
use aes::{
    Aes128,
    cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray},
};
use base64::Engine;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn gold() -> Value {
    serde_json::from_str(include_str!("outbound-oracle.json")).unwrap()
}
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("weixin-outbound-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Mock {
    origin: String,
    rows: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock(name: &str) -> Mock {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let rows = Arc::new(Mutex::new(vec![]));
    let recorded = rows.clone();
    let base = origin.clone();
    let name = name.to_owned();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
            conn=listener.accept()=>{let(mut socket,_)=conn.unwrap();let(rows,base,name)=(recorded.clone(),base.clone(),name.clone());children.spawn(async move{
            let mut raw=vec![];while !raw.ends_with(b"\r\n\r\n"){let mut b=[0];if socket.read_exact(&mut b).await.is_err(){return}raw.push(b[0]);assert!(raw.len()<65536);}let header=String::from_utf8(raw).unwrap();let mut first=header.lines().next().unwrap().split_whitespace();let method=first.next().unwrap().to_owned();let path=first.next().unwrap().to_owned();
            let field=|name:&str|header.lines().find_map(|l|l.split_once(':').filter(|(k,_)|k.eq_ignore_ascii_case(name)).map(|(_,v)|v.trim().to_owned()));let len=field("content-length").map(|s|s.parse::<usize>().unwrap()).unwrap_or(0);let mut data=vec![0;len];socket.read_exact(&mut data).await.unwrap();
            let mut row=json!({"method":method,"path":path,"contentType":field("content-type"),"authorization":field("authorization")});
            if let Ok(body)=serde_json::from_slice::<Value>(&data){row["body"]=body;}else{row["bytesBase64"]=base64::engine::general_purpose::STANDARD.encode(&data).into();}
            rows.lock().unwrap().push(row);
            let(status,headers,body)=if path.ends_with("/getuploadurl"){
             let value=if name=="getupload-ret"{json!({"ret":7,"errmsg":"mock refused"})}else if name=="upload-param"{json!({"ret":0,"upload_param":"mock param /+"})}else{json!({"ret":0,"upload_full_url":format!("{base}/{}",if name=="redirect307"{"cdn-redirect"}else{"cdn-upload"})})};
             ("200 OK","Content-Type: application/json\r\n".to_owned(),value.to_string())
            }else if path=="/cdn-redirect"{("307 Temporary Redirect","Location: /cdn-upload\r\n".into(),String::new())
            }else if path.starts_with("/cdn-upload")||path.starts_with("/c2c/upload"){
             if name=="cdn-error"{("503 Service Unavailable","x-error-message: mock CDN denied\r\n".into(),"ignored body".into())}else{("200 OK",if name=="missing-header"{String::new()}else{"x-encrypted-param: download-param\r\n".into()},String::new())}
            }else{("200 OK","Content-Type: application/json\r\n".into(),json!({"ret":0}).to_string())};
            let response=format!("HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=socket.write_all(response.as_bytes()).await;
            });},Some(_)=children.join_next(),if !children.is_empty()=>{}
            }
        }
    });
    Mock { origin, rows, task }
}
fn key(hex: &str) -> [u8; 16] {
    assert_eq!(hex.len(), 32);
    std::array::from_fn(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
}
fn decrypt(mut bytes: Vec<u8>, key: &[u8; 16]) -> Vec<u8> {
    assert_eq!(bytes.len() % 16, 0);
    let aes = Aes128::new(GenericArray::from_slice(key));
    for block in bytes.chunks_mut(16) {
        aes.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let padding = *bytes.last().unwrap() as usize;
    assert!((1..=16).contains(&padding));
    assert!(
        bytes[bytes.len() - padding..]
            .iter()
            .all(|b| *b as usize == padding)
    );
    bytes.truncate(bytes.len() - padding);
    bytes
}
fn normalize(rows: &mut [Value]) {
    let upload = rows
        .iter()
        .find(|r| r["path"] == "/ilink/bot/getuploadurl")
        .map(|r| r["body"].clone());
    let aeskey = upload
        .as_ref()
        .and_then(|v| v["aeskey"].as_str())
        .unwrap_or("")
        .to_owned();
    let filekey = upload
        .as_ref()
        .and_then(|v| v["filekey"].as_str())
        .unwrap_or("")
        .to_owned();
    for row in rows {
        if let Some(encoded) = row["bytesBase64"].as_str() {
            let plain = decrypt(
                base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap(),
                &key(&aeskey),
            );
            row.as_object_mut().unwrap().remove("bytesBase64");
            row["plaintextBase64"] = base64::engine::general_purpose::STANDARD
                .encode(plain)
                .into();
        }
        if !filekey.is_empty()
            && let Some(path) = row["path"].as_str()
        {
            row["path"] = path.replace(&filekey, "$FILEKEY").into();
        }
        if let Some(body) = row.get_mut("body") {
            if body.get("aeskey").is_some() {
                let _ = key(body["aeskey"].as_str().unwrap());
                body["aeskey"] = "$AESKEY".into();
                assert_eq!(body["filekey"].as_str().unwrap().len(), 32);
                body["filekey"] = "$FILEKEY".into();
            }
            if let Some(msg) = body.get_mut("msg") {
                assert!(msg["client_id"].as_str().is_some_and(|s| !s.is_empty()));
                msg["client_id"] = "$CLIENT".into();
                for item in msg["item_list"].as_array_mut().unwrap() {
                    let owner = if item.get("image_item").is_some() {
                        item.get_mut("image_item")
                    } else {
                        item.get_mut("file_item")
                    };
                    if let Some(owner) = owner {
                        let media = owner.get_mut("media").unwrap();
                        let decoded = base64::engine::general_purpose::STANDARD
                            .decode(media["aes_key"].as_str().unwrap())
                            .unwrap();
                        assert_eq!(String::from_utf8(decoded).unwrap(), aeskey);
                        media["aes_key"] = "$ENCODEDKEY".into();
                    }
                }
            }
        }
    }
}
#[test]
fn native_aes_ecb_matches_real_fixed_ts_ciphertext_for_image_and_file() {
    let gold = gold();
    assert_eq!(gold["fixed"], "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85");
    for name in ["file", "image"] {
        let case = gold["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .unwrap();
        let plain = base64::engine::general_purpose::STANDARD
            .decode(case["input"]["content"].as_str().unwrap())
            .unwrap();
        let expected = base64::engine::general_purpose::STANDARD
            .decode(case["wire"][1]["bytesBase64"].as_str().unwrap())
            .unwrap();
        assert_eq!(outbound::encrypt(&plain, &[0x11; 16]), expected);
        assert_eq!(outbound::padded_size(plain.len()), expected.len());
    }
}
#[tokio::test]
async fn native_outbound_real_ilink_http_aes_cdn_and_fallbacks_match_fixed_ts() {
    let gold = gold();
    for case in gold["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let root = Root::new();
        let mock = mock(name).await;
        let logs = Arc::new(Mutex::new(vec![]));
        let sink = logs.clone();
        let mut options = WeixinChannelOptions::with_login_base_url(mock.origin.clone());
        options.credentials_path = root.0.join("credentials.json");
        options.credentials_dir = Some(root.0.clone());
        let channel = WeixinChannel::new(options);
        {
            let mut inner = channel.lock_inner();
            if name != "client-missing" {
                inner.client = Some(Arc::new(IlinkHttpClient::new(
                    mock.origin.clone(),
                    "mock-token".into(),
                )));
            }
            if name != "context-missing" {
                inner.context_tokens.insert("alice".into(), "ctx".into());
            }
        }
        *channel.0.logger.lock().unwrap() = Some(Arc::new(move |level, message| {
            sink.lock().unwrap().push(json!([level, message]))
        }));
        let input = parse_json(&case["input"].to_string()).unwrap();
        let result = channel
            .send_attachment_with_cdn(&"alice".into(), &input, &format!("{}/c2c", mock.origin))
            .await;
        let mut actual = mock.rows.lock().unwrap().clone();
        eprintln!(
            "Weixin native outbound case={name} result={result} rawwire={}",
            serde_json::to_string(&actual).unwrap()
        );
        if name == "redirect307" {
            assert_eq!(
                case["result"], false,
                "fixed Node detached ArrayBuffer failure is recorded, never called equivalent"
            );
            assert!(
                result,
                "native stable ciphertext replay is an explicit unapproved bug-fix difference"
            );
            let posts: Vec<_> = actual
                .iter()
                .filter(|r| r["path"] == "/cdn-redirect" || r["path"] == "/cdn-upload")
                .collect();
            assert_eq!(posts.len(), 2);
            assert!(posts.iter().all(|r| r["method"] == "POST"));
            assert_eq!(posts[0]["bytesBase64"], posts[1]["bytesBase64"]);
            assert_eq!(
                actual.last().unwrap()["body"]["msg"]["item_list"][0]["type"],
                4
            );
        } else {
            assert_eq!(result, case["result"].as_bool().unwrap(), "{name}");
            let mut expected = case["wire"].as_array().unwrap().to_vec();
            normalize(&mut actual);
            normalize(&mut expected);
            assert_eq!(actual, expected, "{name} real SDK/API/CDN wire contract");
            assert_eq!(
                json!(*logs.lock().unwrap()),
                case["logs"],
                "{name} exact preparation/fallback logger contract"
            );
        }
        let pending: Vec<Value> = channel
            .lock_inner()
            .pending_replies
            .iter()
            .map(|(user, texts)| {
                json!([
                    String::from_utf16_lossy(user.units()),
                    texts
                        .iter()
                        .map(|s| String::from_utf16_lossy(s.units()))
                        .collect::<Vec<_>>()
                ])
            })
            .collect();
        assert_eq!(
            json!(pending),
            case["pendingReplies"],
            "{name} fallback queue"
        );
    }
}
