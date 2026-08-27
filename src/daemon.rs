use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::json;

use crate::{
    logging::{LogLevel, daemon as log_daemon},
    storage::{LifecycleReport, Store},
};

const CONFIG_SCHEMA_VERSION: u32 = 1;
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
const DEFAULT_BIND: &str = "127.0.0.1:3030";
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 536_870_912;
pub const DEFAULT_MAX_FINALIZED_BLOB_BYTES: u64 = 21_474_836_480;
pub const RETENTION_SECONDS: u64 = 604_800;
pub const CLEANUP_INTERVAL_SECONDS: u64 = 3_600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonLimits {
    pub max_upload_bytes: u64,
    pub max_finalized_blob_bytes: u64,
}

impl Default for DaemonLimits {
    fn default() -> Self {
        Self {
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            max_finalized_blob_bytes: DEFAULT_MAX_FINALIZED_BLOB_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoot {
    pub path: PathBuf,
    pub explicit_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfiguration {
    pub store: StoreRoot,
    pub bind: SocketAddr,
    pub access: AccessConfiguration,
    pub limits: DaemonLimits,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessConfiguration {
    Local,
    Token(TokenAccessConfiguration),
    TrustedProxy(TrustedProxyAccessConfiguration),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProxyAccessConfiguration {
    pub trusted_proxy_ips: HashSet<IpAddr>,
    pub public_origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenAccessConfiguration {
    pub token_file: PathBuf,
    pub public_origin: String,
    pub tls: Option<TlsFiles>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsFiles {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken(String);

impl AccessToken {
    pub fn parse(value: &str) -> Result<Self, String> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(
                "access token must contain exactly 64 lowercase hexadecimal characters".to_owned(),
            );
        }
        Ok(Self(value.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AccessToken([REDACTED])")
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfiguration {
    schema_version: u32,
    store_root: Option<PathBuf>,
    bind: Option<String>,
    access: Option<FileAccessConfiguration>,
    limits: Option<FileLimits>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileLimits {
    max_upload_bytes: u64,
    max_finalized_blob_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum FileAccessConfiguration {
    Local,
    Token {
        token_file: PathBuf,
        public_origin: String,
        tls_certificate: Option<PathBuf>,
        tls_private_key: Option<PathBuf>,
    },
    TrustedProxy {
        trusted_proxy_ips: Vec<String>,
        public_origin: String,
    },
}

fn resolve_daemon_configuration_base_values(
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
    let limits = file
        .as_ref()
        .and_then(|configuration| configuration.limits.as_ref())
        .map_or_else(DaemonLimits::default, |limits| DaemonLimits {
            max_upload_bytes: limits.max_upload_bytes,
            max_finalized_blob_bytes: limits.max_finalized_blob_bytes,
        });
    let access = match file.and_then(|configuration| configuration.access) {
        None | Some(FileAccessConfiguration::Local) => AccessConfiguration::Local,
        Some(FileAccessConfiguration::Token {
            token_file,
            public_origin,
            tls_certificate,
            tls_private_key,
        }) => {
            require_absolute_path("access.token_file", &token_file)?;
            validate_public_origin(&public_origin)?;
            let tls = match (tls_certificate, tls_private_key) {
                (None, None) => None,
                (Some(certificate), Some(private_key)) => {
                    require_absolute_path("access.tls_certificate", &certificate)?;
                    require_absolute_path("access.tls_private_key", &private_key)?;
                    Some(TlsFiles {
                        certificate,
                        private_key,
                    })
                }
                _ => {
                    return Err(
                        "token access requires both tls_certificate and tls_private_key".to_owned(),
                    );
                }
            };
            AccessConfiguration::Token(TokenAccessConfiguration {
                token_file,
                public_origin,
                tls,
            })
        }
        Some(FileAccessConfiguration::TrustedProxy {
            trusted_proxy_ips,
            public_origin,
        }) => AccessConfiguration::TrustedProxy(TrustedProxyAccessConfiguration {
            trusted_proxy_ips: parse_trusted_proxy_ips(
                trusted_proxy_ips.iter().map(String::as_str),
            )?,
            public_origin: validate_public_origin(&public_origin).map(|()| public_origin)?,
        }),
    };
    Ok(DaemonConfiguration {
        store,
        bind,
        access,
        limits,
    })
}

pub fn resolve_daemon_configuration_values(
    file_bytes: Option<&[u8]>,
    store_override: Option<&OsStr>,
    bind_override: Option<&OsStr>,
    xdg_data_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<DaemonConfiguration, String> {
    let configuration = resolve_daemon_configuration_base_values(
        file_bytes,
        store_override,
        bind_override,
        xdg_data_home,
        home,
    )?;
    validate_limits(configuration.limits)?;
    validate_access_bind(&configuration.bind, &configuration.access)?;
    Ok(configuration)
}

fn validate_access_bind(bind: &SocketAddr, access: &AccessConfiguration) -> Result<(), String> {
    match access {
        AccessConfiguration::Local if !bind.ip().is_loopback() => {
            Err("non-loopback bind requires authenticated access".to_owned())
        }
        AccessConfiguration::Token(configuration)
            if !bind.ip().is_loopback() && configuration.tls.is_none() =>
        {
            Err("non-loopback token access requires configured TLS".to_owned())
        }
        AccessConfiguration::Token(configuration) => {
            let origin = configuration
                .public_origin
                .parse::<axum::http::Uri>()
                .map_err(|_| "token public origin is invalid".to_owned())?;
            let scheme = origin.scheme_str().unwrap_or_default();
            if !bind.ip().is_loopback() && scheme != "https" {
                return Err("non-loopback token access requires an HTTPS public origin".to_owned());
            }
            let default_port = if scheme == "https" { 443 } else { 80 };
            if origin
                .authority()
                .and_then(axum::http::uri::Authority::port_u16)
                .unwrap_or(default_port)
                != bind.port()
            {
                return Err("token public origin port must match the bind port".to_owned());
            }
            Ok(())
        }
        AccessConfiguration::TrustedProxy(configuration) => {
            let scheme = configuration
                .public_origin
                .parse::<axum::http::Uri>()
                .map_err(|_| "trusted-proxy public origin is invalid".to_owned())?
                .scheme_str()
                .unwrap_or_default()
                .to_owned();
            if !bind.ip().is_loopback() && scheme != "https" {
                return Err(
                    "non-loopback trusted-proxy access requires an HTTPS public origin".to_owned(),
                );
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn apply_access_environment_values(
    current: AccessConfiguration,
    mode: Option<&OsStr>,
    token_file: Option<&OsStr>,
    public_origin: Option<&OsStr>,
    tls_certificate: Option<&OsStr>,
    tls_private_key: Option<&OsStr>,
    trusted_proxy_ips: Option<&OsStr>,
) -> Result<AccessConfiguration, String> {
    let mode = mode
        .map(|value| {
            value
                .to_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "GLIM_ACCESS_MODE must be nonblank UTF-8".to_owned())
        })
        .transpose()?;
    if mode.is_some_and(|value| !matches!(value, "local" | "token" | "trusted_proxy")) {
        return Err("GLIM_ACCESS_MODE must be local, token, or trusted_proxy".to_owned());
    }
    let any_override = token_file.is_some()
        || public_origin.is_some()
        || tls_certificate.is_some()
        || tls_private_key.is_some()
        || trusted_proxy_ips.is_some();
    if mode.is_none() && !any_override {
        return Ok(current);
    }
    let selected = mode.unwrap_or(match current {
        AccessConfiguration::Local => "local",
        AccessConfiguration::Token(_) => "token",
        AccessConfiguration::TrustedProxy(_) => "trusted_proxy",
    });
    match selected {
        "local" => {
            if any_override {
                return Err("local access mode does not accept access overrides".to_owned());
            }
            Ok(AccessConfiguration::Local)
        }
        "token" => {
            if trusted_proxy_ips.is_some() {
                return Err("token access does not accept GLIM_TRUSTED_PROXY_IPS".to_owned());
            }
            let existing = match current {
                AccessConfiguration::Token(configuration) => Some(configuration),
                _ => None,
            };
            let token_file = environment_path("GLIM_TOKEN_FILE", token_file)?
                .or_else(|| existing.as_ref().map(|value| value.token_file.clone()))
                .ok_or_else(|| {
                    "token access requires GLIM_TOKEN_FILE or access.token_file".to_owned()
                })?;
            require_absolute_path("access.token_file", &token_file)?;
            let public_origin = environment_string("GLIM_PUBLIC_ORIGIN", public_origin)?
                .or_else(|| existing.as_ref().map(|value| value.public_origin.clone()))
                .ok_or_else(|| {
                    "token access requires GLIM_PUBLIC_ORIGIN or access.public_origin".to_owned()
                })?;
            validate_public_origin(&public_origin)?;
            let certificate =
                environment_path("GLIM_TLS_CERTIFICATE", tls_certificate)?.or_else(|| {
                    existing
                        .as_ref()?
                        .tls
                        .as_ref()
                        .map(|tls| tls.certificate.clone())
                });
            let private_key =
                environment_path("GLIM_TLS_PRIVATE_KEY", tls_private_key)?.or_else(|| {
                    existing
                        .as_ref()?
                        .tls
                        .as_ref()
                        .map(|tls| tls.private_key.clone())
                });
            let tls = match (certificate, private_key) {
                (None, None) => None,
                (Some(certificate), Some(private_key)) => {
                    require_absolute_path("access.tls_certificate", &certificate)?;
                    require_absolute_path("access.tls_private_key", &private_key)?;
                    Some(TlsFiles {
                        certificate,
                        private_key,
                    })
                }
                _ => return Err("token access requires both TLS path overrides".to_owned()),
            };
            Ok(AccessConfiguration::Token(TokenAccessConfiguration {
                token_file,
                public_origin,
                tls,
            }))
        }
        "trusted_proxy" => {
            if token_file.is_some() || tls_certificate.is_some() || tls_private_key.is_some() {
                return Err("trusted-proxy access does not accept token or TLS settings".to_owned());
            }
            let existing = match current {
                AccessConfiguration::TrustedProxy(configuration) => Some(configuration),
                _ => None,
            };
            let trusted_proxy_ips = match environment_string(
                "GLIM_TRUSTED_PROXY_IPS",
                trusted_proxy_ips,
            )? {
                Some(value) => parse_trusted_proxy_ips(value.split(',').map(str::trim))?,
                None => existing
                    .as_ref()
                    .map(|value| value.trusted_proxy_ips.clone())
                    .ok_or_else(|| {
                        "trusted-proxy access requires GLIM_TRUSTED_PROXY_IPS or access.trusted_proxy_ips"
                            .to_owned()
                    })?,
            };
            let public_origin = environment_string("GLIM_PUBLIC_ORIGIN", public_origin)?
                .or_else(|| existing.as_ref().map(|value| value.public_origin.clone()))
                .ok_or_else(|| {
                    "trusted-proxy access requires GLIM_PUBLIC_ORIGIN or access.public_origin"
                        .to_owned()
                })?;
            validate_public_origin(&public_origin)?;
            Ok(AccessConfiguration::TrustedProxy(
                TrustedProxyAccessConfiguration {
                    trusted_proxy_ips,
                    public_origin,
                },
            ))
        }
        _ => unreachable!("access mode was validated"),
    }
}

fn environment_string(name: &str, value: Option<&OsStr>) -> Result<Option<String>, String> {
    value
        .map(|value| {
            value
                .to_str()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| format!("{name} must be nonblank UTF-8"))
        })
        .transpose()
}

fn parse_trusted_proxy_ips<'a>(
    values: impl IntoIterator<Item = &'a str>,
) -> Result<HashSet<IpAddr>, String> {
    let mut parsed = HashSet::new();
    for value in values {
        if value.is_empty() {
            return Err("trusted proxy IPs must not contain blank entries".to_owned());
        }
        parsed.insert(value.parse::<IpAddr>().map_err(|_| {
            "trusted proxy IPs must be exact numeric IPv4 or IPv6 addresses".to_owned()
        })?);
    }
    if parsed.is_empty() {
        return Err("trusted proxy access requires at least one proxy IP".to_owned());
    }
    Ok(parsed)
}

fn environment_path(name: &str, value: Option<&OsStr>) -> Result<Option<PathBuf>, String> {
    value
        .map(|value| {
            let path = PathBuf::from(value);
            if path.as_os_str().to_string_lossy().trim().is_empty() {
                Err(format!("{name} must not be blank"))
            } else {
                Ok(path)
            }
        })
        .transpose()
}

fn validate_public_origin(value: &str) -> Result<(), String> {
    let origin = value
        .parse::<axum::http::Uri>()
        .map_err(|_| "public_origin must be an HTTP or HTTPS origin".to_owned())?;
    let scheme = origin.scheme_str().unwrap_or_default();
    let authority = origin
        .authority()
        .ok_or_else(|| "public_origin must include an authority".to_owned())?;
    if !matches!(scheme, "http" | "https")
        || authority.as_str().contains('@')
        || value != format!("{scheme}://{authority}")
    {
        return Err("public_origin must contain only an HTTP or HTTPS origin".to_owned());
    }
    Ok(())
}

fn require_absolute_path(field: &str, path: &Path) -> Result<(), String> {
    if path.as_os_str().to_string_lossy().trim().is_empty() || !path.is_absolute() {
        Err(format!(
            "daemon configuration {field} must be an absolute path"
        ))
    } else {
        Ok(())
    }
}

pub fn load_access_token(path: &Path) -> Result<AccessToken, String> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = fs::File::open(path);
    let file = opened.map_err(|error| format!("could not open access token file: {error}"))?;
    read_access_token(file)
}

pub fn load_or_create_access_token(path: &Path) -> Result<AccessToken, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => return load_access_token(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("could not inspect access token file: {error}")),
    }
    let parent = path
        .parent()
        .ok_or_else(|| "access token file has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create access token directory: {error}"))?;
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| "could not generate access token securely".to_owned())?;
    let mut token = String::with_capacity(64);
    use std::fmt::Write as _;
    for byte in random {
        write!(&mut token, "{byte:02x}").expect("writing to a String cannot fail");
    }

    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path);
    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return load_access_token(path);
        }
        Err(error) => return Err(format!("could not create access token file: {error}")),
    };
    if let Err(error) = file
        .write_all(token.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(format!("could not persist access token: {error}"));
    }
    Ok(AccessToken(token))
}

fn read_access_token(file: fs::File) -> Result<AccessToken, String> {
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect access token file: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("access token path must be a regular non-symlink file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "access token file permissions must not grant group or other access".to_owned(),
            );
        }
    }
    let mut token = String::new();
    file.take(65)
        .read_to_string(&mut token)
        .map_err(|error| format!("could not read access token file: {error}"))?;
    AccessToken::parse(&token).map_err(|_| {
        "access token file must contain exactly 64 lowercase hexadecimal characters".to_owned()
    })
}

pub fn resolve_client_access_token() -> Result<Option<AccessToken>, String> {
    match resolve_daemon_configuration()?.access {
        AccessConfiguration::Local | AccessConfiguration::TrustedProxy(_) => Ok(None),
        AccessConfiguration::Token(configuration) => {
            load_access_token(&configuration.token_file).map(Some)
        }
    }
}

fn parse_limit_environment(name: &str, value: Option<&OsStr>) -> Result<Option<u64>, String> {
    value
        .map(|value| {
            let value = value
                .to_str()
                .ok_or_else(|| format!("{name} must be decimal bytes"))?;
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(format!("{name} must be nonzero decimal bytes"));
            }
            let parsed = value
                .parse::<u64>()
                .map_err(|_| format!("{name} exceeds the supported byte range"))?;
            if parsed == 0 {
                return Err(format!("{name} must be greater than zero"));
            }
            Ok(parsed)
        })
        .transpose()
}

fn validate_limits(limits: DaemonLimits) -> Result<(), String> {
    if limits.max_upload_bytes == 0 || limits.max_finalized_blob_bytes == 0 {
        return Err("daemon limits must be greater than zero".to_owned());
    }
    if limits.max_upload_bytes > limits.max_finalized_blob_bytes {
        return Err("max_upload_bytes must not exceed max_finalized_blob_bytes".to_owned());
    }
    Ok(())
}

pub fn resolve_daemon_configuration_limit_values(
    file_bytes: Option<&[u8]>,
    upload_override: Option<&OsStr>,
    finalized_override: Option<&OsStr>,
) -> Result<DaemonLimits, String> {
    let file = file_bytes
        .map(|bytes| {
            serde_json::from_slice::<FileConfiguration>(bytes)
                .map_err(|error| format!("daemon configuration is malformed: {error}"))
        })
        .transpose()?;
    if file
        .as_ref()
        .is_some_and(|value| value.schema_version != CONFIG_SCHEMA_VERSION)
    {
        return Err(format!(
            "daemon configuration schema_version must be {CONFIG_SCHEMA_VERSION}"
        ));
    }
    let from_file = file
        .and_then(|value| value.limits)
        .map(|limits| DaemonLimits {
            max_upload_bytes: limits.max_upload_bytes,
            max_finalized_blob_bytes: limits.max_finalized_blob_bytes,
        })
        .unwrap_or_default();
    let limits = DaemonLimits {
        max_upload_bytes: parse_limit_environment("GLIM_MAX_UPLOAD_BYTES", upload_override)?
            .unwrap_or(from_file.max_upload_bytes),
        max_finalized_blob_bytes: parse_limit_environment(
            "GLIM_MAX_FINALIZED_BLOB_BYTES",
            finalized_override,
        )?
        .unwrap_or(from_file.max_finalized_blob_bytes),
    };
    validate_limits(limits)?;
    Ok(limits)
}

pub fn cleanup_cutoff(now: std::time::SystemTime) -> Result<i64, String> {
    let seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_secs();
    let cutoff = seconds
        .checked_sub(RETENTION_SECONDS)
        .ok_or_else(|| "system clock predates the seven-day retention window".to_owned())?;
    i64::try_from(cutoff).map_err(|_| "system clock exceeds signed Unix seconds".to_owned())
}

pub fn resolve_daemon_configuration() -> Result<DaemonConfiguration, String> {
    let file_bytes = load_configuration_file(
        std::env::var_os("GLIM_CONFIG").as_deref(),
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    let mut configuration = resolve_daemon_configuration_base_values(
        file_bytes.as_deref(),
        std::env::var_os("GLIM_STORE_ROOT").as_deref(),
        std::env::var_os("GLIM_BIND").as_deref(),
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )?;
    configuration.limits = DaemonLimits {
        max_upload_bytes: parse_limit_environment(
            "GLIM_MAX_UPLOAD_BYTES",
            std::env::var_os("GLIM_MAX_UPLOAD_BYTES").as_deref(),
        )?
        .unwrap_or(configuration.limits.max_upload_bytes),
        max_finalized_blob_bytes: parse_limit_environment(
            "GLIM_MAX_FINALIZED_BLOB_BYTES",
            std::env::var_os("GLIM_MAX_FINALIZED_BLOB_BYTES").as_deref(),
        )?
        .unwrap_or(configuration.limits.max_finalized_blob_bytes),
    };
    validate_limits(configuration.limits)?;
    configuration.access = apply_access_environment_values(
        configuration.access,
        std::env::var_os("GLIM_ACCESS_MODE").as_deref(),
        std::env::var_os("GLIM_TOKEN_FILE").as_deref(),
        std::env::var_os("GLIM_PUBLIC_ORIGIN").as_deref(),
        std::env::var_os("GLIM_TLS_CERTIFICATE").as_deref(),
        std::env::var_os("GLIM_TLS_PRIVATE_KEY").as_deref(),
        std::env::var_os("GLIM_TRUSTED_PROXY_IPS").as_deref(),
    )?;
    validate_access_bind(&configuration.bind, &configuration.access)?;
    Ok(configuration)
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

pub fn open_store(root: StoreRoot, limits: DaemonLimits) -> Result<Store, String> {
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
    Store::open_with_limits(
        &root.path,
        crate::storage::StoreLimits {
            max_upload_bytes: limits.max_upload_bytes,
            max_finalized_blob_bytes: limits.max_finalized_blob_bytes,
        },
    )
    .map_err(|error| format!("could not open Glim store: {error}"))
}

pub fn purge_expired_sessions(
    store: &mut Store,
    now: std::time::SystemTime,
) -> Result<LifecycleReport, String> {
    let cutoff = cleanup_cutoff(now)?;
    store
        .purge_inactive_sessions(cutoff)
        .map_err(|error| format!("could not purge inactive sessions: {error}"))
}

pub fn log_cleanup_completed(trigger: &'static str, report: LifecycleReport) {
    log_daemon(
        LogLevel::Info,
        "cleanup_completed",
        &lifecycle_fields(trigger, report),
    );
}

fn lifecycle_fields(
    trigger: &'static str,
    report: LifecycleReport,
) -> Vec<(&'static str, serde_json::Value)> {
    vec![
        ("trigger", json!(trigger)),
        ("sessions_deleted", json!(report.sessions_deleted)),
        ("projects_deleted", json!(report.projects_deleted)),
        ("posts_deleted", json!(report.posts_deleted)),
        ("post_files_deleted", json!(report.post_files_deleted)),
        (
            "support_assets_deleted",
            json!(report.support_assets_deleted),
        ),
        (
            "blob_references_deleted",
            json!(report.blob_references_deleted),
        ),
        ("blobs_queued", json!(report.blobs_queued)),
        ("blobs_deleted", json!(report.blobs_deleted)),
    ]
}

pub fn spawn_periodic_cleanup(root: StoreRoot, limits: DaemonLimits) {
    drop(spawn_periodic_cleanup_with_interval(
        root,
        limits,
        std::time::Duration::from_secs(CLEANUP_INTERVAL_SECONDS),
    ));
}

#[doc(hidden)]
pub fn spawn_periodic_cleanup_with_interval(
    root: StoreRoot,
    limits: DaemonLimits,
    cleanup_interval: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(cleanup_interval);
        interval.tick().await;
        loop {
            interval.tick().await;
            let root = root.clone();
            let result = tokio::task::spawn_blocking(move || {
                let mut store = open_store(root, limits)?;
                purge_expired_sessions(&mut store, std::time::SystemTime::now())
            })
            .await;
            // Cleanup failures are intentionally retried on the next tick. Due-session and
            // queued-deletion counts remain visible through authenticated daemon status.
            match result {
                Ok(Ok(report)) => log_cleanup_completed("periodic", report),
                Ok(Err(_)) => log_daemon(
                    LogLevel::Error,
                    "cleanup_failed",
                    &[
                        ("trigger", json!("periodic")),
                        ("category", json!("cleanup_operation_failed")),
                    ],
                ),
                Err(_) => log_daemon(
                    LogLevel::Error,
                    "cleanup_failed",
                    &[
                        ("trigger", json!("periodic")),
                        ("category", json!("cleanup_worker_failed")),
                    ],
                ),
            }
        }
    })
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

    #[test]
    fn token_mode_requires_absolute_credentials_and_tls_for_non_loopback() {
        let configured = resolve_daemon_configuration_values(
            Some(br#"{"schema_version":1,"bind":"0.0.0.0:3443","access":{"mode":"token","token_file":"/secrets/token","public_origin":"https://glim.example:3443","tls_certificate":"/secrets/cert.pem","tls_private_key":"/secrets/key.pem"}}"#),
            None,
            None,
            Some(OsStr::new("/xdg-data")),
            Some(OsStr::new("/home/test")),
        )
        .unwrap();
        assert_eq!(configured.bind, "0.0.0.0:3443".parse().unwrap());
        assert_eq!(
            configured.access,
            AccessConfiguration::Token(TokenAccessConfiguration {
                token_file: PathBuf::from("/secrets/token"),
                public_origin: "https://glim.example:3443".into(),
                tls: Some(TlsFiles {
                    certificate: PathBuf::from("/secrets/cert.pem"),
                    private_key: PathBuf::from("/secrets/key.pem"),
                }),
            })
        );

        for file in [
            br#"{"schema_version":1,"bind":"0.0.0.0:3443","access":{"mode":"local"}}"#.as_slice(),
            br#"{"schema_version":1,"bind":"0.0.0.0:3443","access":{"mode":"token","token_file":"/token","public_origin":"https://glim.example:3443"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"token","token_file":"relative","public_origin":"http://127.0.0.1:3030"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"token","token_file":"/token","public_origin":"http://127.0.0.1:3030","tls_certificate":"/cert"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"token","token_file":"/token"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"token","token_file":"/token","public_origin":"https://glim.example/path"}}"#.as_slice(),
        ] {
            assert!(
                resolve_daemon_configuration_values(
                    Some(file),
                    None,
                    None,
                    Some(OsStr::new("/xdg-data")),
                    Some(OsStr::new("/home/test")),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn trusted_proxy_configuration_requires_exact_ips_and_https_for_non_loopback() {
        let configured = resolve_daemon_configuration_values(
            Some(br#"{"schema_version":1,"bind":"0.0.0.0:3030","access":{"mode":"trusted_proxy","trusted_proxy_ips":["127.0.0.1","::1"],"public_origin":"https://glim.example"}}"#),
            None,
            None,
            Some(OsStr::new("/xdg-data")),
            Some(OsStr::new("/home/test")),
        )
        .unwrap();
        assert_eq!(
            configured.access,
            AccessConfiguration::TrustedProxy(TrustedProxyAccessConfiguration {
                trusted_proxy_ips: ["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()]
                    .into_iter()
                    .collect(),
                public_origin: "https://glim.example".into(),
            })
        );

        for file in [
            br#"{"schema_version":1,"access":{"mode":"trusted_proxy","trusted_proxy_ips":[],"public_origin":"http://127.0.0.1:3030"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"trusted_proxy","trusted_proxy_ips":["proxy.example"],"public_origin":"http://127.0.0.1:3030"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"trusted_proxy","trusted_proxy_ips":["127.0.0.0/8"],"public_origin":"http://127.0.0.1:3030"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"trusted_proxy","trusted_proxy_ips":["127.0.0.1"]}}"#.as_slice(),
            br#"{"schema_version":1,"bind":"0.0.0.0:3030","access":{"mode":"trusted_proxy","trusted_proxy_ips":["127.0.0.1"],"public_origin":"http://glim.example:3030"}}"#.as_slice(),
            br#"{"schema_version":1,"access":{"mode":"trusted_proxy","trusted_proxy_ips":["127.0.0.1"],"public_origin":"http://127.0.0.1:3030","token_file":"/token"}}"#.as_slice(),
        ] {
            assert!(resolve_daemon_configuration_values(
                Some(file),
                None,
                None,
                Some(OsStr::new("/xdg-data")),
                Some(OsStr::new("/home/test")),
            )
            .is_err());
        }
    }

    #[test]
    fn trusted_proxy_environment_overrides_file_and_rejects_unrelated_settings() {
        let current = resolve_daemon_configuration_values(
            Some(br#"{"schema_version":1,"access":{"mode":"trusted_proxy","trusted_proxy_ips":["127.0.0.2"],"public_origin":"http://127.0.0.1:3030"}}"#),
            None,
            None,
            Some(OsStr::new("/xdg-data")),
            Some(OsStr::new("/home/test")),
        )
        .unwrap()
        .access;
        let access = apply_access_environment_values(
            current,
            Some(OsStr::new("trusted_proxy")),
            None,
            Some(OsStr::new("https://glim.example")),
            None,
            None,
            Some(OsStr::new("127.0.0.1, ::1")),
        )
        .unwrap();
        assert!(matches!(
            access,
            AccessConfiguration::TrustedProxy(TrustedProxyAccessConfiguration { ref trusted_proxy_ips, ref public_origin })
                if trusted_proxy_ips.len() == 2 && public_origin == "https://glim.example"
        ));

        for (mode, token, certificate, proxies) in [
            (
                Some(OsStr::new("trusted_proxy")),
                Some(OsStr::new("/token")),
                None,
                Some(OsStr::new("127.0.0.1")),
            ),
            (
                Some(OsStr::new("trusted_proxy")),
                None,
                Some(OsStr::new("/cert")),
                Some(OsStr::new("127.0.0.1")),
            ),
            (
                Some(OsStr::new("token")),
                None,
                None,
                Some(OsStr::new("127.0.0.1")),
            ),
            (
                Some(OsStr::new("trusted_proxy")),
                None,
                None,
                Some(OsStr::new("")),
            ),
            (
                Some(OsStr::new("trusted_proxy")),
                None,
                None,
                Some(OsStr::new("127.0.0.1,localhost")),
            ),
        ] {
            assert!(
                apply_access_environment_values(
                    AccessConfiguration::Local,
                    mode,
                    token,
                    Some(OsStr::new("https://glim.example")),
                    certificate,
                    None,
                    proxies,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn token_environment_overrides_file_access_before_bind_validation() {
        let access = apply_access_environment_values(
            AccessConfiguration::Local,
            Some(OsStr::new("token")),
            Some(OsStr::new("/environment/token")),
            Some(OsStr::new("https://glim.example:3443")),
            Some(OsStr::new("/environment/cert.pem")),
            Some(OsStr::new("/environment/key.pem")),
            None,
        )
        .unwrap();
        assert!(matches!(access, AccessConfiguration::Token(_)));
        validate_access_bind(&"0.0.0.0:3443".parse().unwrap(), &access).unwrap();

        for (mode, token, origin, certificate, key) in [
            (Some(OsStr::new("unknown")), None, None, None, None),
            (Some(OsStr::new("token")), None, None, None, None),
            (
                Some(OsStr::new("local")),
                Some(OsStr::new("/token")),
                None,
                None,
                None,
            ),
            (
                Some(OsStr::new("token")),
                Some(OsStr::new("relative")),
                Some(OsStr::new("http://127.0.0.1:3030")),
                None,
                None,
            ),
            (
                Some(OsStr::new("token")),
                Some(OsStr::new("/token")),
                Some(OsStr::new("http://127.0.0.1:3030")),
                Some(OsStr::new("/cert")),
                None,
            ),
        ] {
            assert!(
                apply_access_environment_values(
                    AccessConfiguration::Local,
                    mode,
                    token,
                    origin,
                    certificate,
                    key,
                    None,
                )
                .is_err()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn access_token_is_private_persistent_and_strictly_validated() {
        let root = tempfile::tempdir().unwrap();
        let token_path = root.path().join("access-token");
        let first = load_or_create_access_token(&token_path).unwrap();
        assert_eq!(first.expose().len(), 64);
        assert!(first.expose().bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(load_or_create_access_token(&token_path).unwrap(), first);

        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_or_create_access_token(&token_path).is_err());
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&token_path, b"short").unwrap();
        assert!(load_or_create_access_token(&token_path).is_err());

        let target = root.path().join("target");
        std::fs::write(&target, "a".repeat(64)).unwrap();
        let symlink = root.path().join("symlink-token");
        std::os::unix::fs::symlink(target, &symlink).unwrap();
        assert!(load_or_create_access_token(&symlink).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_explicit_store_directory_is_private_with_permissive_umask() {
        const CHILD_ROOT: &str = "GLIM_TEST_EXPLICIT_ROOT";
        if let Some(path) = std::env::var_os(CHILD_ROOT) {
            let path = PathBuf::from(path);
            super::open_store(
                StoreRoot {
                    path: path.clone(),
                    explicit_override: true,
                },
                DaemonLimits::default(),
            )
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
        super::open_store(default.clone(), DaemonLimits::default()).unwrap();
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
        super::open_store(
            StoreRoot {
                path: explicit_path.clone(),
                explicit_override: true,
            },
            DaemonLimits::default(),
        )
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
