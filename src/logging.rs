use std::{
    ffi::OsStr,
    io::Write,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Map, Value, json};

const MAX_LINE_BYTES: usize = 4096;
const MAX_STRING_FIELD_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidLogLevel;

impl LogLevel {
    pub fn parse(value: Option<&OsStr>) -> Result<Self, InvalidLogLevel> {
        match value.map(OsStr::to_str) {
            None => Ok(Self::Info),
            Some(Some("error")) => Ok(Self::Error),
            Some(Some("warn")) => Ok(Self::Warn),
            Some(Some("info")) => Ok(Self::Info),
            _ => Err(InvalidLogLevel),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }

    fn permits(self, event: Self) -> bool {
        match self {
            Self::Error => event == Self::Error,
            Self::Warn => event != Self::Info,
            Self::Info => true,
        }
    }
}

#[derive(Clone)]
pub struct Logger {
    level: LogLevel,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl Logger {
    pub fn new(level: LogLevel, writer: impl Write + Send + 'static) -> Self {
        Self {
            level,
            writer: Arc::new(Mutex::new(Box::new(writer))),
        }
    }

    pub fn emit(&self, level: LogLevel, event: &str, fields: &[(&str, Value)]) {
        if !self.level.permits(level) {
            return;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_secs());
        let mut object = Map::from_iter([
            ("schema_version".to_owned(), json!(1)),
            ("timestamp".to_owned(), json!(timestamp)),
            ("level".to_owned(), json!(level.name())),
            ("event".to_owned(), json!(bounded_string(event))),
        ]);
        for (key, value) in fields {
            let value = match value {
                Value::String(value) => Value::String(bounded_string(value)),
                value => value.clone(),
            };
            object.insert((*key).to_owned(), value);
        }
        let mut line = serde_json::to_vec(&Value::Object(object)).unwrap_or_else(|_| {
            br#"{"schema_version":1,"timestamp":0,"level":"error","event":"logger_error"}"#.to_vec()
        });
        if line.len() + 1 > MAX_LINE_BYTES {
            line = br#"{"schema_version":1,"timestamp":0,"level":"error","event":"logger_error"}"#
                .to_vec();
        }
        line.push(b'\n');
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writer.write_all(&line);
        let _ = writer.flush();
    }
}

fn bounded_string(value: &str) -> String {
    if value.len() <= MAX_STRING_FIELD_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_STRING_FIELD_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

static DAEMON_LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn initialize_daemon(level: LogLevel) {
    initialize_daemon_with_writer(level, std::io::stderr());
}

#[doc(hidden)]
pub fn initialize_daemon_with_writer(level: LogLevel, writer: impl Write + Send + 'static) {
    let _ = DAEMON_LOGGER.set(Logger::new(level, writer));
}

pub fn daemon(level: LogLevel, event: &str, fields: &[(&str, Value)]) {
    if let Some(logger) = DAEMON_LOGGER.get() {
        logger.emit(level, event, fields);
    }
}
