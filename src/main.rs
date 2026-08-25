use std::{
    net::{Ipv4Addr, SocketAddr},
    process::ExitCode,
};

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.is_empty()
        || arguments
            .first()
            .is_some_and(|argument| argument == "daemon")
    {
        if arguments.len() > 1 {
            print_cli_error(glim::cli::CliError {
                code: "usage_error".into(),
                message: "daemon does not accept arguments".into(),
                details: serde_json::Map::new(),
            });
            return ExitCode::FAILURE;
        }
        return match run_daemon().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("glim: {error}");
                ExitCode::FAILURE
            }
        };
    }

    match glim::cli::run_command(arguments).await {
        Ok(payload) => {
            println!(
                "{}",
                serde_json::to_string(&payload).expect("CLI result serializes")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            print_cli_error(error);
            ExitCode::FAILURE
        }
    }
}

fn print_cli_error(error: glim::cli::CliError) {
    println!(
        "{}",
        serde_json::to_string(&error.envelope()).expect("CLI error serializes")
    );
}

async fn run_daemon() -> Result<(), String> {
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
