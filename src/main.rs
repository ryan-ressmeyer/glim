use std::net::{Ipv4Addr, SocketAddr};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 3030));
    let listener = tokio::net::TcpListener::bind(address).await?;

    axum::serve(listener, glim::app()).await
}
