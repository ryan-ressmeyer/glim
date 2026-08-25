use std::{
    ffi::OsStr,
    fs,
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::storage::Store;

const CONFIG_SCHEMA_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const DEFAULT_BIND: &str = "127.0.0.1:3030";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoot {
    pub path: PathBuf,
    pub explicit_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfiguration {
    pub store: StoreRoot,
    pub bind: SocketAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfiguration {
    schema_version: u32,
    store_root: Option<PathBuf>,
    bind: Option<String>,
}

pub fn resolve_daemon_configuration_values(
    file_bytes: Option<&[u8]>,
    store_override: Option<&OsStr>,
    bind_override: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<DaemonConfiguration, String> {
    let file = file_bytes
        .map(|bytes| {
            serde_json::from_slice::<FileConfiguration>(bytes)
                .map_err(|error| format!("daemon configuration is malformed: {error}"))
        })
        .transpose()?;
    if file
        .as_ref()
        .is_some_and(|configuration| configuration.schema_version != CONFIG_SCHEMA_VERSION)
    {
        return Err(format!(
            "daemon configuration schema_version must be {CONFIG_SCHEMA_VERSION}"
        ));
    }

    let store = if store_override.is_some() {
        resolve_store_root_values(store_override, xdg_data_home, home)?
    } else if let Some(path) = file
        .as_ref()
        .and_then(|configuration| configuration.store_root.clone())
    {
        if path.as_os_str().to_string_lossy().trim().is_empty() || !path.is_absolute() {
            return Err("daemon configuration store_root must be an absolute path".to_owned());
        }
        StoreRoot {
            path,
            explicit_override: true,
        }
    } else {
        resolve_store_root_values(None, xdg_data_home, home)?
    };

    let bind_value = match bind_override {
        Some(value) => value
            .to_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "GLIM_BIND must be a nonblank UTF-8 socket address".to_owned())?,
        None => file
            .as_ref()
            .and_then(|configuration| configuration.bind.as_deref())
            .unwrap_or(DEFAULT_BIND),
    };
    let bind = bind_value.parse::<SocketAddr>().map_err(|_| {
        "daemon bind must be a numeric IP socket address such as 127.0.0.1:3030".to_owned()
    })?;
    if bind.port() == 0 {
        return Err("daemon bind port must be greater than zero".to_owned());
    }
    if !bind.ip().is_loopback() {
        return Err(
            "daemon bind must use a loopback address until authenticated access is configured"
                .to_owned(),
        );
    }

    Ok(DaemonConfiguration { store, bind })
}

pub fn resolve_daemon_configuration() -> Result<DaemonConfiguration, String> {
    let file_bytes = load_configuration_file(
        std::env::var_os("GLIM_CONFIG").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    resolve_daemon_configuration_values(
        file_bytes.as_deref(),
        std::env::var_os("GLIM_STORE_ROOT").as_deref(),
        std::env::var_os("GLIM_BIND").as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

fn load_configuration_file(
    explicit_path: Option<&OsStr>,
    xdg_config_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<Option<Vec<u8>>, String> {
    let (path, required) = if let Some(path) = explicit_path {
        let path =
            nonblank_path(Some(path)).ok_or_else(|| "GLIM_CONFIG must not be blank".to_owned())?;
        if !path.is_absolute() {
            return Err("GLIM_CONFIG must be an absolute path".to_owned());
        }
        (path, true)
    } else if let Some(path) = nonblank_path(xdg_config_home).filter(|path| path.is_absolute()) {
        (path.join("glim/config.json"), false)
    } else if let Some(path) = nonblank_path(home).filter(|path| path.is_absolute()) {
        (path.join(".config/glim/config.json"), false)
    } else {
        return Ok(None);
    };

    match read_bounded_configuration(&path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) if required && error.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "explicit configuration file does not exist: {}",
            path.display()
        )),
        Err(error) => Err(format!(
            "could not read daemon configuration {}: {error}",
            path.display()
        )),
    }
}

fn read_bounded_configuration(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "configuration path is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "daemon configuration exceeds 64 KiB",
        ));
    }
    Ok(bytes)
}

pub fn resolve_store_root_values(
    override_root: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<StoreRoot, String> {
    if let Some(path) = nonblank_path(override_root) {
        if !path.is_absolute() {
            return Err("GLIM_STORE_ROOT must be an absolute path".to_owned());
        }
        return Ok(StoreRoot {
            path,
            explicit_override: true,
        });
    }
    if override_root.is_some() {
        return Err("GLIM_STORE_ROOT must not be blank".to_owned());
    }
    if let Some(path) = nonblank_path(xdg_data_home).filter(|path| path.is_absolute()) {
        return Ok(StoreRoot {
            path: path.join("glim"),
            explicit_override: false,
        });
    }
    if let Some(path) = nonblank_path(home).filter(|path| path.is_absolute()) {
        return Ok(StoreRoot {
            path: path.join(".local/share/glim"),
            explicit_override: false,
        });
    }
    Err(
        "no usable data directory: set GLIM_STORE_ROOT for development, XDG_DATA_HOME, or HOME"
            .to_owned(),
    )
}

fn nonblank_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value.and_then(|value| {
        if value.to_string_lossy().trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(value))
        }
    })
}

pub fn resolve_store_root() -> Result<StoreRoot, String> {
    resolve_store_root_values(
        std::env::var_os("GLIM_STORE_ROOT").as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub fn open_store(root: StoreRoot) -> Result<Store, String> {
    let created = create_store_root_if_missing(&root.path)?;
    #[cfg(unix)]
    if created {
        use std::os::unix::fs::PermissionsExt;
        if let Err(error) = fs::set_permissions(&root.path, fs::Permissions::from_mode(0o700)) {
            let _ = fs::remove_dir(&root.path);
            return Err(format!(
                "could not secure new Glim store directory: {error}"
            ));
        }
    }
    Store::open(&root.path).map_err(|error| format!("could not open Glim store: {error}"))
}

fn create_store_root_if_missing(path: &std::path::Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!("could not inspect Glim store directory: {error}"));
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create Glim store parent directory: {error}"))?;
    }

    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    };
    #[cfg(not(unix))]
    let created = fs::create_dir(path);

    match created {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(format!("could not create Glim store directory: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn store_root_precedence_and_blank_values_are_deterministic() {
        let override_root = OsStr::new("/override");
        let xdg = OsStr::new("/xdg");
        let home = OsStr::new("/home/test");
        assert_eq!(
            resolve_store_root_values(Some(override_root), Some(xdg), Some(home)).unwrap(),
            StoreRoot {
                path: "/override".into(),
                explicit_override: true
            }
        );
        assert_eq!(
            resolve_store_root_values(None, Some(xdg), Some(home))
                .unwrap()
                .path,
            PathBuf::from("/xdg/glim")
        );
        assert_eq!(
            resolve_store_root_values(None, Some(OsStr::new("  ")), Some(home))
                .unwrap()
                .path,
            PathBuf::from("/home/test/.local/share/glim")
        );
        assert!(
            resolve_store_root_values(None, None, None)
                .unwrap_err()
                .contains("GLIM_STORE_ROOT")
        );
        assert!(resolve_store_root_values(Some(OsStr::new("")), None, None).is_err());
        assert!(resolve_store_root_values(Some(OsStr::new("relative")), None, None).is_err());
    }

    #[test]
    fn daemon_configuration_precedence_is_environment_then_file_then_defaults() {
        let file = br#"{"schema_version":1,"store_root":"/configured","bind":"127.0.0.1:4040"}"#;
        let configured = resolve_daemon_configuration_values(
            Some(file),
            Some(OsStr::new("/environment")),
            Some(OsStr::new("127.0.0.1:5050")),
            Some(OsStr::new("/xdg-data")),
            Some(OsStr::new("/home/test")),
        )
        .unwrap();
        assert_eq!(configured.store.path, PathBuf::from("/environment"));
        assert!(configured.store.explicit_override);
        assert_eq!(configured.bind, "127.0.0.1:5050".parse().unwrap());

        let from_file = resolve_daemon_configuration_values(
            Some(file),
            None,
            None,
            Some(OsStr::new("/xdg-data")),
            Some(OsStr::new("/home/test")),
        )
        .unwrap();
        assert_eq!(from_file.store.path, PathBuf::from("/configured"));
        assert_eq!(from_file.bind, "127.0.0.1:4040".parse().unwrap());

        let defaults = resolve_daemon_configuration_values(
            None,
            None,
            None,
            Some(OsStr::new("/xdg-data")),
            Some(OsStr::new("/home/test")),
        )
        .unwrap();
        assert_eq!(defaults.store.path, PathBuf::from("/xdg-data/glim"));
        assert_eq!(defaults.bind, "127.0.0.1:3030".parse().unwrap());
    }

    #[test]
    fn configuration_file_discovery_prefers_explicit_then_xdg_then_home() {
        let root = tempfile::tempdir().unwrap();
        let explicit = root.path().join("explicit.json");
        let xdg = root.path().join("xdg");
        let home = root.path().join("home");
        std::fs::create_dir_all(xdg.join("glim")).unwrap();
        std::fs::create_dir_all(home.join(".config/glim")).unwrap();
        std::fs::write(&explicit, b"explicit").unwrap();
        std::fs::write(xdg.join("glim/config.json"), b"xdg").unwrap();
        std::fs::write(home.join(".config/glim/config.json"), b"home").unwrap();

        assert_eq!(
            load_configuration_file(
                Some(explicit.as_os_str()),
                Some(xdg.as_os_str()),
                Some(home.as_os_str())
            )
            .unwrap(),
            Some(b"explicit".to_vec())
        );
        assert_eq!(
            load_configuration_file(None, Some(xdg.as_os_str()), Some(home.as_os_str())).unwrap(),
            Some(b"xdg".to_vec())
        );
        std::fs::remove_file(xdg.join("glim/config.json")).unwrap();
        assert_eq!(
            load_configuration_file(None, Some(xdg.as_os_str()), Some(home.as_os_str())).unwrap(),
            None
        );
        assert_eq!(
            load_configuration_file(None, None, Some(home.as_os_str())).unwrap(),
            Some(b"home".to_vec())
        );
        assert!(
            load_configuration_file(Some(root.path().join("missing").as_os_str()), None, None)
                .is_err()
        );
    }

    #[test]
    fn daemon_configuration_rejects_malformed_and_unsafe_values() {
        for (file, store, bind) in [
            (Some(br#"{}"#.as_slice()), None, None),
            (Some(br#"{"schema_version":2}"#.as_slice()), None, None),
            (
                Some(br#"{"schema_version":1,"unknown":true}"#.as_slice()),
                None,
                None,
            ),
            (
                Some(br#"{"schema_version":1,"store_root":"relative"}"#.as_slice()),
                None,
                None,
            ),
            (None, Some(OsStr::new(" ")), None),
            (None, None, Some(OsStr::new("0.0.0.0:3030"))),
            (None, None, Some(OsStr::new("127.0.0.1:0"))),
            (None, None, Some(OsStr::new("localhost:3030"))),
        ] {
            assert!(
                resolve_daemon_configuration_values(
                    file,
                    store,
                    bind,
                    Some(OsStr::new("/xdg-data")),
                    Some(OsStr::new("/home/test")),
                )
                .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_explicit_store_directory_is_private_with_permissive_umask() {
        const CHILD_ROOT: &str = "GLIM_TEST_EXPLICIT_ROOT";
        if let Some(path) = std::env::var_os(CHILD_ROOT) {
            let path = PathBuf::from(path);
            super::open_store(StoreRoot {
                path: path.clone(),
                explicit_override: true,
            })
            .unwrap();
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("explicit-new");
        let output = std::process::Command::new("sh")
            .args([
                "-c",
                "umask 0002; exec \"$@\"",
                "sh",
                std::env::current_exe().unwrap().to_str().unwrap(),
                "--exact",
                "daemon::tests::newly_created_explicit_store_directory_is_private_with_permissive_umask",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn default_store_directory_is_private_but_override_permissions_are_untouched() {
        let parent = tempfile::tempdir().unwrap();
        let default = StoreRoot {
            path: parent.path().join("default/glim"),
            explicit_override: false,
        };
        super::open_store(default.clone()).unwrap();
        assert_eq!(
            std::fs::metadata(&default.path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let explicit_path = parent.path().join("explicit");
        std::fs::create_dir(&explicit_path).unwrap();
        std::fs::set_permissions(&explicit_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        super::open_store(StoreRoot {
            path: explicit_path.clone(),
            explicit_override: true,
        })
        .unwrap();
        assert_eq!(
            std::fs::metadata(explicit_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
}
