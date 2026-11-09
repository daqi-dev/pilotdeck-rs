use super::super::home_assistant::tests::Gateway;
use super::*;
use base64::Engine;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    sync::Notify,
};
struct Mailbox {
    imap_port: u16,
    smtp_port: u16,
    mail: Arc<StdMutex<Vec<(u64, String)>>>,
    sent: Arc<StdMutex<Vec<String>>>,
    commands: Arc<StdMutex<Vec<String>>>,
    changed: Arc<Notify>,
    tasks: Vec<JoinHandle<()>>,
}
impl Drop for Mailbox {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}
impl Mailbox {
    fn add(&self, uid: u64, text: &str) {
        self.mail.lock().unwrap().push((uid, text.into()));
    }
    async fn wait(&self, predicate: impl Fn(&[String]) -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if predicate(&self.sent.lock().unwrap()) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
}
async fn mailbox() -> Mailbox {
    let imap = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let imap_port = imap.local_addr().unwrap().port();
    let smtp = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let smtp_port = smtp.local_addr().unwrap().port();
    let mail: Arc<StdMutex<Vec<(u64, String)>>> =
        Arc::new(StdMutex::new(vec![(1, "old ignored".into())]));
    let messages = mail.clone();
    let sent = Arc::new(StdMutex::new(vec![]));
    let recorded = sent.clone();
    let commands = Arc::new(StdMutex::new(vec![]));
    let imap_commands = commands.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let imap_task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (stream, _) = imap.accept().await.unwrap();
            let messages = messages.clone();
            let commands = imap_commands.clone();
            connections.spawn(async move{let mut stream=BufReader::new(stream);stream.get_mut().write_all(b"* OK fixture ready\r\n").await.unwrap();loop{let mut line=String::new();if stream.read_line(&mut line).await.unwrap_or(0)==0{return;}let(tag,command)=line.trim_end().split_once(' ').unwrap();commands.lock().unwrap().push(command.into());let response=if command=="CAPABILITY"{format!("* CAPABILITY IMAP4rev1 AUTH=PLAIN\r\n{tag} OK capability\r\n")}else if command.starts_with("LOGIN "){assert_eq!(command,"LOGIN \"bot@example.test\" \"password\"");format!("{tag} OK logged in\r\n")}else if command=="SELECT INBOX"{format!("* 1 EXISTS\r\n{tag} OK selected\r\n")}else if command=="STATUS INBOX (UIDNEXT)"{format!("* STATUS INBOX (UIDNEXT 2)\r\n{tag} OK status\r\n")}else if command=="FETCH 1:1 (UID)"{format!("* 1 FETCH (UID 1)\r\n{tag} OK primed\r\n")}else if command=="SEARCH UNSEEN"{let length=messages.lock().unwrap().len();format!("* SEARCH {}\r\n{tag} OK search\r\n",(1..=length).map(|i|i.to_string()).collect::<Vec<_>>().join(" "))}else if command.starts_with("FETCH "){assert!(command.ends_with("(UID ENVELOPE BODY.PEEK[])"));let messages=messages.lock().unwrap().clone();let mut response=String::new();for(index,(uid,text))in messages.iter().enumerate(){response.push_str(&format!("* {} FETCH (UID {uid} ENVELOPE (NIL NIL ((NIL NIL \"sender\" \"example.test\")) NIL NIL NIL NIL NIL NIL NIL) BODY[] {{{}}}\r\n{text})\r\n",index+1,text.len()));}response.push_str(&format!("{tag} OK fetched\r\n"));response}else if command=="LOGOUT"{let _=stream.get_mut().write_all(format!("* BYE logout\r\n{tag} OK bye\r\n").as_bytes()).await;return;}else{panic!("unexpected IMAP command {command}")};if stream.get_mut().write_all(response.as_bytes()).await.is_err(){return;}}});
            while connections.try_join_next().is_some() {}
        }
    });
    let smtp_task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (stream, _) = smtp.accept().await.unwrap();
            let recorded = recorded.clone();
            let notify = notify.clone();
            connections.spawn(async move {
                let mut stream = BufReader::new(stream);
                stream
                    .get_mut()
                    .write_all(b"220 SMTP fixture\r\n")
                    .await
                    .unwrap();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let command = line.trim_end();
                    let reply = if command == "EHLO localhost" {
                        "250-fixture\r\n250 AUTH PLAIN LOGIN\r\n"
                    } else if let Some(auth) = command.strip_prefix("AUTH PLAIN ") {
                        assert_eq!(
                            base64::engine::general_purpose::STANDARD
                                .decode(auth)
                                .unwrap(),
                            b"\0bot@example.test\0password"
                        );
                        "235 authenticated\r\n"
                    } else if command == "MAIL FROM:<bot@example.test>"
                        || command == "RCPT TO:<sender@example.test>"
                    {
                        "250 accepted\r\n"
                    } else if command == "DATA" {
                        stream
                            .get_mut()
                            .write_all(b"354 send data\r\n")
                            .await
                            .unwrap();
                        let mut message = String::new();
                        loop {
                            let mut line = String::new();
                            stream.read_line(&mut line).await.unwrap();
                            if line == ".\r\n" {
                                break;
                            }
                            message.push_str(&line);
                        }
                        let body = message
                            .split_once("\r\n\r\n")
                            .unwrap()
                            .1
                            .replace(['\r', '\n'], "");
                        let text = String::from_utf8(
                            base64::engine::general_purpose::STANDARD
                                .decode(body)
                                .unwrap(),
                        )
                        .unwrap();
                        assert!(message.contains("Subject: Message\r\n"));
                        recorded.lock().unwrap().push(text);
                        notify.notify_waiters();
                        "250 queued\r\n"
                    } else if command == "QUIT" {
                        let _ = stream.get_mut().write_all(b"221 bye\r\n").await;
                        return;
                    } else {
                        panic!("unexpected SMTP command {command}")
                    };
                    if stream.get_mut().write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Mailbox {
        imap_port,
        smtp_port,
        mail,
        sent,
        commands,
        changed,
        tasks: vec![imap_task, smtp_task],
    }
}
fn options(mailbox: &Mailbox) -> EmailOptions {
    EmailOptions {
        address: "bot@example.test".into(),
        password: "password".into(),
        imap_host: "127.0.0.1".into(),
        smtp_host: "127.0.0.1".into(),
        imap_port: mailbox.imap_port,
        smtp_port: mailbox.smtp_port,
        poll_interval_ms: 20,
        imap_tls: false,
        smtp_tls: false,
        ..Default::default()
    }
}
#[tokio::test]
async fn real_imap_uid_priming_poll_literal_envelope_smtp_reply_and_cron() {
    let mailbox = mailbox().await;
    let gateway = Arc::new(Gateway::default());
    let channel = EmailChannel::new(options(&mailbox), gateway.clone());
    let handle = channel.start().await.unwrap();
    assert!(gateway.turns.lock().unwrap().is_empty());
    mailbox.add(2, "你好");
    mailbox
        .wait(|sent| sent.iter().any(|text| text == "reply 你好"))
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"email:chat=sender@example.test:general","channelKey":"email","message":"你好"})
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(
        channel
            .deliver(&json!({"sessionKey":"email:chat=sender@example.test:general","text":"cron"}))
            .await
            .unwrap()
    );
    assert!(
        mailbox
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| command == "FETCH 1:1 (UID)")
    );
    handle.close().await;
    assert!(
        !channel
            .deliver(
                &json!({"sessionKey":"email:chat=sender@example.test:general","text":"stopped"})
            )
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn polled_permission_and_question_answers_resume_same_turn() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let mailbox = mailbox().await;
        let gateway = Arc::new(Gateway::default());
        let channel = EmailChannel::new(options(&mailbox), gateway.clone());
        let handle = channel.start().await.unwrap();
        mailbox.add(2, input);
        gateway.ready.notified().await;
        mailbox.add(3, answer);
        mailbox
            .wait(|sent| sent.iter().any(|text| text == &format!("reply {input}")))
            .await;
        assert_eq!(gateway.requests.lock().unwrap()[0].0, method);
        handle.close().await;
    }
}
#[tokio::test]
async fn new_busy_and_stop_cancel_polled_stream() {
    let mailbox = mailbox().await;
    let gateway = Arc::new(Gateway::default());
    let channel = EmailChannel::new(options(&mailbox), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel
        .incoming("sender@example.test", "/new")
        .await
        .unwrap();
    mailbox.add(2, "hold");
    gateway.ready.notified().await;
    channel
        .incoming("sender@example.test", "/new ignored")
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(
        gateway.turns.lock().unwrap()[0]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("email:chat=sender@example.test:s_")
    );
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(gateway.cancelled.load(Ordering::SeqCst));
}
#[test]
fn mime_regex_quoted_printable_and_literal_envelope_match_ts() {
    assert_eq!(extract_plain_text(" no headers "), "no headers");
    assert_eq!(
        extract_plain_text(
            "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nfirst=20second=\r\nline=2e\r\n--abcdef123456"
        ),
        "first secondline=2e"
    );
    let raw=b"* 8 FETCH (UID 42 BODY[] {5}\r\nhello ENVELOPE (NIL NIL ((\"Name\" NIL \"alice\" \"example.test\")) NIL NIL NIL NIL NIL NIL NIL))\r\n";
    let message = imap::parse_test(raw).unwrap();
    assert_eq!(message.uid, 42);
    assert_eq!(message.from, "alice@example.test");
    assert_eq!(message.source, "hello");
}
#[tokio::test]
async fn missing_configuration_and_required_starttls_fail() {
    assert!(
        EmailChannel::new(EmailOptions::default(), Arc::new(Gateway::default()))
            .start()
            .await
            .is_err()
    );
    let mailbox = mailbox().await;
    let mut options = options(&mailbox);
    options.smtp_tls = true;
    assert!(
        EmailChannel::new(options, Arc::new(Gateway::default()))
            .start()
            .await
            .is_err()
    );
}
