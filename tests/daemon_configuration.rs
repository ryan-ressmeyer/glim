use glim::storage::{PublicationFile, PublicationRequest, Store};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn free_loopback_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn command(config_path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glim"));
    command
        .arg("daemon")
        .env("GLIM_CONFIG", config_path)
        .env_remove("GLIM_STORE_ROOT")
        .env_remove("GLIM_BIND")
        .env_remove("GLIM_ACCESS_MODE")
        .env_remove("GLIM_TOKEN_FILE")
        .env_remove("GLIM_PUBLIC_ORIGIN")
        .env_remove("GLIM_TLS_CERTIFICATE")
        .env_remove("GLIM_TLS_PRIVATE_KEY")
        .env_remove("GLIM_TRUSTED_PROXY_IPS")
        .env_remove("GLIM_MAX_UPLOAD_BYTES")
        .env_remove("GLIM_MAX_FINALIZED_BLOB_BYTES")
        .env_remove("GLIM_LOG_LEVEL")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("HOME")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn explicit_configuration_starts_on_an_alternate_loopback_port() {
    let root = TempDir::new().unwrap();
    let store = root.path().join("store");
    let port = free_loopback_port();
    let config_path = root.path().join("config.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": store,
            "bind": format!("127.0.0.1:{port}")
        }))
        .unwrap(),
    )
    .unwrap();
    let mut daemon = Daemon(command(&config_path).spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(response) = reqwest::get(format!("http://127.0.0.1:{port}/api/v1/health")).await {
            assert!(response.status().is_success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "configured daemon did not listen"
        );
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "configured daemon exited"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(store.join("metadata.sqlite3").is_file());
}

#[tokio::test]
async fn token_tls_configuration_serves_only_authenticated_https() {
    let root = TempDir::new().unwrap();
    let store = root.path().join("store");
    let token_path = root.path().join("access-token");
    let certificate_path = root.path().join("cert.pem");
    let private_key_path = root.path().join("key.pem");
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    fs::write(&certificate_path, cert.pem()).unwrap();
    fs::write(&private_key_path, signing_key.serialize_pem()).unwrap();
    let port = free_loopback_port();
    let config_path = root.path().join("config.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": store,
            "bind": format!("127.0.0.1:{port}"),
            "access": {
                "mode": "token",
                "token_file": token_path,
                "public_origin": format!("https://localhost:{port}"),
                "tls_certificate": certificate_path,
                "tls_private_key": private_key_path
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut daemon = Daemon(command(&config_path).spawn().unwrap());
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(cert.pem().as_bytes()).unwrap())
        .build()
        .unwrap();
    let health_url = format!("https://localhost:{port}/api/v1/health");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(response) = client.get(&health_url).send().await {
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            break;
        }
        assert!(Instant::now() < deadline, "TLS daemon did not listen");
        assert!(daemon.0.try_wait().unwrap().is_none(), "TLS daemon exited");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let posts_url = format!("https://localhost:{port}/api/v1/posts");
    assert_eq!(
        client.get(&posts_url).send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let token = fs::read_to_string(&token_path).unwrap();
    assert_eq!(
        client
            .get(&posts_url)
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert!(
        reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap()
            .get(format!("http://127.0.0.1:{port}/api/v1/health"))
            .send()
            .await
            .is_err(),
        "TLS listener unexpectedly served plaintext HTTP"
    );
}

#[tokio::test]
async fn trusted_proxy_real_socket_serves_api_sse_and_ranges_only_for_allowlisted_peers() {
    let root = TempDir::new().unwrap();
    let store_path = root.path().join("store");
    let mut store = Store::open(&store_path).unwrap();
    let session = store
        .resolve_session(
            "socket-test",
            "trusted-proxy",
            "Trusted proxy",
            "/tmp/proxy",
        )
        .unwrap();
    let session_public_id = session.public_id.clone();
    let blob = store
        .stage_publication_blob(std::io::Cursor::new(b"range response"))
        .unwrap();
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Proxy artifact".into(),
                commentary: "Socket coverage".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "artifact.txt".into(),
                    caption: None,
                    blob,
                    support_assets: vec![],
                }],
            },
            1,
        )
        .unwrap();
    drop(store);

    let port = free_loopback_port();
    let config_path = root.path().join("trusted.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": store_path,
            "bind": format!("127.0.0.1:{port}"),
            "access": {
                "mode": "trusted_proxy",
                "trusted_proxy_ips": ["127.0.0.1"],
                "public_origin": format!("http://127.0.0.1:{port}")
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut daemon = Daemon(command(&config_path).spawn().unwrap());
    let health_url = format!("http://127.0.0.1:{port}/api/v1/health");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if reqwest::get(&health_url).await.is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "trusted-proxy daemon did not listen"
        );
        assert!(
            daemon.0.try_wait().unwrap().is_none(),
            "trusted-proxy daemon exited"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("http://127.0.0.1:{port}/api/v1/posts"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("http://127.0.0.1:{port}/api/v1/posts/events"))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .post(format!(
                "http://127.0.0.1:{port}/api/v1/sessions/{session_public_id}/heartbeat"
            ))
            .send()
            .await
            .unwrap()
            .status(),
        reqwest::StatusCode::OK
    );
    let range = client
        .get(format!(
            "http://127.0.0.1:{port}/api/v1/posts/1/files/0/content"
        ))
        .header("range", "bytes=0-4")
        .send()
        .await
        .unwrap();
    assert_eq!(range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.bytes().await.unwrap(), "range");
    drop(daemon);

    let denied_port = free_loopback_port();
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": store_path,
            "bind": format!("127.0.0.1:{denied_port}"),
            "access": {
                "mode": "trusted_proxy",
                "trusted_proxy_ips": ["127.0.0.2"],
                "public_origin": format!("http://127.0.0.1:{denied_port}")
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let mut denied = Daemon(command(&config_path).spawn().unwrap());
    let denied_health = format!("http://127.0.0.1:{denied_port}/api/v1/health");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if reqwest::get(&denied_health).await.is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "denial daemon did not listen");
        assert!(
            denied.0.try_wait().unwrap().is_none(),
            "denial daemon exited"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let response = client
        .get(format!("http://127.0.0.1:{denied_port}/api/v1/posts"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    let payload: Value = response.json().await.unwrap();
    assert_eq!(payload["error"]["code"], "proxy_authorization_required");
}

#[tokio::test]
async fn authenticated_status_reports_finite_limits_after_startup_cleanup() {
    let root = TempDir::new().unwrap();
    let store_path = root.path().join("store");
    let mut store = Store::open(&store_path).unwrap();
    let stale = store
        .resolve_session("test", "stale", "Private", "/private/stale")
        .unwrap();
    let fresh = store
        .resolve_session("test", "fresh", "Private", "/private/fresh")
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let connection = rusqlite::Connection::open(store_path.join("metadata.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET last_activity_at=?1 WHERE public_id=?2",
            rusqlite::params![now - 604_800, &stale.public_id],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE sessions SET last_activity_at=?1 WHERE public_id=?2",
            rusqlite::params![now, &fresh.public_id],
        )
        .unwrap();
    drop(store);
    let port = free_loopback_port();
    let config_path = root.path().join("status.json");
    fs::write(&config_path, serde_json::to_vec(&json!({
        "schema_version": 1,
        "store_root": store_path,
        "bind": format!("127.0.0.1:{port}"),
        "limits": {"max_upload_bytes": 512, "max_finalized_blob_bytes": 2048},
        "access": {"mode":"trusted_proxy", "trusted_proxy_ips":["127.0.0.1"], "public_origin":format!("http://127.0.0.1:{port}")}
    })).unwrap()).unwrap();
    let mut daemon = Daemon(command(&config_path).spawn().unwrap());
    let client = reqwest::Client::new();
    let status_url = format!("http://127.0.0.1:{port}/api/v1/status");
    let deadline = Instant::now() + Duration::from_secs(5);
    let status: Value = loop {
        if let Ok(response) = client.get(&status_url).send().await
            && response.status().is_success()
        {
            break response.json().await.unwrap();
        }
        assert!(Instant::now() < deadline, "daemon did not expose status");
        assert!(daemon.0.try_wait().unwrap().is_none(), "daemon exited");
        tokio::time::sleep(Duration::from_millis(25)).await;
    };
    assert_eq!(status["max_upload_bytes"], 512);
    assert_eq!(status["max_finalized_blob_bytes"], 2048);
    assert_eq!(status["active_sessions"], 1);
    let store = Store::open(&store_path).unwrap();
    assert!(store.session(&stale.public_id).is_err());
    assert!(store.session(&fresh.public_id).is_ok());
}

#[tokio::test]
async fn limit_environment_overrides_are_applied_before_relationship_validation() {
    let root = TempDir::new().unwrap();
    let port = free_loopback_port();
    let config_path = root.path().join("limit-precedence.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": root.path().join("store"),
            "bind": format!("127.0.0.1:{port}"),
            "limits": {"max_upload_bytes": 20, "max_finalized_blob_bytes": 10},
            "access": {"mode":"local"}
        }))
        .unwrap(),
    )
    .unwrap();
    let mut process = command(&config_path)
        .env("GLIM_MAX_FINALIZED_BLOB_BYTES", "30")
        .spawn()
        .unwrap();
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Ok(response) = client
            .get(format!("http://127.0.0.1:{port}/api/v1/health"))
            .send()
            .await
            && response.status().is_success()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not apply limit override"
        );
        assert!(
            process.try_wait().unwrap().is_none(),
            "daemon rejected the valid final limit configuration"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    process.kill().unwrap();
    process.wait().unwrap();
}

fn startup_error(output: &std::process::Output) -> Value {
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    let events = stderr
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    for event in &events {
        assert_eq!(event["schema_version"], 1);
        assert!(event["timestamp"].as_u64().is_some());
        assert!(event["event"].as_str().is_some());
    }
    let error = events
        .into_iter()
        .find(|event| event["event"] == "daemon_error")
        .unwrap_or_else(|| panic!("missing daemon_error: {stderr}"));
    assert_eq!(error["level"], "error");
    error
}

#[test]
fn startup_fails_closed_for_explicit_config_errors_and_non_loopback_overrides() {
    let root = TempDir::new().unwrap();
    let store = root.path().join("store");
    let config_path = root.path().join("config.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": store,
            "bind": format!("127.0.0.1:{}", free_loopback_port())
        }))
        .unwrap(),
    )
    .unwrap();

    let output = command(&config_path)
        .env("GLIM_BIND", format!("0.0.0.0:{}", free_loopback_port()))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = startup_error(&output);
    assert_eq!(error["stage"], "configuration");
    assert_eq!(error["category"], "invalid_configuration");
    assert!(
        !store.exists(),
        "unsafe startup opened the configured store"
    );

    let tls_store = root.path().join("tls-store");
    let token = root.path().join("tls-token");
    let certificate = root.path().join("invalid-cert.pem");
    let key = root.path().join("invalid-key.pem");
    fs::write(&certificate, b"invalid certificate").unwrap();
    fs::write(&key, b"invalid key").unwrap();
    let tls_port = free_loopback_port();
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "store_root": tls_store,
            "bind": format!("127.0.0.1:{tls_port}"),
            "access": {
                "mode": "token",
                "token_file": token,
                "public_origin": format!("https://localhost:{tls_port}"),
                "tls_certificate": certificate,
                "tls_private_key": key
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let output = command(&config_path).output().unwrap();
    assert!(!output.status.success());
    let error = startup_error(&output);
    assert_eq!(error["stage"], "tls");
    assert_eq!(error["category"], "tls_material_invalid");
    assert!(
        !tls_store.exists(),
        "invalid TLS opened the configured store"
    );

    fs::write(&config_path, br#"{"schema_version":1,"unknown":true}"#).unwrap();
    let output = command(&config_path).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(startup_error(&output)["category"], "invalid_configuration");

    fs::write(&config_path, vec![b' '; 64 * 1024 + 1]).unwrap();
    let output = command(&config_path).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(startup_error(&output)["category"], "invalid_configuration");

    let missing = root.path().join("missing.json");
    let output = command(&missing).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(startup_error(&output)["category"], "invalid_configuration");
}
