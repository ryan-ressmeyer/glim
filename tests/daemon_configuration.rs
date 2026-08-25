use serde_json::json;
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
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("loopback"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !store.exists(),
        "unsafe startup opened the configured store"
    );

    fs::write(&config_path, br#"{"schema_version":1,"unknown":true}"#).unwrap();
    let output = command(&config_path).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("configuration"));

    fs::write(&config_path, vec![b' '; 64 * 1024 + 1]).unwrap();
    let output = command(&config_path).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("64 KiB"));

    let missing = root.path().join("missing.json");
    let output = command(&missing).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("explicit configuration"));
}
