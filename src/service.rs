use std::path::Path;

use serde_json::Value;

use crate::cli::CliError;

const UNIT_NAME: &str = "glim.service";
const MANAGED_MARKER: &str = "# Managed by Glimse. Do not edit.\n";

pub fn run(args: &[String]) -> Result<Value, CliError> {
    if !cfg!(target_os = "linux") {
        return Err(CliError::new(
            "service_unsupported_platform",
            "service management is supported only on Linux",
        ));
    }
    linux::run(args)
}

/// Escapes an executable path for use inside a double-quoted systemd ExecStart token.
pub fn escape_systemd_exec_path(path: &Path) -> Result<String, String> {
    let value = path
        .to_str()
        .ok_or_else(|| "executable path is not valid UTF-8".to_owned())?;
    if !path.is_absolute() {
        return Err("executable path is not absolute".into());
    }
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '%' => escaped.push_str("%%"),
            '\'' | '"' | '\\' => {
                return Err("executable path contains systemd-forbidden quoting characters".into());
            }
            value if value.is_control() => {
                return Err("executable path contains a control character".into());
            }
            value => escaped.push(value),
        }
    }
    Ok(escaped)
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        collections::HashMap,
        env, fs,
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        process::{Command, Stdio},
        sync::mpsc::{self, Receiver},
        thread,
        time::{Duration, Instant},
    };

    use serde_json::{Value, json};

    use super::{MANAGED_MARKER, UNIT_NAME, escape_systemd_exec_path};
    use crate::cli::CliError;

    const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(2);
    const MAX_SYSTEMCTL_STREAM_BYTES: usize = 32 * 1024;
    const MAX_UNIT_BYTES: u64 = 64 * 1024;

    pub(super) fn run(args: &[String]) -> Result<Value, CliError> {
        let subcommand = args
            .first()
            .ok_or_else(|| CliError::new("usage_error", "service requires a subcommand"))?;
        if args.len() != 1 {
            return Err(CliError::new(
                "usage_error",
                "service subcommands do not accept arguments",
            ));
        }
        match subcommand.as_str() {
            "install" => install(),
            "start" => change_state("start"),
            "stop" => change_state("stop"),
            "status" => status(),
            "uninstall" => uninstall(),
            _ => Err(CliError::new(
                "usage_error",
                format!("unknown service subcommand: {subcommand}"),
            )),
        }
    }

    fn unit_path() -> Result<PathBuf, CliError> {
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute() && path.to_str().is_some())
            .or_else(|| {
                env::var_os("HOME")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute() && path.to_str().is_some())
                    .map(|path| path.join(".config"))
            })
            .ok_or_else(|| {
                CliError::new(
                    "service_configuration_error",
                    "service unit directory requires an absolute XDG_CONFIG_HOME or HOME",
                )
            })?;
        Ok(config_home.join("systemd/user").join(UNIT_NAME))
    }

    fn render_unit() -> Result<String, CliError> {
        let executable = env::current_exe().map_err(|_| {
            CliError::new(
                "service_executable_error",
                "could not resolve the current executable",
            )
        })?;
        let executable = escape_systemd_exec_path(&executable).map_err(|message| {
            CliError::new(
                "service_executable_error",
                format!("current executable cannot be represented safely: {message}"),
            )
        })?;
        Ok(format!(
            "{MANAGED_MARKER}[Unit]\nDescription=Glimse visual output daemon\n\n[Service]\nType=simple\nExecStart=\"{executable}\" daemon\nRestart=on-failure\nRestartSec=2s\nUMask=0077\n\n[Install]\nWantedBy=default.target\n"
        ))
    }

    fn install() -> Result<Value, CliError> {
        let path = unit_path()?;
        if unit_entry_exists(&path)? {
            require_managed(&path)?;
        }
        let parent = path.parent().expect("unit path has a parent");
        fs::create_dir_all(parent).map_err(|_| {
            CliError::new(
                "service_filesystem_error",
                "could not create the systemd user unit directory",
            )
        })?;
        atomic_write(&path, render_unit()?.as_bytes())?;
        systemctl("daemon-reload", &[])?;
        systemctl("enable", &[UNIT_NAME])?;
        Ok(json!({"installed": true, "enabled": true, "unit_path": path}))
    }

    fn change_state(operation: &'static str) -> Result<Value, CliError> {
        let path = unit_path()?;
        require_managed(&path)?;
        systemctl(operation, &[UNIT_NAME])?;
        Ok(json!({"installed": true, "action": operation, "unit_path": path}))
    }

    fn status() -> Result<Value, CliError> {
        let path = unit_path()?;
        if !unit_entry_exists(&path)? {
            return Ok(json!({
                "installed": false,
                "unit_file_state": "not-found",
                "active_state": "inactive",
                "unit_path": path
            }));
        }
        require_managed(&path)?;
        let output = systemctl(
            "show",
            &[
                UNIT_NAME,
                "--property=LoadState",
                "--property=UnitFileState",
                "--property=ActiveState",
            ],
        )?;
        let properties = parse_show(&output.stdout)?;
        Ok(json!({
            "installed": true,
            "unit_file_state": properties["UnitFileState"],
            "active_state": properties["ActiveState"],
            "load_state": properties["LoadState"],
            "unit_path": path
        }))
    }

    fn uninstall() -> Result<Value, CliError> {
        let path = unit_path()?;
        if !unit_entry_exists(&path)? {
            return Ok(json!({"installed": false, "unit_path": path}));
        }
        let original_unit = read_managed_unit(&path)?;
        systemctl("disable", &["--now", UNIT_NAME])?;
        fs::remove_file(&path).map_err(|_| {
            CliError::new(
                "service_filesystem_error",
                "could not remove the managed service unit",
            )
        })?;
        if let Err(reload_error) = systemctl("daemon-reload", &[]) {
            atomic_restore(&path, &original_unit).map_err(|_| {
                CliError::new(
                    "service_filesystem_error",
                    "could not restore the managed service unit after daemon-reload failed",
                )
            })?;
            return Err(reload_error);
        }
        Ok(json!({"installed": false, "unit_path": path}))
    }

    fn unit_entry_exists(path: &Path) -> Result<bool, CliError> {
        match fs::symlink_metadata(path) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(CliError::new(
                "service_filesystem_error",
                "could not inspect the service unit",
            )),
        }
    }

    fn require_managed(path: &Path) -> Result<(), CliError> {
        read_managed_unit(path).map(|_| ())
    }

    fn read_managed_unit(path: &Path) -> Result<Vec<u8>, CliError> {
        let metadata = fs::symlink_metadata(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CliError::new(
                    "service_not_installed",
                    "the managed Glimse service is not installed",
                )
            } else {
                CliError::new(
                    "service_filesystem_error",
                    "could not inspect the service unit",
                )
            }
        })?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_UNIT_BYTES {
            return Err(unmanaged_error());
        }
        let mut bytes = Vec::new();
        File::open(path)
            .and_then(|file| file.take(MAX_UNIT_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|_| {
                CliError::new(
                    "service_filesystem_error",
                    "could not read the service unit",
                )
            })?;
        if bytes.len() as u64 > MAX_UNIT_BYTES || !bytes.starts_with(MANAGED_MARKER.as_bytes()) {
            return Err(unmanaged_error());
        }
        Ok(bytes)
    }

    fn unmanaged_error() -> CliError {
        CliError::new(
            "service_unit_unmanaged",
            "glim.service exists but is not a Glimse-managed unit",
        )
    }

    fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), CliError> {
        atomic_write_inner(path, contents, true)
    }

    fn atomic_restore(path: &Path, contents: &[u8]) -> Result<(), CliError> {
        atomic_write_inner(path, contents, false)
    }

    fn atomic_write_inner(
        path: &Path,
        contents: &[u8],
        cleanup_after_rename_failure: bool,
    ) -> Result<(), CliError> {
        let parent = path.parent().expect("unit path has a parent");
        let mut temporary = None;
        for attempt in 0..100_u32 {
            let candidate = parent.join(format!(
                ".glim.service.{}.{}.tmp",
                std::process::id(),
                attempt
            ));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
            {
                Ok(file) => {
                    temporary = Some((candidate, file));
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => break,
            }
        }
        let (temporary_path, mut file) = temporary.ok_or_else(|| {
            CliError::new(
                "service_filesystem_error",
                "could not create a temporary service unit",
            )
        })?;
        if file
            .write_all(contents)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::set_permissions(&temporary_path, fs::Permissions::from_mode(0o600)))
            .is_err()
        {
            let _ = fs::remove_file(&temporary_path);
            return Err(CliError::new(
                "service_filesystem_error",
                "could not atomically write the service unit",
            ));
        }
        if fs::rename(&temporary_path, path).is_err() {
            if cleanup_after_rename_failure {
                let _ = fs::remove_file(&temporary_path);
            }
            return Err(CliError::new(
                "service_filesystem_error",
                "could not atomically write the service unit",
            ));
        }
        Ok(())
    }

    struct ProcessOutput {
        stdout: Vec<u8>,
    }

    fn systemctl(operation: &'static str, arguments: &[&str]) -> Result<ProcessOutput, CliError> {
        let deadline = Instant::now() + SYSTEMCTL_TIMEOUT;
        let mut child = Command::new("systemctl")
            .arg("--user")
            .arg(operation)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|_| {
                systemctl_error(
                    "systemctl_spawn_failed",
                    "could not start systemctl",
                    operation,
                    None,
                )
            })?;
        let stdout = drain_bounded(child.stdout.take().expect("stdout was piped"));
        let stderr = drain_bounded(child.stderr.take().expect("stderr was piped"));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(systemctl_error(
                        "systemctl_timeout",
                        "systemctl command timed out",
                        operation,
                        None,
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(systemctl_error(
                        "systemctl_wait_failed",
                        "could not wait for systemctl",
                        operation,
                        None,
                    ));
                }
            }
        };
        let (stdout, stdout_large) = receive_output(stdout, deadline, operation)?;
        let (_, stderr_large) = receive_output(stderr, deadline, operation)?;
        if stdout_large || stderr_large {
            return Err(systemctl_error(
                "systemctl_output_too_large",
                "systemctl output exceeded the supported limit",
                operation,
                status.code(),
            ));
        }
        if !status.success() {
            return Err(systemctl_error(
                "systemctl_failed",
                "systemctl command failed",
                operation,
                status.code(),
            ));
        }
        Ok(ProcessOutput { stdout })
    }

    fn drain_bounded<R: Read + Send + 'static>(mut reader: R) -> Receiver<(Vec<u8>, bool)> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut retained = Vec::new();
            let mut oversized = false;
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(count) => {
                        let available = MAX_SYSTEMCTL_STREAM_BYTES.saturating_sub(retained.len());
                        retained.extend_from_slice(&buffer[..count.min(available)]);
                        oversized |= count > available;
                    }
                }
            }
            let _ = sender.send((retained, oversized));
        });
        receiver
    }

    fn receive_output(
        receiver: Receiver<(Vec<u8>, bool)>,
        deadline: Instant,
        operation: &str,
    ) -> Result<(Vec<u8>, bool), CliError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        receiver
            .recv_timeout(remaining)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => systemctl_error(
                    "systemctl_timeout",
                    "systemctl command timed out",
                    operation,
                    None,
                ),
                mpsc::RecvTimeoutError::Disconnected => systemctl_error(
                    "systemctl_output_error",
                    "could not collect systemctl output",
                    operation,
                    None,
                ),
            })
    }

    fn systemctl_error(
        code: &str,
        message: &str,
        operation: &str,
        exit_code: Option<i32>,
    ) -> CliError {
        CliError::new(code, message).with_details(json!({
            "operation": operation,
            "exit_code": exit_code
        }))
    }

    fn parse_show(bytes: &[u8]) -> Result<HashMap<String, String>, CliError> {
        let text = std::str::from_utf8(bytes).map_err(|_| malformed_status())?;
        let mut values = HashMap::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let (key, value) = line.split_once('=').ok_or_else(malformed_status)?;
            if !matches!(key, "LoadState" | "UnitFileState" | "ActiveState")
                || !safe_state(value)
                || values.insert(key.to_owned(), value.to_owned()).is_some()
            {
                return Err(malformed_status());
            }
        }
        if !["LoadState", "UnitFileState", "ActiveState"]
            .iter()
            .all(|key| values.contains_key(*key))
        {
            return Err(malformed_status());
        }
        Ok(values)
    }

    fn safe_state(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }

    fn malformed_status() -> CliError {
        CliError::new(
            "service_status_malformed",
            "systemctl returned malformed service state",
        )
    }
}

#[cfg(not(target_os = "linux"))]
mod linux {
    use serde_json::Value;

    use crate::cli::CliError;

    pub(super) fn run(_: &[String]) -> Result<Value, CliError> {
        Err(CliError::new(
            "service_unsupported_platform",
            "service management is supported only on Linux",
        ))
    }
}
