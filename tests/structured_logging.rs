use std::{
    ffi::OsStr,
    io::{self, Write},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
};

use glim::logging::{LogLevel, Logger};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl SharedWriter {
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

fn lines(writer: &SharedWriter) -> Vec<Value> {
    String::from_utf8(writer.bytes())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn logger_emits_bounded_json_lines_with_fixed_schema() {
    let writer = SharedWriter::default();
    let logger = Logger::new(LogLevel::Info, writer.clone());
    logger.emit(
        LogLevel::Info,
        "test_event",
        &[("message", json!("é".repeat(8_000)))],
    );

    let bytes = writer.bytes();
    assert!(bytes.ends_with(b"\n"));
    assert!(bytes.len() <= 4096);
    let event: Value = serde_json::from_slice(&bytes[..bytes.len() - 1]).unwrap();
    assert_eq!(event["schema_version"], 1);
    assert!(event["timestamp"].as_u64().is_some());
    assert_eq!(event["level"], "info");
    assert_eq!(event["event"], "test_event");
}

#[test]
fn logger_filters_levels_and_parses_only_exact_supported_values() {
    assert_eq!(LogLevel::parse(None).unwrap(), LogLevel::Info);
    assert_eq!(
        LogLevel::parse(Some(OsStr::new("error"))).unwrap(),
        LogLevel::Error
    );
    assert_eq!(
        LogLevel::parse(Some(OsStr::new("warn"))).unwrap(),
        LogLevel::Warn
    );
    assert_eq!(
        LogLevel::parse(Some(OsStr::new("info"))).unwrap(),
        LogLevel::Info
    );
    for invalid in ["", " ", "debug", "INFO"] {
        assert!(LogLevel::parse(Some(OsStr::new(invalid))).is_err());
    }

    let writer = SharedWriter::default();
    let logger = Logger::new(LogLevel::Warn, writer.clone());
    logger.emit(LogLevel::Info, "ignored", &[]);
    logger.emit(LogLevel::Warn, "warning", &[]);
    logger.emit(LogLevel::Error, "failure", &[]);
    assert_eq!(
        lines(&writer)
            .into_iter()
            .map(|line| line["event"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        ["warning", "failure"]
    );
}

#[cfg(unix)]
#[test]
fn non_utf8_level_is_rejected() {
    use std::os::unix::ffi::OsStrExt;
    assert!(LogLevel::parse(Some(OsStr::from_bytes(b"info\xff"))).is_err());
}

#[test]
fn concurrent_writes_do_not_interleave_and_writer_failure_is_ignored() {
    let writer = SharedWriter::default();
    let logger = Logger::new(LogLevel::Info, writer.clone());
    let workers = (0..16)
        .map(|worker| {
            let logger = logger.clone();
            std::thread::spawn(move || {
                for sequence in 0..50 {
                    logger.emit(
                        LogLevel::Info,
                        "concurrent_event",
                        &[("worker", json!(worker)), ("sequence", json!(sequence))],
                    );
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    let output = writer.bytes();
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 800);
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        assert!(line.len() < 4096);
        serde_json::from_slice::<Value>(line).unwrap();
    }

    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    Logger::new(LogLevel::Info, Broken).emit(LogLevel::Error, "write_failed", &[]);
}

fn clean_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glim"));
    for name in [
        "GLIM_CONFIG",
        "GLIM_STORE_ROOT",
        "GLIM_BIND",
        "GLIM_ACCESS_MODE",
        "GLIM_TOKEN_FILE",
        "GLIM_PUBLIC_ORIGIN",
        "GLIM_TLS_CERTIFICATE",
        "GLIM_TLS_PRIVATE_KEY",
        "GLIM_TRUSTED_PROXY_IPS",
        "GLIM_MAX_UPLOAD_BYTES",
        "GLIM_MAX_FINALIZED_BLOB_BYTES",
        "GLIM_LOG_LEVEL",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "HOME",
    ] {
        command.env_remove(name);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command
}

#[test]
fn cli_commands_never_activate_daemon_logging() {
    let output = clean_command().arg("service-status").output().unwrap();
    assert!(output.stderr.is_empty());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
}

#[test]
fn invalid_level_and_configuration_are_structured_bounded_startup_errors() {
    let invalid_level = clean_command()
        .arg("daemon")
        .env("GLIM_LOG_LEVEL", "debug")
        .output()
        .unwrap();
    assert!(!invalid_level.status.success());
    let level_line: Value = serde_json::from_slice(&invalid_level.stderr).unwrap();
    assert_eq!(level_line["event"], "daemon_error");
    assert_eq!(level_line["stage"], "logging");
    assert_eq!(level_line["category"], "invalid_log_level");
    assert!(invalid_level.stderr.len() <= 4096);

    let marker = "FORBIDDEN_CONFIG_PATH_MARKER";
    let invalid_config = clean_command()
        .arg("daemon")
        .env("GLIM_CONFIG", format!("/tmp/{marker}"))
        .output()
        .unwrap();
    assert!(!invalid_config.status.success());
    let config_line: Value = serde_json::from_slice(&invalid_config.stderr).unwrap();
    assert_eq!(config_line["event"], "daemon_error");
    assert_eq!(config_line["stage"], "configuration");
    assert!(
        !String::from_utf8(invalid_config.stderr)
            .unwrap()
            .contains(marker)
    );
}

#[tokio::test]
async fn daemon_logs_only_allowlisted_publication_close_and_bounded_request_events() {
    let root = tempfile::tempdir().unwrap();
    let store = root.path().join("FORBIDDEN_STORE_PATH/store");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut child = clean_command()
        .arg("daemon")
        .env("GLIM_STORE_ROOT", &store)
        .env("GLIM_BIND", format!("127.0.0.1:{port}"))
        .spawn()
        .unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://127.0.0.1:{port}/api/v1");
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if client.get(format!("{base}/health")).send().await.is_ok() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    for _ in 0..20 {
        assert!(
            client
                .get(format!("{base}/health?FORBIDDEN_QUERY"))
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
        assert!(
            client
                .get(format!("{base}/status"))
                .header("User-Agent", "FORBIDDEN_HEADER")
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }

    let marker = "FORBIDDEN_MANIFEST_MARKER";
    let manifest = json!({
        "integration_namespace": marker,
        "external_key": marker,
        "project_label": marker,
        "working_directory": format!("/tmp/{marker}"),
        "title": marker,
        "commentary": marker,
        "predecessor_post_id": null,
        "git": {"root": format!("/tmp/{marker}"), "branch": marker, "commit": null},
        "files": [{"part":"artifact", "filename":format!("{marker}.txt"), "caption":marker, "media_type":"text/plain", "support_assets":[]}]
    });
    let response = client
        .post(format!("{base}/posts"))
        .multipart(
            reqwest::multipart::Form::new()
                .text("manifest", manifest.to_string())
                .part(
                    "artifact",
                    reqwest::multipart::Part::bytes(marker.as_bytes().to_vec()),
                ),
        )
        .header("User-Agent", "FORBIDDEN_HEADER")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    let published: Value = response.json().await.unwrap();
    let public_id = published["session"]["public_id"].as_str().unwrap();

    let failed = client
        .post(format!("{base}/posts"))
        .header("content-type", "multipart/form-data; boundary=missing")
        .body("FORBIDDEN_BODY")
        .send()
        .await
        .unwrap();
    assert!(!failed.status().is_success());
    assert!(
        client
            .delete(format!("{base}/sessions/{public_id}"))
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );

    rusqlite::Connection::open(store.join("metadata.sqlite3"))
        .unwrap()
        .execute_batch("DROP TABLE sessions;")
        .unwrap();
    let internal_failure = client
        .post(format!("{base}/posts"))
        .multipart(
            reqwest::multipart::Form::new()
                .text("manifest", manifest.to_string())
                .part(
                    "artifact",
                    reqwest::multipart::Part::bytes(marker.as_bytes().to_vec()),
                ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        internal_failure.status(),
        reqwest::StatusCode::INTERNAL_SERVER_ERROR
    );

    child.kill().unwrap();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains(marker));
    assert!(!stderr.contains("FORBIDDEN_"));
    let events = stderr
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert!(events.len() <= 7, "unexpected request logging: {stderr}");
    let common = ["schema_version", "timestamp", "level", "event"];
    for event in &events {
        let allowed: &[&str] = match event["event"].as_str().unwrap() {
            "daemon_starting" => &[
                "version",
                "access_mode",
                "tls",
                "bind",
                "max_upload_bytes",
                "max_finalized_blob_bytes",
            ],
            "cleanup_completed" => &[
                "trigger",
                "sessions_deleted",
                "projects_deleted",
                "posts_deleted",
                "post_files_deleted",
                "support_assets_deleted",
                "blob_references_deleted",
                "blobs_queued",
                "blobs_deleted",
            ],
            "publication_succeeded" => &[
                "post_id",
                "visible_file_count",
                "support_asset_count",
                "staged_bytes",
            ],
            "publication_failed" => &["http_status", "api_error_code"],
            "session_closed" => &[
                "sessions_deleted",
                "projects_deleted",
                "posts_deleted",
                "post_files_deleted",
                "support_assets_deleted",
                "blob_references_deleted",
                "blobs_queued",
                "blobs_deleted",
            ],
            other => panic!("unexpected event {other}: {event}"),
        };
        assert!(
            event
                .as_object()
                .unwrap()
                .keys()
                .all(|key| common.contains(&key.as_str()) || allowed.contains(&key.as_str())),
            "unexpected key in {event}"
        );
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "publication_succeeded")
            .count(),
        1
    );
    let publication_failures = events
        .iter()
        .filter(|event| event["event"] == "publication_failed")
        .collect::<Vec<_>>();
    assert_eq!(publication_failures.len(), 2);
    assert!(
        publication_failures
            .iter()
            .any(|event| { event["http_status"] == 400 && event["level"] == "warn" })
    );
    assert!(
        publication_failures
            .iter()
            .any(|event| { event["http_status"] == 500 && event["level"] == "error" })
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "session_closed")
            .count(),
        1
    );
}
