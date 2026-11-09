use crate::ProxyError;
use std::net::SocketAddr;
pub async fn serve_web(listen: SocketAddr) -> Result<(), ProxyError> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    std::mem::forget(listener);
    Ok(())
}
