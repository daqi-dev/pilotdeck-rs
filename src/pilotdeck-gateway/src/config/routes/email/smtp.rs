use super::wire::Wire;
use super::*;
use base64::Engine;
pub(super) struct Smtp {
    wire: Wire,
}
impl Smtp {
    pub async fn connect(options: &EmailOptions) -> Result<Self> {
        let mut client = Self {
            wire: Wire::connect(
                &options.smtp_host,
                options.smtp_port,
                options.smtp_port == 465,
            )
            .await?,
        };
        client.reply(&[220]).await?;
        let mut ehlo = client.command("EHLO localhost", &[250]).await?;
        if options.smtp_port != 465
            && (options.smtp_tls || ehlo.to_ascii_uppercase().contains("STARTTLS"))
        {
            if !ehlo.to_ascii_uppercase().contains("STARTTLS") {
                return Err(failure("SMTP requires STARTTLS"));
            }
            client.command("STARTTLS", &[220]).await?;
            client.wire.start_tls().await?;
            ehlo = client.command("EHLO localhost", &[250]).await?;
        }
        let plain = base64::engine::general_purpose::STANDARD
            .encode(format!("\0{}\0{}", options.address, options.password));
        if ehlo.to_ascii_uppercase().contains("PLAIN") {
            let reply = client
                .command(&format!("AUTH PLAIN {plain}"), &[235, 334])
                .await?;
            if reply.starts_with("334") {
                client.command(&plain, &[235]).await?;
            }
        } else {
            client.command("AUTH LOGIN", &[334]).await?;
            client
                .command(
                    &base64::engine::general_purpose::STANDARD.encode(options.address.as_bytes()),
                    &[334],
                )
                .await?;
            client
                .command(
                    &base64::engine::general_purpose::STANDARD.encode(options.password.as_bytes()),
                    &[235],
                )
                .await?;
        }
        Ok(client)
    }
    async fn command(&mut self, command: &str, allowed: &[u16]) -> Result<String> {
        if command.contains(['\r', '\n']) {
            return Err(failure("Invalid SMTP command"));
        }
        self.wire.write(format!("{command}\r\n").as_bytes()).await?;
        self.reply(allowed).await
    }
    async fn reply(&mut self, allowed: &[u16]) -> Result<String> {
        let mut reply = String::new();
        loop {
            let line = self.wire.line().await?;
            let text = String::from_utf8_lossy(&line);
            let code = text
                .get(..3)
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| failure("Invalid SMTP response"))?;
            let last = line.get(3) != Some(&b'-');
            reply.push_str(&text);
            if last {
                if !allowed.contains(&code) {
                    return Err(failure("SMTP command rejected"));
                }
                return Ok(reply);
            }
        }
    }
    pub async fn send(options: &EmailOptions, to: &str, text: &str) -> Result<()> {
        let mut client = Self::connect(options).await?;
        for value in [&options.address, to] {
            if value.contains(['\r', '\n', '<', '>']) {
                return Err(failure("Invalid email address"));
            }
        }
        client
            .command(&format!("MAIL FROM:<{}>", options.address), &[250])
            .await?;
        client
            .command(&format!("RCPT TO:<{to}>"), &[250, 251])
            .await?;
        client.command("DATA", &[354]).await?;
        let subject = if options.default_subject.is_ascii()
            && !options.default_subject.contains(['\r', '\n'])
        {
            options.default_subject.clone()
        } else {
            format!(
                "=?UTF-8?B?{}?=",
                base64::engine::general_purpose::STANDARD
                    .encode(options.default_subject.as_bytes())
            )
        };
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        let body = encoded
            .as_bytes()
            .chunks(76)
            .map(|part| std::str::from_utf8(part).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n");
        let message = format!(
            "From: {}\r\nTo: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{body}\r\n.\r\n",
            options.address
        );
        client.wire.write(message.as_bytes()).await?;
        client.reply(&[250]).await?;
        client.quit().await;
        Ok(())
    }
    pub async fn quit(&mut self) {
        let _ = self.command("QUIT", &[221]).await;
    }
}
