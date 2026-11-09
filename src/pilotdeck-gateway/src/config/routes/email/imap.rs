use super::wire::{Wire, quote};
use super::*;
pub(super) struct Imap {
    wire: Wire,
    next: u64,
}
pub(super) struct Message {
    pub uid: u64,
    pub from: String,
    pub source: String,
}
impl Imap {
    pub async fn connect(options: &EmailOptions) -> Result<Self> {
        let mut client = Self {
            wire: Wire::connect(&options.imap_host, options.imap_port, options.imap_tls).await?,
            next: 1,
        };
        let greeting = client.wire.line().await?;
        if !String::from_utf8_lossy(&greeting)
            .to_ascii_uppercase()
            .starts_with("* OK")
        {
            return Err(failure("IMAP rejected connection"));
        }
        let capabilities = client.command("CAPABILITY").await?;
        if !options.imap_tls
            && capabilities.iter().any(|line| {
                String::from_utf8_lossy(line)
                    .split_whitespace()
                    .any(|capability| capability.eq_ignore_ascii_case("STARTTLS"))
            })
        {
            client.command("STARTTLS").await?;
            client.wire.start_tls().await?;
        }
        client
            .command(&format!(
                "LOGIN {} {}",
                quote(&options.address)?,
                quote(&options.password)?
            ))
            .await?;
        client.command("SELECT INBOX").await?;
        Ok(client)
    }
    async fn command(&mut self, command: &str) -> Result<Vec<Vec<u8>>> {
        let tag = format!("A{:04}", self.next);
        self.next += 1;
        self.wire
            .write(format!("{tag} {command}\r\n").as_bytes())
            .await?;
        let mut responses = vec![];
        loop {
            let mut response = self.wire.line().await?;
            loop {
                let line = String::from_utf8_lossy(&response);
                let length = line
                    .trim_end()
                    .rsplit_once('{')
                    .and_then(|(_, tail)| tail.strip_suffix('}'))
                    .and_then(|length| {
                        length
                            .strip_suffix('+')
                            .unwrap_or(length)
                            .parse::<usize>()
                            .ok()
                    });
                let Some(length) = length else {
                    break;
                };
                response.extend_from_slice(&self.wire.exact(length).await?);
                response.extend_from_slice(&self.wire.line().await?);
            }
            if response.starts_with(format!("{tag} ").as_bytes()) {
                let status = String::from_utf8_lossy(&response);
                if !status[tag.len() + 1..]
                    .to_ascii_uppercase()
                    .starts_with("OK")
                {
                    return Err(failure("IMAP command rejected"));
                }
                return Ok(responses);
            }
            if response.starts_with(b"* BYE") {
                return Err(failure("IMAP server closed mailbox"));
            }
            responses.push(response);
        }
    }
    pub async fn prime(&mut self) -> Result<HashSet<u64>> {
        let responses = self.command("STATUS INBOX (UIDNEXT)").await?;
        let next = responses
            .iter()
            .find_map(|response| {
                let text = String::from_utf8_lossy(response);
                let re = regex::Regex::new(r"(?i)UIDNEXT\s+(\d+)").unwrap();
                re.captures(&text)
                    .and_then(|cap| cap[1].parse::<u64>().ok())
            })
            .unwrap_or(1);
        if next <= 1 {
            return Ok(HashSet::new());
        }
        // ImapFlow fetch(string) uses a sequence range unless uid:true is an
        // operation option. The baseline sets uid in the query, not options.
        let responses = self
            .command(&format!(
                "FETCH {}:{} (UID)",
                next.saturating_sub(300).max(1),
                next - 1
            ))
            .await?;
        Ok(responses
            .iter()
            .filter_map(|response| parse_fetch(response).map(|message| message.uid))
            .collect())
    }
    pub async fn unseen(&mut self) -> Result<Vec<Message>> {
        let responses = self.command("SEARCH UNSEEN").await?;
        let ids = responses
            .iter()
            .find_map(|response| {
                let text = String::from_utf8_lossy(response);
                text.strip_prefix("* SEARCH ").map(|ids| {
                    ids.split_whitespace()
                        .filter(|id| id.bytes().all(|byte| byte.is_ascii_digit()))
                        .collect::<Vec<_>>()
                        .join(",")
                })
            })
            .unwrap_or_default();
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let responses = self
            .command(&format!("FETCH {ids} (UID ENVELOPE BODY.PEEK[])"))
            .await?;
        Ok(responses
            .iter()
            .filter_map(|response| parse_fetch(response))
            .collect())
    }
    pub async fn logout(&mut self) {
        let _ = self.command("LOGOUT").await;
    }
}
#[derive(Debug)]
enum Token {
    Atom(String),
    List(Vec<Token>),
    Nil,
}
impl Token {
    fn text(&self) -> Option<&str> {
        if let Self::Atom(text) = self {
            Some(text)
        } else {
            None
        }
    }
    fn list(&self) -> Option<&[Token]> {
        if let Self::List(list) = self {
            Some(list)
        } else {
            None
        }
    }
}
fn token(bytes: &[u8], position: &mut usize) -> Option<Token> {
    while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
        *position += 1;
    }
    let first = *bytes.get(*position)?;
    *position += 1;
    if first == b'(' {
        let mut values = vec![];
        loop {
            while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
                *position += 1;
            }
            if bytes.get(*position) == Some(&b')') {
                *position += 1;
                return Some(Token::List(values));
            }
            values.push(token(bytes, position)?);
        }
    }
    if first == b'"' {
        let mut value = vec![];
        while let Some(&byte) = bytes.get(*position) {
            *position += 1;
            if byte == b'"' {
                return Some(Token::Atom(String::from_utf8_lossy(&value).into_owned()));
            }
            if byte == b'\\' {
                value.push(*bytes.get(*position)?);
                *position += 1;
            } else {
                value.push(byte);
            }
        }
        return None;
    }
    if first == b'{' {
        let start = *position;
        while bytes.get(*position).is_some_and(u8::is_ascii_digit) {
            *position += 1;
        }
        let length = std::str::from_utf8(&bytes[start..*position])
            .ok()?
            .parse::<usize>()
            .ok()?;
        if bytes.get(*position) == Some(&b'+') {
            *position += 1;
        }
        if bytes.get(*position) != Some(&b'}') {
            return None;
        }
        *position += 1;
        if bytes.get(*position) == Some(&b'\r') {
            *position += 1;
        }
        if bytes.get(*position) != Some(&b'\n') {
            return None;
        }
        *position += 1;
        let end = position.checked_add(length)?;
        let text = String::from_utf8_lossy(bytes.get(*position..end)?).into_owned();
        *position = end;
        return Some(Token::Atom(text));
    }
    let start = *position - 1;
    while bytes
        .get(*position)
        .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'(' && *byte != b')')
    {
        *position += 1;
    }
    let value = String::from_utf8_lossy(&bytes[start..*position]).into_owned();
    if value.eq_ignore_ascii_case("NIL") {
        Some(Token::Nil)
    } else {
        Some(Token::Atom(value))
    }
}
fn parse_fetch(bytes: &[u8]) -> Option<Message> {
    let mut position = 0;
    let mut tokens = vec![];
    while position < bytes.len() {
        while bytes.get(position).is_some_and(u8::is_ascii_whitespace) {
            position += 1;
        }
        if position == bytes.len() {
            break;
        }
        tokens.push(token(bytes, &mut position)?);
    }
    if !tokens.get(2)?.text()?.eq_ignore_ascii_case("FETCH") {
        return None;
    }
    let attrs = tokens.get(3)?.list()?;
    let mut uid = None;
    let mut from = "unknown".to_owned();
    let mut source = String::new();
    for pair in attrs.chunks_exact(2) {
        let key = pair[0].text()?.to_ascii_uppercase();
        match key.as_str() {
            "UID" => uid = pair[1].text().and_then(|id| id.parse().ok()),
            "ENVELOPE" => {
                if let Some(address) = pair[1]
                    .list()
                    .and_then(|envelope| envelope.get(2))
                    .and_then(Token::list)
                    .and_then(|from| from.first())
                    .and_then(Token::list)
                {
                    if let (Some(mailbox), Some(host)) = (
                        address.get(2).and_then(Token::text),
                        address.get(3).and_then(Token::text),
                    ) {
                        from = format!("{mailbox}@{host}");
                    }
                }
            }
            key if key.starts_with("BODY[") || key == "RFC822" => {
                source = pair[1].text().unwrap_or("").into()
            }
            _ => {}
        }
    }
    Some(Message {
        uid: uid?,
        from,
        source,
    })
}
#[cfg(test)]
pub(super) fn parse_test(bytes: &[u8]) -> Option<Message> {
    parse_fetch(bytes)
}
