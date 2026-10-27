use super::*;

// The target hostname's trailing dot prevents the unconditional localhost
// bypass from masking the very delimiter being tested.
#[tokio::test]
async fn no_proxy_js_whitespace_over_tcp() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("oracle.json")).expect("#860 parse Node oracle");
    let cases = oracle["noProxy"].as_array().expect("#860 cases");
    assert_eq!(cases.len(), 29, "#860 full whitespace and exclusions");
    for case in cases {
        let direct = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("#860 target bind");
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("#860 proxy bind");
        let target = format!(
            "http://localhost.:{}/",
            direct.local_addr().expect("#860 target address").port()
        );
        let config = ProxyConfig::new(
            &format!("http://{}", proxy.local_addr().expect("#860 proxy address")),
            case["value"].as_str().expect("#860 no_proxy string"),
        )
        .expect("#860 proxy parse");
        let transport = HyperModelTransport::from_config(webpki_client_config(), Some(config));
        let peer = tokio::spawn(async move {
            let (mut stream, tunnel) = tokio::select! {
                r = direct.accept() => (r.expect("#860 direct accept").0, false),
                r = proxy.accept() => (r.expect("#860 proxy accept").0, true),
            };
            async fn headers(stream: &mut tokio::net::TcpStream) {
                let mut bytes = Vec::new();
                while !bytes.ends_with(b"\r\n\r\n") {
                    bytes.push(stream.read_u8().await.expect("#860 request headers"));
                    assert!(bytes.len() < 16384, "#860 header bound");
                }
            }
            headers(&mut stream).await;
            if tunnel {
                stream
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .expect("#860 CONNECT response");
                headers(&mut stream).await;
            }
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await
                .expect("#860 HTTP response");
            tunnel
        });
        let exchange = async {
            let response = transport
                .send(
                    Request::get(&target)
                        .body(Full::new(Bytes::new()))
                        .expect("#860 request"),
                )
                .await
                .expect("#860 TCP exchange");
            assert_eq!(
                response
                    .into_body()
                    .collect()
                    .await
                    .expect("#860 body")
                    .to_bytes(),
                "ok",
                "#860 TCP reply"
            );
            assert_eq!(
                peer.await.expect("#860 peer completion"),
                case["proxy"].as_bool().expect("#860 expected route"),
                "#860 undici no_proxy route: U+{:04X}",
                case["cp"].as_u64().expect("#860 codepoint")
            );
        };
        tokio::time::timeout(Duration::from_secs(5), exchange)
            .await
            .expect("#860 TCP deadline");
    }
}

#[test]
fn zero_idle_option_uses_provider_or_default() {
    let oracle = parse_json(include_str!("oracle.json")).expect("#360 idle oracle");
    let JsValue::Array(cases) = field(&oracle, "cases") else {
        panic!("#360 idle cases");
    };
    let mut count = 0;
    for case in cases {
        if field(case, "streamTimeoutMs").as_number() == Some(0.0) {
            let options = StreamOptions {
                stream_timeout_ms: Some(0.0),
                ..Default::default()
            };
            assert_eq!(
                idle_timeout_ms(field(case, "provider"), &options),
                field(case, "resolvedIdle")
                    .as_number()
                    .expect("#360 resolved idle"),
                "#360 zero idle resolution"
            );
            count += 1;
        }
    }
    assert_eq!(count, 2, "#360 zero idle case coverage");
}
