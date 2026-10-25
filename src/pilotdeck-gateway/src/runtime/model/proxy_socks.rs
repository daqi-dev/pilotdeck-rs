//! SOCKS5 CONNECT (RFC 1928/1929), shared by MCP, model and hook transport.
use super::http::ProxyIo;
use std::io::{Error, ErrorKind, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

pub(super) async fn connect(
    stream: &mut Box<dyn ProxyIo>,
    proxy: &url::Url,
    target: &hyper::Uri,
) -> Result<()> {
    // ProxyAgent passes URL userinfo as options to Socks5ProxyAgent, so
    // percent escapes remain encoded on the SOCKS wire (undici 8.3).
    let user = proxy.username().as_bytes();
    let password = proxy.password().unwrap_or_default().as_bytes();
    let auth = !user.is_empty() && !password.is_empty();
    stream
        .write_all(if auth { &[5, 2, 2, 0] } else { &[5, 1, 0] })
        .await?;
    let mut reply = [0; 2];
    stream.read_exact(&mut reply).await?;
    if reply[0] != 5 {
        return Err(invalid("Invalid SOCKS version"));
    }
    match reply[1] {
        0 => {}
        2 if auth => {
            let ulen =
                u8::try_from(user.len()).map_err(|_| invalid("Username or password too long"))?;
            let plen = u8::try_from(password.len())
                .map_err(|_| invalid("Username or password too long"))?;
            let mut request = vec![1, ulen];
            request.extend_from_slice(user);
            request.push(plen);
            request.extend_from_slice(password);
            stream.write_all(&request).await?;
            stream.read_exact(&mut reply).await?;
            if reply != [1, 0] {
                return Err(invalid("SOCKS5 authentication failed"));
            }
        }
        _ => return Err(invalid("SOCKS5 authentication method rejected")),
    }
    let host = target
        .host()
        .ok_or_else(|| invalid("Missing SOCKS5 target"))?;
    let port = target.port_u16().filter(|port| *port != 0).unwrap_or(
        if target.scheme_str() == Some("https") {
            443
        } else {
            80
        },
    );
    let mut request = vec![5, 1, 0];
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            request.push(1);
            request.extend_from_slice(&ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            request.push(4);
            request.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            request.extend_from_slice(&[
                3,
                u8::try_from(host.len()).map_err(|_| invalid("SOCKS5 hostname too long"))?,
            ]);
            request.extend_from_slice(host.as_bytes());
        }
    }
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).await?;
    let mut header = [0; 4];
    stream.read_exact(&mut header).await?;
    if header[0..3] != [5, 0, 0] {
        return Err(invalid("SOCKS5 connection rejected"));
    }
    let count = match header[3] {
        1 => 4,
        4 => 16,
        3 => usize::from(stream.read_u8().await?),
        _ => return Err(invalid("Invalid SOCKS5 address type")),
    };
    // Read only the bound address/port; application bytes remain on stream.
    stream.read_exact(&mut vec![0; count + 2]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn exchange(reply: &[u8], proxy: &str, target: &str) -> (Result<()>, Vec<u8>) {
        let (client, mut server) = tokio::io::duplex(2048);
        server.write_all(reply).await.unwrap();
        server.shutdown().await.unwrap();
        let mut client: Box<dyn ProxyIo> = Box::new(client);
        let result = connect(
            &mut client,
            &url::Url::parse(proxy).unwrap(),
            &target.parse().unwrap(),
        )
        .await;
        drop(client);
        let mut wire = Vec::new();
        server.read_to_end(&mut wire).await.unwrap();
        (result, wire)
    }
    #[tokio::test]
    async fn addresses_ports_and_reply_framing() {
        for (target, encoded, port) in [
            ("http://127.0.0.1:123/", vec![1, 127, 0, 0, 1], 123u16),
            (
                "http://mcp.test:0/",
                [vec![3, 8], b"mcp.test".to_vec()].concat(),
                80,
            ),
            // URL.hostname in TS retains IPv6 brackets, so undici sends DOMAIN.
            (
                "https://[::1]:0/",
                [vec![3, 5], b"[::1]".to_vec()].concat(),
                443,
            ),
        ] {
            for address in [vec![1, 127, 0, 0, 1], vec![3, 1, b'x'], vec![4; 17]] {
                let reply = [vec![5, 0, 5, 0, 0], address, vec![0, 0]].concat();
                let (result, wire) = exchange(&reply, "socks://127.0.0.1", target).await;
                result.unwrap();
                assert_eq!(
                    wire,
                    [
                        vec![5, 1, 0, 5, 1, 0],
                        encoded.clone(),
                        port.to_be_bytes().to_vec()
                    ]
                    .concat()
                );
            }
        }
    }
    #[tokio::test]
    async fn rejects_malformed_handshake_auth_and_connect_replies() {
        for (reply, expected) in [
            (vec![4, 0], "Invalid SOCKS version"),
            (vec![5, 255], "SOCKS5 authentication method rejected"),
            (vec![5, 3], "SOCKS5 authentication method rejected"),
            (vec![5, 2], "SOCKS5 authentication method rejected"),
            (vec![5, 0, 5, 5, 0, 1], "SOCKS5 connection rejected"),
            (vec![5, 0, 4, 0, 0, 1], "SOCKS5 connection rejected"),
            (vec![5, 0, 5, 0, 1, 1], "SOCKS5 connection rejected"),
            (vec![5, 0, 5, 0, 0, 7], "Invalid SOCKS5 address type"),
        ] {
            let error = exchange(&reply, "socks5://127.0.0.1", "http://mcp.test/")
                .await
                .0
                .unwrap_err();
            assert_eq!(error.to_string(), expected, "{reply:?}");
        }
        assert_eq!(
            exchange(
                &[5, 0, 5, 0, 0, 1],
                "socks5://127.0.0.1",
                "http://mcp.test/"
            )
            .await
            .0
            .unwrap_err()
            .kind(),
            ErrorKind::UnexpectedEof
        );
        for reply in [vec![5, 2, 1, 1], vec![5, 2, 2, 0]] {
            assert_eq!(
                exchange(&reply, "socks5://user:pass@127.0.0.1", "http://mcp.test/")
                    .await
                    .0
                    .unwrap_err()
                    .to_string(),
                "SOCKS5 authentication failed"
            );
        }
        for (user, pass) in [
            ("x".repeat(256), "pass".into()),
            ("user".into(), "x".repeat(256)),
        ] {
            let proxy = format!("socks5://{user}:{pass}@127.0.0.1");
            assert_eq!(
                exchange(&[5, 2], &proxy, "http://mcp.test/")
                    .await
                    .0
                    .unwrap_err()
                    .to_string(),
                "Username or password too long"
            );
        }
    }
}
