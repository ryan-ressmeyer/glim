use std::process::ExitCode;

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
    let configuration = glim::daemon::resolve_daemon_configuration()?;
    match configuration.access {
        glim::daemon::AccessConfiguration::Local => {
            let store = glim::daemon::open_store(configuration.store)?;
            let listener = tokio::net::TcpListener::bind(configuration.bind)
                .await
                .map_err(|error| format!("could not bind {}: {error}", configuration.bind))?;
            axum::serve(listener, glim::app_with_store(store))
                .await
                .map_err(|error| format!("server failed: {error}"))
        }
        glim::daemon::AccessConfiguration::Token(access) => {
            let token = glim::daemon::load_or_create_access_token(&access.token_file)?;
            let tls = match access.tls {
                Some(files) => Some(
                    axum_server::tls_rustls::RustlsConfig::from_pem_file(
                        files.certificate,
                        files.private_key,
                    )
                    .await
                    .map_err(|error| format!("could not load TLS certificate and key: {error}"))?,
                ),
                None => None,
            };
            let store = glim::daemon::open_store(configuration.store)?;
            let app = glim::app_with_store_and_token_auth(
                store,
                token,
                access.public_origin,
                tls.is_some(),
            );
            if let Some(tls) = tls {
                axum_server::bind_rustls(configuration.bind, tls)
                    .serve(app.into_make_service())
                    .await
                    .map_err(|error| format!("TLS server failed: {error}"))
            } else {
                let listener = tokio::net::TcpListener::bind(configuration.bind)
                    .await
                    .map_err(|error| format!("could not bind {}: {error}", configuration.bind))?;
                axum::serve(listener, app)
                    .await
                    .map_err(|error| format!("server failed: {error}"))
            }
        }
    }
}
