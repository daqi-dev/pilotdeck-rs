use super::*;
use pilotdeck_protocol::parse_json;
use std::collections::VecDeque;
use tokio::sync::watch;

fn js(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn oracle() -> serde_json::Value {
    serde_json::from_str(include_str!("oracle.json")).unwrap()
}

#[test]
fn mapper_and_renderer_match_real_fixed_ts_capture() {
    let gold = oracle();
    let mut mapper = EmailSessionMapper::new(
        Default::default(),
        Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
    );
    for case in gold["mapper"].as_array().unwrap() {
        let text: JsString = case["text"].as_str().unwrap().into();
        let mapped = mapper.resolve(&"sender@example.test".into(), &text);
        let expected = &case["result"];
        assert_eq!(
            shown(&mapped.session_key),
            expected["sessionKey"].as_str().unwrap()
        );
        assert_eq!(
            shown(&mapped.message),
            expected["message"].as_str().unwrap()
        );
        assert_eq!(mapped.new_session, expected.get("command").is_some());
    }
    for case in gold["render"].as_array().unwrap() {
        let event = js(&case["event"].to_string());
        assert_eq!(
            render::render_email_event(&event).map(|s| shown(&s)),
            case["result"].as_str().map(str::to_owned)
        );
    }
}
struct MockGateway {
    calls: watch::Sender<Vec<TurnRequest>>,
    gate: watch::Sender<bool>,
}
struct Events {
    gate: watch::Receiver<bool>,
    events: VecDeque<JsValue>,
}
impl TurnEventStream for Events {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            self.gate
                .wait_for(|open| *open)
                .await
                .map_err(|_| ChannelError::error("mock gate closed"))?;
            Ok(self.events.pop_front())
        })
    }
}
impl TurnGateway for MockGateway {
    fn submit(
        &self,
        request: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.calls.send_modify(|rows| rows.push(request));
        let gate = self.gate.subscribe();
        Box::pin(async move {
            Ok(Box::new(Events {
                gate,
                events: VecDeque::from([js(r#"{"type":"assistant_text_delta","text":" REPLY "}"#)]),
            }) as Box<dyn TurnEventStream>)
        })
    }
}
#[test]
fn plain_body_and_fixed_js_quoted_printable_semantics() {
    assert_eq!(shown(&extract_plain_text("  hello  ")), "hello");
    assert_eq!(
        shown(&extract_plain_text(
            "Content-Type: text/plain\r\nContent-Transfer-Encoding: quoted-printable\r\n=41=42=3d\r\n"
        )),
        "AB=3d"
    );
}

#[test]
fn plain_extraction_matches_actual_ts() {
    for c in oracle()["plain"].as_array().unwrap() {
        assert_eq!(
            shown(&extract_plain_text(c["raw"].as_str().unwrap())),
            c["result"].as_str().unwrap()
        );
    }
}
#[derive(Clone)]
struct Row {
    uid: u32,
    from: String,
    raw: String,
}
struct Mock {
    imap: u16,
    smtp: u16,
    messages: Arc<Mutex<Vec<Row>>>,
    mail: watch::Sender<Vec<serde_json::Value>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
async fn until(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap();
}
async fn imap_connection(socket: tokio::net::TcpStream, messages: Arc<Mutex<Vec<Row>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = socket.into_split();
    let mut read = BufReader::new(r);
    w.write_all(b"* OK owned mock\r\n").await.unwrap();
    let mut line = String::new();
    loop {
        line.clear();
        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let parts: Vec<_> = line.trim_end().split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let tag = parts[0];
        let cmd = parts[1].to_ascii_uppercase();
        let rows = lock(&messages).clone();
        let mut response = match cmd.as_str() {
            "LOGIN" => format!("{tag} OK login\r\n"),
            "CAPABILITY" => format!("* CAPABILITY IMAP4rev1\r\n{tag} OK cap\r\n"),
            "SELECT" => format!(
                "* {} EXISTS\r\n* OK [UIDVALIDITY 1] valid\r\n* OK [UIDNEXT {}] next\r\n{tag} OK [READ-WRITE] selected\r\n",
                rows.len(),
                rows.last().map(|r| r.uid + 1).unwrap_or(1)
            ),
            "STATUS" => format!(
                "* STATUS INBOX (UIDNEXT {})\r\n{tag} OK status\r\n",
                rows.last().map(|r| r.uid + 1).unwrap_or(1)
            ),
            "SEARCH" => format!(
                "* SEARCH {}\r\n{tag} OK search\r\n",
                (1..=rows.len())
                    .map(|n| n.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            "LOGOUT" => format!("* BYE logout\r\n{tag} OK logout\r\n"),
            "FETCH" => String::new(),
            _ => format!("{tag} BAD unsupported\r\n"),
        };
        if cmd == "FETCH" {
            let range = parts[2];
            let ids: Vec<usize> = if let Some((a, b)) = range.split_once(':') {
                (a.parse::<usize>().unwrap()..=b.parse::<usize>().unwrap()).collect()
            } else {
                range.split(',').map(|s| s.parse().unwrap()).collect()
            };
            for id in ids {
                let Some(row) = rows.get(id - 1) else {
                    continue;
                };
                if !line.contains("BODY") {
                    response.push_str(&format!("* {id} FETCH (UID {})\r\n", row.uid));
                    continue;
                }
                let (user, host) = row.from.split_once('@').unwrap();
                response.push_str(&format!("* {id} FETCH (UID {} ENVELOPE (NIL \"Message\" ((NIL NIL \"{user}\" \"{host}\")) NIL NIL NIL NIL NIL NIL NIL) BODY[] {{{}}}\r\n{})\r\n",row.uid,row.raw.len(),row.raw));
            }
            response.push_str(&format!("{tag} OK fetch\r\n"));
        }
        if w.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        if cmd == "LOGOUT" {
            return;
        }
    }
}
fn decode_mail(raw: &str) -> (String, String) {
    use base64::Engine;
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
    let subject = head
        .lines()
        .find_map(|l| l.strip_prefix("Subject: "))
        .unwrap_or("")
        .to_owned();
    let body = body.strip_suffix("\r\n").unwrap_or(body);
    let text = if head
        .to_ascii_lowercase()
        .contains("content-transfer-encoding: base64")
    {
        String::from_utf8(
            base64::engine::general_purpose::STANDARD
                .decode(body.replace(['\r', '\n'], ""))
                .unwrap(),
        )
        .unwrap()
    } else if head
        .to_ascii_lowercase()
        .contains("content-transfer-encoding: quoted-printable")
    {
        let folded = body.replace("=\r\n", "");
        let b = folded.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'=' && i + 2 < b.len() {
                if let Ok(n) = u8::from_str_radix(&folded[i + 1..i + 3], 16) {
                    out.push(n);
                    i += 3;
                    continue;
                }
            }
            out.push(b[i]);
            i += 1;
        }
        String::from_utf8(out).unwrap()
    } else {
        body.to_owned()
    };
    (subject, text.replace("\r\n", "\n"))
}
async fn smtp_connection(
    socket: tokio::net::TcpStream,
    mail: watch::Sender<Vec<serde_json::Value>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = socket.into_split();
    let mut read = BufReader::new(r);
    w.write_all(b"220 owned mock\r\n").await.unwrap();
    let mut line = String::new();
    let (mut from, mut to) = (String::new(), String::new());
    loop {
        line.clear();
        if read.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let cmd = line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let response = match cmd.as_str() {
            "EHLO" => "250-owned\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n",
            "AUTH" => "235 authenticated\r\n",
            "MAIL" => {
                from = line
                    .split('<')
                    .nth(1)
                    .unwrap()
                    .split('>')
                    .next()
                    .unwrap()
                    .into();
                "250 sender\r\n"
            }
            "RCPT" => {
                to = line
                    .split('<')
                    .nth(1)
                    .unwrap()
                    .split('>')
                    .next()
                    .unwrap()
                    .into();
                "250 recipient\r\n"
            }
            "DATA" => {
                w.write_all(b"354 data\r\n").await.unwrap();
                let mut data = String::new();
                loop {
                    line.clear();
                    if read.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line == ".\r\n" {
                        break;
                    }
                    data.push_str(if line.starts_with("..") {
                        &line[1..]
                    } else {
                        &line
                    });
                }
                let (subject, text) = decode_mail(&data);
                mail.send_modify(|v| {
                    v.push(serde_json::json!({"from":from,"to":to,"subject":subject,"text":text}))
                });
                if text == "FAIL" {
                    "550 owned fail\r\n"
                } else {
                    "250 queued\r\n"
                }
            }
            "QUIT" => "221 bye\r\n",
            _ => "250 ok\r\n",
        };
        if w.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        if cmd == "QUIT" {
            return;
        }
    }
}
impl Mock {
    async fn start() -> Self {
        let imap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let smtp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (imap_port, smtp_port) = (
            imap.local_addr().unwrap().port(),
            smtp.local_addr().unwrap().port(),
        );
        let messages = Arc::new(Mutex::new(vec![Row {
            uid: 1,
            from: "old@example.test".into(),
            raw: "old".into(),
        }]));
        let (mail, _) = watch::channel(Vec::new());
        let rows = messages.clone();
        let mails = mail.clone();
        let imap_task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {r=imap.accept()=>{let(s,_)=r.unwrap();children.spawn(imap_connection(s,rows.clone()));},_=children.join_next(),if !children.is_empty()=>{}}
            }
        });
        let smtp_task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {r=smtp.accept()=>{let(s,_)=r.unwrap();children.spawn(smtp_connection(s,mails.clone()));},_=children.join_next(),if !children.is_empty()=>{}}
            }
        });
        Self {
            imap: imap_port,
            smtp: smtp_port,
            messages,
            mail,
            tasks: vec![imap_task, smtp_task],
        }
    }
    fn inject(&self, uid: u32, text: &str) {
        lock(&self.messages).push(Row {
            uid,
            from: "sender@example.test".into(),
            raw: text.into(),
        });
    }
    fn options(&self) -> EmailOptions {
        let mut extra = JsObject::new();
        for (k, v) in [
            ("address", "bot@example.test"),
            ("password", "owned"),
            ("imapHost", "127.0.0.1"),
            ("smtpHost", "127.0.0.1"),
            ("defaultSubject", "Message"),
        ] {
            extra.insert(k.into(), v.into());
        }
        extra.insert("imapPort".into(), JsValue::Number(self.imap as f64));
        extra.insert("smtpPort".into(), JsValue::Number(self.smtp as f64));
        extra.insert("imapTls".into(), JsValue::Bool(false));
        extra.insert("smtpTls".into(), JsValue::Bool(false));
        extra.insert("pollIntervalMs".into(), JsValue::Number(3600000.0));
        EmailOptions {
            extra,
            mapper: EmailSessionMapper::new(
                Default::default(),
                Arc::new(|| "00000000-0000-0000-0000-000000000001".into()),
            ),
            env: Some(Arc::new(|_| None)),
        }
    }
}
#[tokio::test]
async fn actual_imap_smtp_loop_matches_fixed_ts_and_does_not_cancel_inflight() {
    let mock = Mock::start().await;
    let (calls, _) = watch::channel(Vec::<TurnRequest>::new());
    let (gate, _) = watch::channel(false);
    let channel = EmailChannel::new(
        mock.options(),
        Arc::new(super::super::im_interaction::NoInteraction),
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(MockGateway {
                calls: calls.clone(),
                gate: gate.clone(),
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(calls.borrow().is_empty());
    mock.inject(2, "/new");
    channel.shared.poll().await;
    until(|| mock.mail.borrow().len() == 1).await;
    channel.drain().await;
    mock.inject(3, "hello");
    channel.shared.poll().await;
    until(|| calls.borrow().len() == 1).await;
    channel.shared.poll().await;
    assert_eq!(calls.borrow().len(), 1);
    gate.send_replace(true);
    channel.drain().await;
    until(|| mock.mail.borrow().len() == 2).await;
    let cron = CronResultDelivery {
        task_id: "t".into(),
        run_id: "r".into(),
        session_key: "email:chat=sender@example.test:general".into(),
        channel_key: ChannelKey::new("email"),
        origin_session_key: None,
        origin_channel_key: None,
        text: "CRON".into(),
    };
    assert!(channel.deliver_cron_result(&cron).await);
    assert!(
        !channel
            .deliver_cron_result(&CronResultDelivery {
                text: "FAIL".into(),
                ..cron.clone()
            })
            .await
    );
    gate.send_replace(false);
    mock.inject(4, "inflight");
    channel.shared.poll().await;
    until(|| calls.borrow().len() == 2).await;
    handle.stop("owned-stop").await.unwrap();
    assert_eq!(channel.active_tasks(), 1);
    gate.send_replace(true);
    channel.drain().await;
    assert!(!channel.deliver_cron_result(&cron).await);
    let gold = oracle();
    assert_eq!(serde_json::json!(*mock.mail.borrow()), gold["mail"]);
    let turns:Vec<_>=calls.borrow().iter().map(|r|serde_json::json!({"sessionKey":shown(&r.session_key),"channelKey":"email","message":shown(&r.message)})).collect();
    assert_eq!(serde_json::json!(turns), gold["calls"]);
    let restarted = channel.start(ChannelStartDeps::default()).await.unwrap();
    assert_eq!(calls.borrow().len(), 2);
    restarted.stop("restart").await.unwrap();
}

#[test]
fn loader_options_unwrap_extra_without_treating_the_wrapper_as_mail_settings() {
    let options = js(
        r#"{"extra":{"address":"bot@example.test","imap":{"host":"127.0.0.1"}},"address":"wrong@example.test"}"#,
    );
    let parsed = EmailOptions::from_object(options.as_object().unwrap());
    assert_eq!(
        parsed.extra.get("address"),
        Some(&JsValue::from("bot@example.test"))
    );
    assert!(parsed.extra.get("extra").is_none());
    assert!(EmailOptions::from_object(&JsObject::new()).extra.is_empty());
}
