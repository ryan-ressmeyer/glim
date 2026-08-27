use std::process::ExitCode;

use glim::logging::{LogLevel, daemon};
use serde_json::json;

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
        let level = match LogLevel::parse(std::env::var_os("GLIM_LOG_LEVEL").as_deref()) {
            Ok(level) => level,
            Err(_) => {
                glim::logging::initialize_daemon(LogLevel::Info);
                daemon(
                    LogLevel::Error,
                    "daemon_error",
                    &[
                        ("stage", json!("logging")),
                        ("category", json!("invalid_log_level")),
                        (
                            "message",
                            json!("GLIM_LOG_LEVEL must be error, warn, or info"),
                        ),
                    ],
                );
                return ExitCode::FAILURE;
            }
        };
        glim::logging::initialize_daemon(level);
        return match run_daemon().await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                daemon(
                    LogLevel::Error,
                    "daemon_error",
                    &[
                        ("stage", json!(error.stage)),
                        ("category", json!(error.category)),
                        ("message", json!(error.message)),
                    ],
                );
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

struct DaemonFailure {
    stage: &'static str,
    category: &'static str,
    message: &'static str,
}

impl DaemonFailure {
    fn configuration() -> Self {
        Self {
            stage: "configuration",
            category: "invalid_configuration",
            message: "daemon configuration is invalid",
        }
    }

    fn new(stage: &'static str, category: &'static str, message: &'static str) -> Self {
        Self {
            stage,
            category,
            message,
        }
    }
}

async fn run_daemon() -> Result<(), DaemonFailure> {
    let configuration =
        glim::daemon::resolve_daemon_configuration().map_err(|_| DaemonFailure::configuration())?;
    let (access_mode, tls) = match &configuration.access {
        glim::daemon::AccessConfiguration::Local => ("local", false),
        glim::daemon::AccessConfiguration::Token(access) => ("token", access.tls.is_some()),
        glim::daemon::AccessConfiguration::TrustedProxy(_) => ("trusted_proxy", false),
    };
    daemon(
        LogLevel::Info,
        "daemon_starting",
        &[
            ("version", json!(env!("CARGO_PKG_VERSION"))),
            ("access_mode", json!(access_mode)),
            ("tls", json!(tls)),
            ("bind", json!(configuration.bind.to_string())),
            (
                "max_upload_bytes",
                json!(configuration.limits.max_upload_bytes),
            ),
            (
                "max_finalized_blob_bytes",
                json!(configuration.limits.max_finalized_blob_bytes),
            ),
        ],
    );
    let store_root = configuration.store.clone();
    let limits = configuration.limits;
    match configuration.access {
        glim::daemon::AccessConfiguration::Local => {
            let store = prepare_store(store_root, limits)?;
            let listener = tokio::net::TcpListener::bind(configuration.bind)
                .await
                .map_err(|_| DaemonFailure::new("bind", "bind_failed", "daemon bind failed"))?;
            axum::serve(
                listener,
                glim::app_with_store(store)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .map_err(|_| DaemonFailure::new("server", "server_failed", "daemon server failed"))
        }
        glim::daemon::AccessConfiguration::Token(access) => {
            let token =
                glim::daemon::load_or_create_access_token(&access.token_file).map_err(|_| {
                    DaemonFailure::new(
                        "access",
                        "token_material_invalid",
                        "access token unavailable",
                    )
                })?;
            let tls = match access.tls {
                Some(files) => Some(
                    axum_server::tls_rustls::RustlsConfig::from_pem_file(
                        files.certificate,
                        files.private_key,
                    )
                    .await
                    .map_err(|_| {
                        DaemonFailure::new("tls", "tls_material_invalid", "TLS material is invalid")
                    })?,
                ),
                None => None,
            };
            let store = prepare_store(store_root, limits)?;
            let app = glim::app_with_store_and_token_auth(
                store,
                token,
                access.public_origin,
                tls.is_some(),
            );
            if let Some(tls) = tls {
                axum_server::bind_rustls(configuration.bind, tls)
                    .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
                    .await
                    .map_err(|_| DaemonFailure::new("server", "server_failed", "TLS server failed"))
            } else {
                let listener = tokio::net::TcpListener::bind(configuration.bind)
                    .await
                    .map_err(|_| DaemonFailure::new("bind", "bind_failed", "daemon bind failed"))?;
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .await
                .map_err(|_| DaemonFailure::new("server", "server_failed", "daemon server failed"))
            }
        }
        glim::daemon::AccessConfiguration::TrustedProxy(access) => {
            let store = prepare_store(store_root, limits)?;
            let app = glim::app_with_store_and_trusted_proxy(
                store,
                access.trusted_proxy_ips,
                access.public_origin,
            );
            let listener = tokio::net::TcpListener::bind(configuration.bind)
                .await
                .map_err(|_| DaemonFailure::new("bind", "bind_failed", "daemon bind failed"))?;
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .map_err(|_| DaemonFailure::new("server", "server_failed", "daemon server failed"))
        }
    }
}

fn prepare_store(
    root: glim::daemon::StoreRoot,
    limits: glim::daemon::DaemonLimits,
) -> Result<glim::storage::Store, DaemonFailure> {
    let mut store = glim::daemon::open_store(root.clone(), limits).map_err(|_| {
        DaemonFailure::new(
            "storage",
            "store_open_failed",
            "daemon store could not be opened",
        )
    })?;
    let report = glim::daemon::purge_expired_sessions(&mut store, std::time::SystemTime::now())
        .map_err(|_| {
            daemon(
                LogLevel::Error,
                "cleanup_failed",
                &[
                    ("trigger", json!("startup")),
                    ("category", json!("cleanup_operation_failed")),
                ],
            );
            DaemonFailure::new(
                "cleanup",
                "startup_cleanup_failed",
                "startup cleanup failed",
            )
        })?;
    glim::daemon::log_cleanup_completed("startup", report);
    glim::daemon::spawn_periodic_cleanup(root, limits);
    Ok(store)
}
