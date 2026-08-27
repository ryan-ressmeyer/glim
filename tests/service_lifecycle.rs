#![cfg(target_os = "linux")]

use std::{
    ffi::OsString,
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

use serde_json::Value;
use tempfile::TempDir;

const MARKER: &str = "# Managed by Glimse. Do not edit.\n";

struct Fixture {
    root: TempDir,
    bin: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let log = root.path().join("systemctl.log");
        let script = bin.join("systemctl");
        fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$FAKE_SYSTEMCTL_LOG"
if [ -n "$FAKE_SYSTEMCTL_STDOUT_BYTES" ]; then
  head -c "$FAKE_SYSTEMCTL_STDOUT_BYTES" /dev/zero | tr '\0' x
  exit 0
fi
if [ -n "$FAKE_SYSTEMCTL_SLEEP" ]; then sleep "$FAKE_SYSTEMCTL_SLEEP"; fi
if [ -n "$FAKE_SYSTEMCTL_INHERITED_PIPE_SLEEP" ]; then
  (sleep "$FAKE_SYSTEMCTL_INHERITED_PIPE_SLEEP") &
  exit 0
fi
if [ "$2" = "show" ]; then printf '%s' "$FAKE_SYSTEMCTL_SHOW"; fi
if [ "$2" = "$FAKE_SYSTEMCTL_FAIL_OPERATION" ]; then exit 7; fi
exit "${FAKE_SYSTEMCTL_EXIT:-0}"
"#,
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, bin, log }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_glim"));
        command
            .env("XDG_CONFIG_HOME", self.root.path())
            .env_remove("HOME")
            .env("PATH", format!("{}:/usr/bin:/bin", self.bin.display()))
            .env("FAKE_SYSTEMCTL_LOG", &self.log)
            .env_remove("FAKE_SYSTEMCTL_SHOW")
            .env_remove("FAKE_SYSTEMCTL_EXIT")
            .env_remove("FAKE_SYSTEMCTL_SLEEP")
            .env_remove("FAKE_SYSTEMCTL_INHERITED_PIPE_SLEEP")
            .env_remove("FAKE_SYSTEMCTL_FAIL_OPERATION")
            .env_remove("FAKE_SYSTEMCTL_STDOUT_BYTES");
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
            "GLIM_DAEMON_URL",
            "GLIM_BROWSER_COMMAND",
        ] {
            command.env_remove(name);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn unit_path(&self) -> PathBuf {
        self.root.path().join("systemd/user/glim.service")
    }

    fn log(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }
}

fn json(output: &Output) -> Value {
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(stdout.lines().count(), 1, "stdout: {stdout:?}");
    serde_json::from_str(stdout.trim()).unwrap()
}

fn error_code(output: &Output) -> String {
    assert!(!output.status.success());
    json(output)["error"]["code"].as_str().unwrap().to_owned()
}

fn installed_unit(executable: &Path) -> String {
    format!(
        "{MARKER}[Unit]\nDescription=Glimse visual output daemon\n\n[Service]\nType=simple\nExecStart=\"{}\" daemon\nRestart=on-failure\nRestartSec=2s\nUMask=0077\n\n[Install]\nWantedBy=default.target\n",
        glim::service::escape_systemd_exec_path(executable).unwrap()
    )
}

#[test]
fn install_writes_the_exact_unit_then_reloads_and_enables_without_starting() {
    let fixture = Fixture::new();
    let first = fixture.run(&["service", "install"]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stdout)
    );
    assert_eq!(json(&first)["result"]["installed"], true);
    assert_eq!(
        fs::read_to_string(fixture.unit_path()).unwrap(),
        installed_unit(Path::new(env!("CARGO_BIN_EXE_glim")))
    );
    assert_eq!(
        fixture.log(),
        "--user daemon-reload\n--user enable glim.service\n"
    );
    assert!(!fixture.log().contains("start"));

    fs::write(&fixture.log, "").unwrap();
    let second = fixture.run(&["service", "install"]);
    assert!(second.status.success());
    assert_eq!(
        fs::read_to_string(fixture.unit_path()).unwrap(),
        installed_unit(Path::new(env!("CARGO_BIN_EXE_glim")))
    );
    assert_eq!(
        fixture.log(),
        "--user daemon-reload\n--user enable glim.service\n"
    );
}

#[test]
fn renderer_escapes_specifiers_and_rejects_unrepresentable_executable_paths() {
    assert_eq!(
        glim::service::escape_systemd_exec_path(Path::new("/tmp/a space/percent%")).unwrap(),
        "/tmp/a space/percent%%"
    );
    for path in [
        "/tmp/quote\"",
        "/tmp/apostrophe'",
        "/tmp/backslash\\",
        "/tmp/tab\t",
        "/tmp/newline\n",
        "/tmp/delete\u{7f}",
    ] {
        assert!(
            glim::service::escape_systemd_exec_path(Path::new(path)).is_err(),
            "systemd must reject unrepresentable executable path {path:?}"
        );
    }
}

#[test]
fn install_refuses_to_replace_an_unmanaged_unit() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.unit_path().parent().unwrap()).unwrap();
    fs::write(fixture.unit_path(), "[Service]\nExecStart=/usr/bin/other\n").unwrap();
    let output = fixture.run(&["service", "install"]);
    assert_eq!(error_code(&output), "service_unit_unmanaged");
    assert_eq!(fixture.log(), "");
    assert_eq!(
        fs::read_to_string(fixture.unit_path()).unwrap(),
        "[Service]\nExecStart=/usr/bin/other\n"
    );
}

#[test]
fn install_refuses_to_replace_a_broken_unit_symlink() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.unit_path().parent().unwrap()).unwrap();
    symlink(
        fixture.root.path().join("missing-target"),
        fixture.unit_path(),
    )
    .unwrap();
    let output = fixture.run(&["service", "install"]);
    assert_eq!(error_code(&output), "service_unit_unmanaged");
    assert!(
        fs::symlink_metadata(fixture.unit_path())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fixture.log(), "");
}

#[test]
fn start_and_stop_require_the_managed_unit_and_invoke_systemctl() {
    let fixture = Fixture::new();
    assert_eq!(
        error_code(&fixture.run(&["service", "start"])),
        "service_not_installed"
    );
    assert_eq!(
        error_code(&fixture.run(&["service", "stop"])),
        "service_not_installed"
    );

    assert!(fixture.run(&["service", "install"]).status.success());
    fs::write(&fixture.log, "").unwrap();
    assert!(fixture.run(&["service", "start"]).status.success());
    assert!(fixture.run(&["service", "stop"]).status.success());
    assert_eq!(
        fixture.log(),
        "--user start glim.service\n--user stop glim.service\n"
    );
}

#[test]
fn status_reports_absent_inactive_and_active_as_successful_structured_states() {
    let fixture = Fixture::new();
    let absent = fixture.run(&["service", "status"]);
    assert!(absent.status.success());
    let absent = json(&absent);
    assert_eq!(absent["result"]["installed"], false);
    assert_eq!(absent["result"]["unit_file_state"], "not-found");
    assert_eq!(absent["result"]["active_state"], "inactive");
    assert_eq!(
        absent["result"]["unit_path"],
        fixture.unit_path().to_string_lossy().as_ref()
    );

    assert!(fixture.run(&["service", "install"]).status.success());
    for (show, enabled, active) in [
        (
            "LoadState=loaded\nUnitFileState=enabled\nActiveState=inactive\n",
            "enabled",
            "inactive",
        ),
        (
            "LoadState=loaded\nUnitFileState=enabled\nActiveState=active\n",
            "enabled",
            "active",
        ),
    ] {
        let output = fixture
            .command()
            .args(["service", "status"])
            .env("FAKE_SYSTEMCTL_SHOW", show)
            .output()
            .unwrap();
        assert!(output.status.success());
        let payload = json(&output);
        assert_eq!(payload["result"]["installed"], true);
        assert_eq!(payload["result"]["unit_file_state"], enabled);
        assert_eq!(payload["result"]["active_state"], active);
    }
}

#[test]
fn malformed_failed_and_oversized_systemctl_results_are_typed_and_bounded() {
    let fixture = Fixture::new();
    assert!(fixture.run(&["service", "install"]).status.success());

    let malformed = fixture
        .command()
        .args(["service", "status"])
        .env("FAKE_SYSTEMCTL_SHOW", "not properties\n")
        .output()
        .unwrap();
    assert_eq!(error_code(&malformed), "service_status_malformed");

    let failed = fixture
        .command()
        .args(["service", "status"])
        .env("FAKE_SYSTEMCTL_EXIT", "7")
        .output()
        .unwrap();
    assert_eq!(error_code(&failed), "systemctl_failed");
    assert!(json(&failed)["error"]["details"].to_string().len() < 1024);

    let oversized = fixture
        .command()
        .args(["service", "status"])
        .env("FAKE_SYSTEMCTL_STDOUT_BYTES", "70000")
        .output()
        .unwrap();
    assert_eq!(error_code(&oversized), "systemctl_output_too_large");
    assert!(
        oversized.stdout.len() < 2048,
        "CLI leaked subprocess output"
    );
}

#[test]
fn systemctl_spawn_failure_and_timeout_are_typed() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.unit_path().parent().unwrap()).unwrap();
    fs::write(fixture.unit_path(), MARKER).unwrap();

    let spawn = fixture
        .command()
        .args(["service", "start"])
        .env("PATH", fixture.root.path().join("missing-bin"))
        .output()
        .unwrap();
    assert_eq!(error_code(&spawn), "systemctl_spawn_failed");

    let timeout = fixture
        .command()
        .args(["service", "start"])
        .env("FAKE_SYSTEMCTL_SLEEP", "3")
        .output()
        .unwrap();
    assert_eq!(error_code(&timeout), "systemctl_timeout");
}

#[test]
fn inherited_output_descriptors_cannot_extend_the_systemctl_deadline() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.unit_path().parent().unwrap()).unwrap();
    fs::write(fixture.unit_path(), MARKER).unwrap();

    let started = Instant::now();
    let output = fixture
        .command()
        .args(["service", "start"])
        .env("FAKE_SYSTEMCTL_INHERITED_PIPE_SLEEP", "5")
        .output()
        .unwrap();
    assert_eq!(error_code(&output), "systemctl_timeout");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "inherited output descriptors held the CLI for {:?}",
        started.elapsed()
    );
}

#[test]
fn failed_uninstall_reload_restores_the_managed_unit_for_a_safe_retry() {
    let fixture = Fixture::new();
    assert!(fixture.run(&["service", "install"]).status.success());
    let original = fs::read(fixture.unit_path()).unwrap();

    let failed = fixture
        .command()
        .args(["service", "uninstall"])
        .env("FAKE_SYSTEMCTL_FAIL_OPERATION", "daemon-reload")
        .output()
        .unwrap();
    assert_eq!(error_code(&failed), "systemctl_failed");
    assert_eq!(fs::read(fixture.unit_path()).unwrap(), original);

    let retried = fixture.run(&["service", "uninstall"]);
    assert!(retried.status.success());
    assert!(!fixture.unit_path().exists());
}

#[test]
fn service_unit_location_falls_back_to_home_and_ignores_daemon_configuration_values() {
    let fixture = Fixture::new();
    let home = fixture.root.path().join("home");
    fs::create_dir(&home).unwrap();
    let output = fixture
        .command()
        .args(["service", "install"])
        .env("XDG_CONFIG_HOME", "relative-is-unusable")
        .env("HOME", &home)
        .env("GLIM_CONFIG", "relative-invalid-daemon-config")
        .env("GLIM_STORE_ROOT", "relative-invalid-store")
        .env("GLIM_ACCESS_MODE", "invalid-mode")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(home.join(".config/systemd/user/glim.service").is_file());

    let unavailable = fixture
        .command()
        .args(["service", "status"])
        .env("XDG_CONFIG_HOME", "relative")
        .env_remove("HOME")
        .output()
        .unwrap();
    assert_eq!(error_code(&unavailable), "service_configuration_error");
}

#[test]
fn non_utf8_unit_location_returns_an_envelope_instead_of_panicking_during_json_output() {
    let fixture = Fixture::new();
    let invalid = OsString::from_vec(b"/tmp/glim-\xff".to_vec());
    let output = fixture
        .command()
        .args(["service", "status"])
        .env("XDG_CONFIG_HOME", invalid)
        .env_remove("HOME")
        .output()
        .unwrap();
    assert_eq!(error_code(&output), "service_configuration_error");
}

#[test]
fn uninstall_is_idempotent_and_preserves_configuration_and_store_data() {
    let fixture = Fixture::new();
    let config = fixture.root.path().join("glim/config.json");
    let store = fixture.root.path().join("store/sentinel");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    fs::write(&config, "config").unwrap();
    fs::write(&store, "session data").unwrap();

    let absent = fixture.run(&["service", "uninstall"]);
    assert!(absent.status.success());
    assert_eq!(json(&absent)["result"]["installed"], false);
    assert_eq!(fixture.log(), "");

    assert!(fixture.run(&["service", "install"]).status.success());
    fs::write(&fixture.log, "").unwrap();
    let removed = fixture.run(&["service", "uninstall"]);
    assert!(removed.status.success());
    assert!(!fixture.unit_path().exists());
    assert_eq!(
        fixture.log(),
        "--user disable --now glim.service\n--user daemon-reload\n"
    );
    assert_eq!(fs::read_to_string(config).unwrap(), "config");
    assert_eq!(fs::read_to_string(store).unwrap(), "session data");
}

#[test]
fn malformed_service_subcommands_return_one_usage_error_envelope() {
    let fixture = Fixture::new();
    for args in [
        vec!["service"],
        vec!["service", "unknown"],
        vec!["service", "start", "extra"],
    ] {
        let output = fixture.run(&args);
        assert_eq!(error_code(&output), "usage_error");
    }
}
