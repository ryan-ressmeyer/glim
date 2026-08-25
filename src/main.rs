use std::{
    net::{Ipv4Addr, SocketAddr},
    process::ExitCode,
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("glim: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let root = glim::daemon::resolve_store_root()?;
    let store = glim::daemon::open_store(root)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, 3030));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| format!("could not bind {address}: {error}"))?;
    axum::serve(listener, glim::app_with_store(store))
        .await
        .map_err(|error| format!("server failed: {error}"))
}
