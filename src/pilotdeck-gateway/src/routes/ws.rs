//! The public WS entry point never opens a TS upstream connection.
#[cfg(test)]
use tokio::io::{AsyncRead, AsyncWrite};

#[cfg(test)]
pub(crate) async fn run<S>(stream: S, token: String) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    crate::pairing::run_local_with_state(stream, token, crate::pairing::LocalState::default()).await
}

#[cfg(test)]
pub(crate) async fn run_with_gateway<S>(
    stream: S,
    token: String,
    gateway: Option<std::sync::Arc<dyn crate::runtime::connection::ConnectionGateway>>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match gateway {
        Some(gateway) => crate::pairing::run_local_with_gateway(stream, token, gateway).await,
        None => {
            crate::pairing::run_local_with_state(
                stream,
                token,
                crate::pairing::LocalState::default(),
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Decoder, client_frame};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::{Duration, timeout},
    };

    #[tokio::test]
    async fn rsv_text_and_ignored_continuation_reach_the_local_gateway() {
        timeout(Duration::from_secs(3), async {
            let (server, mut client) = tokio::io::duplex(4096);
            let job = tokio::spawn(run(server, "isolated-test-token".into()));
            let hello = br#"{"type":"hello","protocolVersion":"1.2","token":"isolated-test-token","clientName":"test","clientVersion":"1"}"#;
            client.write_all(&client_frame(0xc1, 126, hello)).await.unwrap();
            let mut decoder = Decoder::default();
            let frame = loop {
                if let Some(frame) = decoder.next_server().unwrap() { break frame; }
                assert_ne!(client.read_buf(&mut decoder.buffer).await.unwrap(), 0);
            };
            let reply: serde_json::Value = serde_json::from_slice(&frame.payload).unwrap();
            assert_eq!(reply["type"], "hello_ok");
            client.write_all(&client_frame(0x80, 3, b"bad")).await.unwrap();
            client.write_all(&client_frame(0x89, 2, b"ok")).await.unwrap();
            loop {
                if let Some(frame) = decoder.next_server().unwrap() {
                    assert_eq!(frame.opcode, 10);
                    assert_eq!(frame.payload, b"ok");
                    break;
                }
                assert_ne!(client.read_buf(&mut decoder.buffer).await.unwrap(), 0);
            }
            client.write_all(&client_frame(0x88, 0, b"")).await.unwrap();
            let mut tail = Vec::new();
            client.read_to_end(&mut tail).await.unwrap();
            assert_eq!(tail, crate::frame::server_frame(8, &1000u16.to_be_bytes()));
            job.await.unwrap().unwrap();
        }).await.unwrap();
    }

    #[tokio::test]
    async fn unmasked_input_closes_only_its_local_connection() {
        let (server, mut client) = tokio::io::duplex(64);
        let job = tokio::spawn(run(server, "isolated-test-token".into()));
        client.write_all(&[0x81, 0]).await.unwrap();
        let result = timeout(Duration::from_secs(3), job).await.unwrap().unwrap();
        assert!(result.is_err());
        let mut output = Vec::new();
        client.read_to_end(&mut output).await.unwrap();
        assert!(output.is_empty());
    }

    struct BoundGateway;
    impl crate::runtime::connection::ConnectionGateway for BoundGateway {
        fn describe_server(
            &self,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<pilotdeck_protocol::JsValue, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async {
                Ok(crate::metadata::object([(
                    "sessionCount",
                    pilotdeck_protocol::JsValue::Number(7.0),
                )]))
            })
        }
        fn request(
            &self,
            method: pilotdeck_protocol::JsString,
            params: pilotdeck_protocol::JsValue,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<pilotdeck_protocol::JsValue, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async move {
                assert!(method.equals_str("new_session"));
                Ok(params)
            })
        }
        fn submit<'a>(
            &'a self,
            _params: pilotdeck_protocol::JsValue,
            _emit: &'a (dyn Fn(pilotdeck_protocol::JsValue) + Send + Sync),
        ) -> crate::runtime::connection::Call<
            'a,
            Result<(), crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(()) })
        }
        fn disconnect(
            &self,
            _session: pilotdeck_protocol::JsValue,
            _binding: crate::runtime::connection::binding::WsBinding,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<bool, crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(false) })
        }
        fn abort_on_close(
            &self,
            _params: pilotdeck_protocol::JsValue,
        ) -> crate::runtime::connection::Call<
            '_,
            Result<(), crate::runtime::connection::RequestError>,
        > {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn bound_gateway_supplies_hello_and_requests_without_a_proxy() {
        timeout(Duration::from_secs(3), async {
            let (server, mut client) = tokio::io::duplex(8192);
            let job = tokio::spawn(run_with_gateway(server, "isolated".into(), Some(std::sync::Arc::new(BoundGateway))));
            let hello = br#"{"type":"hello","protocolVersion":"1.2","token":"isolated","clientName":"cli","clientVersion":"1"}"#;
            client.write_all(&client_frame(0x81, 126, hello)).await.unwrap();
            let mut decoder = Decoder::default();
            async fn receive(client: &mut tokio::io::DuplexStream, decoder: &mut Decoder) -> serde_json::Value {
                loop {
                    if let Some(frame) = decoder.next_server().unwrap() { return serde_json::from_slice(&frame.payload).unwrap(); }
                    assert_ne!(client.read_buf(&mut decoder.buffer).await.unwrap(), 0);
                }
            }
            let hello = receive(&mut client, &mut decoder).await;
            assert_eq!(hello["type"], "hello_ok");
            assert_eq!(hello["serverInfo"]["sessionCount"], 7);
            let request = br#"{"type":"request","id":"bound","method":"new_session","params":{"sessionKey":"created"}}"#;
            client.write_all(&client_frame(0x81, 126, request)).await.unwrap();
            let response = receive(&mut client, &mut decoder).await;
            assert_eq!(response["ok"], true);
            assert_eq!(response["result"]["sessionKey"], "created");
            client.shutdown().await.unwrap();
            job.await.unwrap().unwrap();
        }).await.unwrap();
    }
}
