use super::*;
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf,
};
trait AsyncIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncIo for T {}
struct Stream(Box<dyn AsyncIo>);
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}
pub(super) struct Wire {
    stream: BufReader<Stream>,
    host: String,
}
impl Wire {
    pub async fn connect(host: &str, port: u16, tls: bool) -> Result<Self> {
        let stream = tokio::time::timeout(
            Duration::from_secs(20),
            tokio::net::TcpStream::connect((host, port)),
        )
        .await
        .map_err(|_| failure("Email connection timed out"))?
        .map_err(|_| failure("Email connection failed"))?;
        let mut wire = Self {
            stream: BufReader::new(Stream(Box::new(stream))),
            host: host.into(),
        };
        if tls {
            wire.start_tls().await?;
        }
        Ok(wire)
    }
    pub async fn start_tls(&mut self) -> Result<()> {
        // Replace only after taking ownership. No plaintext operation is ever
        // issued on the temporary stream, and this method returns on failure.
        let replacement = tokio::io::duplex(1).0;
        let current = std::mem::replace(
            &mut self.stream,
            BufReader::new(Stream(Box::new(replacement))),
        );
        if !current.buffer().is_empty() {
            return Err(failure("Unexpected plaintext after STARTTLS"));
        }
        let name = rustls::pki_types::ServerName::try_from(self.host.clone())
            .map_err(|_| failure("Invalid mail TLS hostname"))?;
        let connector = tokio_rustls::TlsConnector::from(Arc::new(
            crate::runtime::model::http::webpki_client_config(),
        ));
        let stream = tokio::time::timeout(
            Duration::from_secs(20),
            connector.connect(name, current.into_inner()),
        )
        .await
        .map_err(|_| failure("Mail TLS timed out"))?
        .map_err(|_| failure("Mail TLS failed"))?;
        self.stream = BufReader::new(Stream(Box::new(stream)));
        Ok(())
    }
    pub async fn line(&mut self) -> Result<Vec<u8>> {
        let mut line = Vec::new();
        let read = tokio::time::timeout(
            Duration::from_secs(30),
            self.stream.read_until(b'\n', &mut line),
        )
        .await
        .map_err(|_| failure("Mail response timed out"))?
        .map_err(|_| failure("Mail response failed"))?;
        if read == 0 {
            return Err(failure("Mail server disconnected"));
        }
        Ok(line)
    }
    pub async fn exact(&mut self, length: usize) -> Result<Vec<u8>> {
        let mut bytes = vec![0; length];
        tokio::time::timeout(Duration::from_secs(30), self.stream.read_exact(&mut bytes))
            .await
            .map_err(|_| failure("IMAP literal timed out"))?
            .map_err(|_| failure("IMAP literal read failed"))?;
        Ok(bytes)
    }
    pub async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), self.stream.write_all(bytes))
            .await
            .map_err(|_| failure("Mail write timed out"))?
            .map_err(|_| failure("Mail write failed"))
    }
}
pub(super) fn quote(text: &str) -> Result<String> {
    if text.contains(['\r', '\n', '\0']) {
        return Err(failure("Invalid mail protocol string"));
    }
    Ok(format!(
        "\"{}\"",
        text.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
