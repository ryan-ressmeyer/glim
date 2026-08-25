use std::{ffi::OsStr, fs, path::PathBuf};

use crate::storage::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoot {
    pub path: PathBuf,
    pub explicit_override: bool,
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
