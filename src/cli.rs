use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
};

use cssparser::{Parser as CssParser, ParserInput, Token};
use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Parser, Tag};
use scraper::{Html, Selector};

const MAX_REFERENCES: usize = 512;
const MAX_CSS_RECURSION_DEPTH: usize = 8;

#[derive(Debug)]
pub struct CollectedSupportAsset {
    pub relative_path: String,
    pub canonical_path: PathBuf,
    pub file: File,
}

pub fn collect_support_assets(entry: &Path) -> Result<Vec<CollectedSupportAsset>, String> {
    let canonical_entry = fs::canonicalize(entry)
        .map_err(|error| format!("could not resolve entry file: {error}"))?;
    let extension = canonical_entry
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !matches!(extension.as_str(), "md" | "markdown" | "html" | "htm") {
        return Ok(vec![]);
    }
    let root = canonical_entry
        .parent()
        .ok_or_else(|| "entry file has no parent directory".to_owned())?
        .to_owned();
    let mut text = String::new();
    File::open(&canonical_entry)
        .map_err(|error| format!("could not open entry file: {error}"))?
        .read_to_string(&mut text)
        .map_err(|error| format!("entry file must be UTF-8 text: {error}"))?;
    let references = match extension.as_str() {
        "md" | "markdown" => markdown_references(&text)?,
        "html" | "htm" => html_references(&text)?,
        _ => unreachable!("entry extension was checked above"),
    };
    resolve_references(&root, references)
}

fn markdown_references(text: &str) -> Result<Vec<String>, String> {
    let mut references = Vec::new();
    for event in Parser::new(text) {
        match event {
            Event::Start(Tag::Image { dest_url, .. }) => references.push(dest_url.into_string()),
            Event::Html(html) | Event::InlineHtml(html) => {
                references.extend(html_references(&html)?);
            }
            _ => {}
        }
        if references.len() > MAX_REFERENCES {
            return Err("entry document exceeds the support-reference limit".into());
        }
    }
    Ok(references)
}

fn html_references(text: &str) -> Result<Vec<String>, String> {
    let document = Html::parse_fragment(text);
    let selector =
        Selector::parse("*").map_err(|_| "could not initialize HTML parser".to_owned())?;
    let mut references = Vec::new();
    for element in document.select(&selector) {
        let name = element.value().name();
        let attributes = element.value();
        if matches!(
            name,
            "img" | "script" | "source" | "video" | "audio" | "track" | "iframe" | "input"
        ) && let Some(src) = attributes.attr("src")
        {
            references.push(src.to_owned());
        }
        if name == "video"
            && let Some(poster) = attributes.attr("poster")
        {
            references.push(poster.to_owned());
        }
        if matches!(name, "img" | "source")
            && let Some(srcset) = attributes.attr("srcset")
        {
            if srcset
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("data:")
            {
                continue;
            }
            for candidate in srcset.split(',') {
                if let Some(url) = candidate
                    .split_ascii_whitespace()
                    .next()
                    .filter(|url| !url.is_empty())
                {
                    references.push(url.to_owned());
                }
            }
        }
        if name == "link"
            && attributes.attr("rel").is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|part| matches!(part.to_ascii_lowercase().as_str(), "stylesheet" | "icon"))
            })
            && let Some(href) = attributes.attr("href")
        {
            references.push(href.to_owned());
        }
        if references.len() > MAX_REFERENCES {
            return Err("entry document exceeds the support-reference limit".into());
        }
    }
    Ok(references)
}

fn resolve_references(
    root: &Path,
    references: Vec<String>,
) -> Result<Vec<CollectedSupportAsset>, String> {
    let mut collector = AssetCollector {
        root,
        seen: HashSet::new(),
        assets: Vec::new(),
        references_seen: 0,
    };
    for reference in references {
        collector.collect(&reference, Path::new(""), 0)?;
    }
    Ok(collector.assets)
}

struct AssetCollector<'a> {
    root: &'a Path,
    seen: HashSet<String>,
    assets: Vec<CollectedSupportAsset>,
    references_seen: usize,
}

impl AssetCollector<'_> {
    fn collect(
        &mut self,
        reference: &str,
        base_directory: &Path,
        css_depth: usize,
    ) -> Result<(), String> {
        self.references_seen += 1;
        if self.references_seen > MAX_REFERENCES {
            return Err("entry document exceeds the support-reference limit".into());
        }
        let Some(relative_path) = normalize_local_reference_from(reference, base_directory)? else {
            return Ok(());
        };
        if !self.seen.insert(relative_path.clone()) {
            return Ok(());
        }
        if self.assets.len() >= 255 {
            return Err("entry document declares too many support assets".into());
        }
        ensure_supported_dependency(&relative_path)?;
        let canonical_path = fs::canonicalize(self.root.join(&relative_path))
            .map_err(|error| format!("could not resolve support asset {relative_path}: {error}"))?;
        if !canonical_path.starts_with(self.root) {
            return Err(format!(
                "support asset escapes the entry directory: {relative_path}"
            ));
        }
        let path_metadata = fs::metadata(&canonical_path)
            .map_err(|error| format!("could not inspect support asset {relative_path}: {error}"))?;
        if !path_metadata.is_file() {
            return Err(format!(
                "support asset is not a regular file: {relative_path}"
            ));
        }
        let file = File::open(&canonical_path)
            .map_err(|error| format!("could not open support asset {relative_path}: {error}"))?;
        if !file
            .metadata()
            .map_err(|error| {
                format!("could not inspect opened support asset {relative_path}: {error}")
            })?
            .is_file()
        {
            return Err(format!(
                "support asset changed while opening: {relative_path}"
            ));
        }
        let css_references = if Path::new(&relative_path)
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("css"))
        {
            if css_depth >= MAX_CSS_RECURSION_DEPTH {
                return Err("CSS dependency recursion exceeds the supported depth".into());
            }
            let mut css = String::new();
            File::open(&canonical_path)
                .and_then(|mut css_file| css_file.read_to_string(&mut css))
                .map_err(|error| format!("CSS support asset must be UTF-8 text: {error}"))?;
            Some(css_references(&css)?)
        } else {
            None
        };
        let css_base = Path::new(&relative_path)
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .to_owned();
        self.assets.push(CollectedSupportAsset {
            relative_path,
            canonical_path,
            file,
        });
        if let Some(references) = css_references {
            for reference in references {
                self.collect(&reference, &css_base, css_depth + 1)?;
            }
        }
        Ok(())
    }
}

fn css_references(text: &str) -> Result<Vec<String>, String> {
    let mut input = ParserInput::new(text);
    let mut parser = CssParser::new(&mut input);
    let mut references = Vec::new();
    let mut import_pending = false;
    scan_css(&mut parser, &mut references, &mut import_pending)
        .map_err(|_| "CSS support asset contains malformed syntax".to_owned())?;
    Ok(references)
}

fn scan_css<'i, 't>(
    parser: &mut CssParser<'i, 't>,
    references: &mut Vec<String>,
    import_pending: &mut bool,
) -> Result<(), cssparser::ParseError<'i, ()>> {
    loop {
        let token = match parser.next_including_whitespace_and_comments() {
            Ok(token) => token.clone(),
            Err(_) => return Ok(()),
        };
        match token {
            Token::AtKeyword(name) if name.eq_ignore_ascii_case("import") => {
                *import_pending = true;
            }
            Token::UnquotedUrl(value) => {
                references.push(value.to_string());
                *import_pending = false;
            }
            Token::QuotedString(value) if *import_pending => {
                references.push(value.to_string());
                *import_pending = false;
            }
            Token::Function(name) if name.eq_ignore_ascii_case("url") => {
                let value = parser.parse_nested_block(|nested| {
                    nested
                        .expect_string_cloned()
                        .or_else(|_| nested.expect_ident_cloned())
                        .map(|value| value.to_string())
                        .map_err(Into::into)
                })?;
                references.push(value);
                *import_pending = false;
            }
            Token::Function(_)
            | Token::ParenthesisBlock
            | Token::SquareBracketBlock
            | Token::CurlyBracketBlock => {
                parser.parse_nested_block(|nested| {
                    let mut nested_import = false;
                    scan_css(nested, references, &mut nested_import)
                })?;
            }
            Token::BadUrl(_) | Token::BadString(_) => {
                return Err(parser.new_custom_error(()));
            }
            Token::Semicolon => *import_pending = false,
            Token::WhiteSpace(_) | Token::Comment(_) => {}
            _ if *import_pending => *import_pending = false,
            _ => {}
        }
        if references.len() > MAX_REFERENCES {
            return Err(parser.new_custom_error(()));
        }
    }
}

fn normalize_local_reference_from(
    reference: &str,
    base_directory: &Path,
) -> Result<Option<String>, String> {
    let value = reference.trim();
    if value.is_empty() || value.starts_with('#') || value.starts_with("//") {
        return Ok(None);
    }
    if let Some(index) = value.find(':')
        && index > 0
        && value[..index].bytes().enumerate().all(|(offset, byte)| {
            if offset == 0 {
                byte.is_ascii_alphabetic()
            } else {
                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
            }
        })
    {
        let scheme = &value[..index];
        if scheme.eq_ignore_ascii_case("file") || scheme.len() == 1 {
            return Err("absolute filesystem support references are not allowed".into());
        }
        return Ok(None);
    }
    let path = value.split(['?', '#']).next().unwrap_or_default();
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2f") || lower.contains("%5c") {
        return Err("encoded path separators are not allowed in support references".into());
    }
    let decoded = percent_decode_str(path)
        .decode_utf8()
        .map_err(|_| "support reference has malformed UTF-8 percent encoding".to_owned())?;
    if has_malformed_percent(path) {
        return Err("support reference has malformed percent encoding".into());
    }
    if decoded.starts_with('/') || decoded.starts_with('\\') || decoded.contains('\\') {
        return Err("absolute or backslash support paths are not allowed".into());
    }
    let parsed = Path::new(decoded.as_ref());
    let mut segments = base_directory
        .components()
        .filter_map(|component| match component {
            Component::Normal(segment) => segment.to_str().map(str::to_owned),
            _ => None,
        })
        .collect::<Vec<_>>();
    for component in parsed.components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment
                    .to_str()
                    .ok_or_else(|| "support path is not UTF-8".to_owned())?;
                if segment.is_empty() || segment.chars().any(char::is_control) {
                    return Err("support path contains an invalid segment".into());
                }
                segments.push(segment.to_owned());
            }
            Component::CurDir => {}
            Component::ParentDir if segments.pop().is_some() => {}
            _ => return Err("support path traversal escapes the entry directory".into()),
        }
    }
    if segments.is_empty() {
        return Err("support path is empty".into());
    }
    Ok(Some(segments.join("/")))
}

fn has_malformed_percent(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return true;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    false
}

fn ensure_supported_dependency(path: &str) -> Result<(), String> {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    const ALLOWED: &[&str] = &[
        "css", "js", "mjs", "json", "wasm", "png", "jpg", "jpeg", "gif", "webp", "avif", "ico",
        "svg", "mp3", "wav", "ogg", "m4a", "mp4", "webm", "woff", "woff2", "ttf", "otf",
    ];
    if ALLOWED.contains(&extension.as_str()) {
        Ok(())
    } else {
        Err(format!("unsupported local dependency type: {path}"))
    }
}

use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::io::ReaderStream;

const CLI_SCHEMA_VERSION: u32 = 1;
const MAX_STDIN_BYTES: u64 = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const PUBLIC_ID_ALPHABET: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

#[derive(Debug)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub details: serde_json::Map<String, Value>,
}

impl CliError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: serde_json::Map::new(),
        }
    }

    pub(crate) fn with_details(mut self, details: Value) -> Self {
        if let Value::Object(details) = details {
            self.details = details;
        }
        self
    }

    pub fn envelope(&self) -> Value {
        json!({"schema_version": CLI_SCHEMA_VERSION, "ok": false, "error": {
            "code": self.code, "message": self.message, "details": self.details
        }})
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonPublication {
    schema_version: u32,
    integration_namespace: String,
    external_session_key: String,
    project_label: String,
    working_directory: PathBuf,
    title: String,
    commentary: String,
    predecessor_post_id: Option<i64>,
    files: Vec<JsonPublicationFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonPublicationFile {
    source_path: PathBuf,
    published_filename: Option<String>,
    caption: Option<String>,
    media_type: Option<String>,
    #[serde(default = "default_collect_assets")]
    collect_assets: bool,
}

fn default_collect_assets() -> bool {
    true
}

struct PublicationInput {
    integration_namespace: String,
    external_key: String,
    project_label: String,
    working_directory: PathBuf,
    title: String,
    commentary: String,
    predecessor_post_id: Option<i64>,
    files: Vec<JsonPublicationFile>,
}

#[derive(Debug)]
struct PreparedPublication {
    integration_namespace: String,
    external_key: String,
    project_label: String,
    working_directory: String,
    title: String,
    commentary: String,
    predecessor_post_id: Option<i64>,
    git: Option<crate::storage::GitProvenance>,
    files: Vec<PreparedFile>,
}

#[derive(Debug)]
struct PreparedFile {
    filename: String,
    caption: Option<String>,
    media_type: Option<String>,
    file: File,
    byte_size: u64,
    support_assets: Vec<CollectedSupportAsset>,
}

#[derive(Serialize)]
struct OutManifest<'a> {
    integration_namespace: &'a str,
    external_key: &'a str,
    project_label: &'a str,
    working_directory: &'a str,
    title: &'a str,
    commentary: &'a str,
    predecessor_post_id: Option<i64>,
    git: &'a Option<crate::storage::GitProvenance>,
    files: Vec<OutManifestFile<'a>>,
}

#[derive(Serialize)]
struct OutManifestFile<'a> {
    part: String,
    filename: &'a str,
    caption: &'a Option<String>,
    media_type: &'a Option<String>,
    support_assets: Vec<OutManifestAsset<'a>>,
}

#[derive(Serialize)]
struct OutManifestAsset<'a> {
    part: String,
    relative_path: &'a str,
}

#[derive(Deserialize)]
struct PublicationResponse {
    session: crate::storage::SessionRead,
    post: crate::storage::PostRead,
}

pub async fn run_command(arguments: Vec<String>) -> Result<Value, CliError> {
    let command = arguments
        .first()
        .map(String::as_str)
        .ok_or_else(|| CliError::new("usage_error", "missing command"))?;
    match command {
        "publish" => publish_command(&arguments[1..]).await,
        "service" => crate::service::run(&arguments[1..]).map(success),
        "status" => {
            no_args(&arguments[1..])?;
            request_status().await
        }
        "health" => {
            no_args(&arguments[1..])?;
            request_json("GET", "/api/v1/health").await
        }
        "show" => {
            let id = exactly_one(&arguments[1..], "show requires one post ID")?;
            request_json(
                "GET",
                &format!("/api/v1/posts/{}", positive_integer(id, "post ID")?),
            )
            .await
        }
        "close" => {
            let id = session_public_id(exactly_one(
                &arguments[1..],
                "close requires one public session ID",
            )?)?;
            request_json("DELETE", &format!("/api/v1/sessions/{id}")).await
        }
        "list" => list_command(&arguments[1..]).await,
        "open" => open_command(&arguments[1..]).await,
        _ => Err(CliError::new(
            "usage_error",
            format!("unknown command: {command}"),
        )),
    }
}

fn no_args(args: &[String]) -> Result<(), CliError> {
    if args.is_empty() {
        Ok(())
    } else {
        Err(CliError::new(
            "usage_error",
            "command does not accept arguments",
        ))
    }
}

fn exactly_one<'a>(args: &'a [String], message: &str) -> Result<&'a str, CliError> {
    if args.len() == 1 {
        Ok(&args[0])
    } else {
        Err(CliError::new("usage_error", message))
    }
}

fn session_public_id(value: &str) -> Result<&str, CliError> {
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        Ok(value)
    } else {
        Err(CliError::new(
            "usage_error",
            "public session ID must be ASCII alphanumeric",
        ))
    }
}

fn positive_integer(value: &str, name: &str) -> Result<i64, CliError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| CliError::new("usage_error", format!("{name} must be a positive integer")))
}

async fn publish_command(args: &[String]) -> Result<Value, CliError> {
    let open = args.iter().any(|arg| arg == "--open");
    let prepared = if args.first().is_some_and(|arg| arg == "--json") {
        if args.iter().skip(1).any(|arg| arg != "--open") {
            return Err(CliError::new(
                "usage_error",
                "publish --json accepts only --open",
            ));
        }
        prepare_json_publication(read_stdin_json()?)?
    } else {
        prepare_flag_publication(args)?
    };
    let mut result = send_publication(prepared).await?;
    let viewer_url = result["viewer_url"]
        .as_str()
        .expect("validated publication result has viewer URL")
        .to_owned();
    let browser_launch = if open {
        match launch_browser(&viewer_url) {
            Ok(()) => json!({"requested": true, "opened": true, "error": null}),
            Err(error) => json!({
                "requested": true,
                "opened": false,
                "error": {"code": error.code, "message": error.message, "details": error.details}
            }),
        }
    } else {
        json!({"requested": false, "opened": false, "error": null})
    };
    result
        .as_object_mut()
        .expect("validated publication response is an object")
        .insert("browser_launch".into(), browser_launch);
    Ok(success(result.take()))
}

fn read_stdin_json() -> Result<JsonPublication, CliError> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| CliError::new("stdin_error", format!("could not read stdin: {error}")))?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return Err(CliError::new(
            "stdin_too_large",
            "publication JSON exceeds 1 MiB",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        CliError::new(
            "invalid_publication_json",
            format!("publication JSON is invalid: {error}"),
        )
    })?;
    if let Some(version) = value.get("schema_version").and_then(Value::as_u64)
        && version != u64::from(CLI_SCHEMA_VERSION)
    {
        return Err(CliError::new(
            "unsupported_schema_version",
            format!("unsupported publication schema version: {version}"),
        ));
    }
    serde_json::from_value(value).map_err(|error| {
        CliError::new(
            "invalid_publication_json",
            format!("publication JSON is invalid: {error}"),
        )
    })
}

fn prepare_json_publication(input: JsonPublication) -> Result<PreparedPublication, CliError> {
    if input.schema_version != CLI_SCHEMA_VERSION {
        return Err(CliError::new(
            "unsupported_schema_version",
            format!(
                "unsupported publication schema version: {}",
                input.schema_version
            ),
        ));
    }
    prepare_publication(PublicationInput {
        integration_namespace: input.integration_namespace,
        external_key: input.external_session_key,
        project_label: input.project_label,
        working_directory: input.working_directory,
        title: input.title,
        commentary: input.commentary,
        predecessor_post_id: input.predecessor_post_id,
        files: input.files,
    })
}

fn prepare_flag_publication(args: &[String]) -> Result<PreparedPublication, CliError> {
    let mut values = std::collections::HashMap::<String, String>::new();
    let mut index = 0;
    while index < args.len() {
        let flag = &args[index];
        if flag == "--open" {
            index += 1;
            continue;
        }
        if !matches!(
            flag.as_str(),
            "--file"
                | "--integration"
                | "--external-key"
                | "--project"
                | "--working-directory"
                | "--title"
                | "--commentary"
                | "--commentary-file"
                | "--filename"
                | "--caption"
                | "--media-type"
                | "--predecessor"
        ) {
            return Err(CliError::new(
                "usage_error",
                format!("unknown publish option: {flag}"),
            ));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| CliError::new("usage_error", format!("{flag} requires a value")))?;
        if values.insert(flag.clone(), value.clone()).is_some() {
            return Err(CliError::new(
                "usage_error",
                format!("duplicate option: {flag}"),
            ));
        }
        index += 2;
    }
    if values.contains_key("--commentary") && values.contains_key("--commentary-file") {
        return Err(CliError::new(
            "usage_error",
            "use either --commentary or --commentary-file",
        ));
    }
    let required = |flag: &str| {
        values
            .get(flag)
            .cloned()
            .ok_or_else(|| CliError::new("usage_error", format!("missing required option: {flag}")))
    };
    let commentary = if let Some(value) = values.get("--commentary") {
        value.clone()
    } else {
        let path = required("--commentary-file")?;
        read_bounded_commentary(Path::new(&path))?
    };
    let predecessor = values
        .get("--predecessor")
        .map(|value| positive_integer(value, "predecessor post ID"))
        .transpose()?;
    prepare_publication(PublicationInput {
        integration_namespace: required("--integration")?,
        external_key: required("--external-key")?,
        project_label: required("--project")?,
        working_directory: PathBuf::from(required("--working-directory")?),
        title: required("--title")?,
        commentary,
        predecessor_post_id: predecessor,
        files: vec![JsonPublicationFile {
            source_path: PathBuf::from(required("--file")?),
            published_filename: values.get("--filename").cloned(),
            caption: values.get("--caption").cloned(),
            media_type: values.get("--media-type").cloned(),
            collect_assets: true,
        }],
    })
}

fn read_bounded_commentary(path: &Path) -> Result<String, CliError> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(64 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(|error| {
            CliError::new(
                "filesystem_error",
                format!("could not read commentary file: {error}"),
            )
        })?;
    if bytes.len() > 64 * 1024 {
        return Err(CliError::new(
            "validation_error",
            "commentary file exceeds 64 KiB",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| CliError::new("validation_error", "commentary file must be UTF-8"))
}

fn prepare_publication(input: PublicationInput) -> Result<PreparedPublication, CliError> {
    for (name, value) in [
        ("integration_namespace", &input.integration_namespace),
        ("external_session_key", &input.external_key),
        ("project_label", &input.project_label),
        ("title", &input.title),
        ("commentary", &input.commentary),
    ] {
        if value.trim().is_empty() {
            return Err(CliError::new(
                "validation_error",
                format!("{name} must not be blank"),
            ));
        }
    }
    if input.files.is_empty() {
        return Err(CliError::new(
            "validation_error",
            "publication requires at least one file",
        ));
    }
    if input.predecessor_post_id.is_some_and(|id| id <= 0) {
        return Err(CliError::new(
            "validation_error",
            "predecessor_post_id must be positive",
        ));
    }
    let working_directory = fs::canonicalize(&input.working_directory).map_err(|error| {
        CliError::new(
            "filesystem_error",
            format!("could not resolve working directory: {error}"),
        )
    })?;
    if !working_directory.is_dir() {
        return Err(CliError::new(
            "filesystem_error",
            "working directory is not a directory",
        ));
    }
    let mut seen = HashSet::new();
    let mut prepared = Vec::with_capacity(input.files.len());
    let mut total_parts = 0usize;
    for specification in input.files {
        let canonical = fs::canonicalize(&specification.source_path).map_err(|error| {
            CliError::new(
                "filesystem_error",
                format!("could not resolve source file: {error}"),
            )
        })?;
        if !seen.insert(canonical.clone()) {
            return Err(CliError::new("validation_error", "duplicate source file"));
        }
        let file = File::open(&canonical).map_err(|error| {
            CliError::new(
                "filesystem_error",
                format!("could not open source file: {error}"),
            )
        })?;
        let metadata = file.metadata().map_err(|error| {
            CliError::new(
                "filesystem_error",
                format!("could not inspect opened source file: {error}"),
            )
        })?;
        if !metadata.is_file() {
            return Err(CliError::new(
                "filesystem_error",
                "source path is not a regular file",
            ));
        }
        let filename = specification.published_filename.unwrap_or_else(|| {
            canonical
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned()
        });
        if filename.trim().is_empty()
            || filename.contains('/')
            || filename.contains('\\')
            || filename.chars().any(char::is_control)
        {
            return Err(CliError::new(
                "validation_error",
                "published filename is invalid",
            ));
        }
        if specification
            .media_type
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(CliError::new(
                "validation_error",
                "media_type must not be blank",
            ));
        }
        let support_assets = if specification.collect_assets {
            collect_support_assets(&canonical)
                .map_err(|message| CliError::new("asset_collection_error", message))?
        } else {
            vec![]
        };
        total_parts += 1 + support_assets.len();
        if total_parts > 256 {
            return Err(CliError::new(
                "asset_collection_error",
                "publication exceeds 256 total byte parts",
            ));
        }
        prepared.push(PreparedFile {
            filename,
            caption: specification.caption,
            media_type: specification.media_type,
            file,
            byte_size: metadata.len(),
            support_assets,
        });
    }
    let git = collect_git_provenance(&working_directory)?;
    Ok(PreparedPublication {
        integration_namespace: input.integration_namespace,
        external_key: input.external_key,
        project_label: input.project_label,
        working_directory: working_directory
            .to_str()
            .ok_or_else(|| {
                CliError::new(
                    "validation_error",
                    "canonical working directory must be valid UTF-8",
                )
            })?
            .to_owned(),
        title: input.title,
        commentary: input.commentary,
        predecessor_post_id: input.predecessor_post_id,
        git,
        files: prepared,
    })
}

fn collect_git_provenance(
    directory: &Path,
) -> Result<Option<crate::storage::GitProvenance>, CliError> {
    let Some(root) = git_output(directory, &["rev-parse", "--show-toplevel"])? else {
        return Ok(None);
    };
    if root.len() > 4096 {
        return Err(CliError::new(
            "git_error",
            "Git root exceeds the supported length",
        ));
    }
    let branch = git_output(directory, &["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let commit = git_output(directory, &["rev-parse", "--verify", "HEAD"])?;
    if branch.as_ref().is_some_and(|value| value.len() > 1024)
        || commit.as_ref().is_some_and(|value| value.len() > 128)
    {
        return Err(CliError::new(
            "git_error",
            "Git provenance exceeds the supported length",
        ));
    }
    let provenance = crate::storage::GitProvenance {
        root,
        branch,
        commit,
    };
    if !provenance.is_valid() {
        return Err(CliError::new(
            "git_error",
            "Git returned invalid provenance",
        ));
    }
    Ok(Some(provenance))
}

fn git_output(directory: &Path, args: &[&str]) -> Result<Option<String>, CliError> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| CliError::new("git_error", format!("could not execute Git: {error}")))?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait().map_err(|error| {
            CliError::new("git_error", format!("could not wait for Git: {error}"))
        })? {
            Some(status) => {
                let mut output = Vec::new();
                if let Some(stdout) = child.stdout.take() {
                    stdout
                        .take(8193)
                        .read_to_end(&mut output)
                        .map_err(|error| {
                            CliError::new(
                                "git_error",
                                format!("could not read Git output: {error}"),
                            )
                        })?;
                }
                if !status.success() {
                    return Ok(None);
                }
                if output.len() > 8192 {
                    return Err(CliError::new("git_error", "Git output exceeds 8 KiB"));
                }
                let value = String::from_utf8(output)
                    .map_err(|_| CliError::new("git_error", "Git output is not UTF-8"))?;
                return Ok(Some(value.trim_end_matches(['\r', '\n']).to_owned()));
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CliError::new("git_error", "Git command timed out"));
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

async fn send_publication(publication: PreparedPublication) -> Result<Value, CliError> {
    let files = publication.files;
    let manifest_files = files
        .iter()
        .enumerate()
        .map(|(file_index, file)| OutManifestFile {
            part: format!("file-{file_index}"),
            filename: &file.filename,
            caption: &file.caption,
            media_type: &file.media_type,
            support_assets: file
                .support_assets
                .iter()
                .enumerate()
                .map(|(asset_index, asset)| OutManifestAsset {
                    part: format!("asset-{file_index}-{asset_index}"),
                    relative_path: &asset.relative_path,
                })
                .collect(),
        })
        .collect();
    let manifest = OutManifest {
        integration_namespace: &publication.integration_namespace,
        external_key: &publication.external_key,
        project_label: &publication.project_label,
        working_directory: &publication.working_directory,
        title: &publication.title,
        commentary: &publication.commentary,
        predecessor_post_id: publication.predecessor_post_id,
        git: &publication.git,
        files: manifest_files,
    };
    let manifest = serde_json::to_string(&manifest).map_err(|error| {
        CliError::new(
            "schema_error",
            format!("could not encode publication manifest: {error}"),
        )
    })?;
    if manifest.len() > 64 * 1024 {
        return Err(CliError::new(
            "schema_error",
            "publication manifest exceeds 64 KiB",
        ));
    }
    let mut form = Form::new().part(
        "manifest",
        Part::text(manifest)
            .mime_str("application/json")
            .map_err(|error| CliError::new("schema_error", error.to_string()))?,
    );
    for (file_index, file) in files.into_iter().enumerate() {
        let body =
            reqwest::Body::wrap_stream(ReaderStream::new(tokio::fs::File::from_std(file.file)));
        form = form.part(
            format!("file-{file_index}"),
            Part::stream_with_length(body, file.byte_size),
        );
        for (asset_index, asset) in file.support_assets.into_iter().enumerate() {
            let length = asset
                .file
                .metadata()
                .map_err(|error| {
                    CliError::new(
                        "filesystem_error",
                        format!("could not recheck opened support asset: {error}"),
                    )
                })?
                .len();
            let body = reqwest::Body::wrap_stream(ReaderStream::new(tokio::fs::File::from_std(
                asset.file,
            )));
            form = form.part(
                format!("asset-{file_index}-{asset_index}"),
                Part::stream_with_length(body, length),
            );
        }
    }
    let base = daemon_url()?;
    let url = format!("{base}/api/v1/posts");
    let request = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .read_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| CliError::new("http_error", error.to_string()))?
        .post(url)
        .multipart(form);
    let response = authorize_request(request)?.send().await.map_err(|error| {
        publication_may_have_succeeded(CliError::new(
            "daemon_unavailable",
            format!("could not reach daemon: {error}"),
        ))
    })?;
    let status = response.status();
    let payload = bounded_response_json(response)
        .await
        .map_err(publication_may_have_succeeded)?;
    if status.as_u16() != 201 {
        return Err(daemon_error(status.as_u16(), payload));
    }
    let response: PublicationResponse = serde_json::from_value(payload.clone()).map_err(|_| {
        publication_may_have_succeeded(CliError::new(
            "malformed_daemon_response",
            "publication response is missing required session or post structure",
        ))
    })?;
    let session_id = response.session.public_id;
    if session_id.len() < 6
        || !session_id
            .bytes()
            .all(|byte| PUBLIC_ID_ALPHABET.contains(&byte))
    {
        return Err(publication_may_have_succeeded(CliError::new(
            "malformed_daemon_response",
            "publication response contains an invalid session.public_id",
        )));
    }
    let post_id = response.post.id;
    if post_id <= 0 {
        return Err(publication_may_have_succeeded(CliError::new(
            "malformed_daemon_response",
            "publication response contains a non-positive post.id",
        )));
    }
    if response.post.session_public_id != session_id {
        return Err(publication_may_have_succeeded(CliError::new(
            "malformed_daemon_response",
            "publication response post.session_public_id does not match session.public_id",
        )));
    }
    let mut result = payload;
    let object = result.as_object_mut().ok_or_else(|| {
        publication_may_have_succeeded(CliError::new(
            "malformed_daemon_response",
            "publication response must be an object",
        ))
    })?;
    object.insert(
        "viewer_url".into(),
        json!(format!("{base}/sessions/{session_id}")),
    );
    object.insert(
        "post_url".into(),
        json!(format!("{base}/sessions/{session_id}#post-{post_id}")),
    );
    Ok(result)
}

fn publication_may_have_succeeded(mut error: CliError) -> CliError {
    error
        .details
        .insert("publication_may_have_succeeded".into(), json!(true));
    error
}

async fn request_status() -> Result<Value, CliError> {
    let payload = request_json_payload("GET", "/api/v1/status").await?;
    let status = serde_json::from_value::<crate::api::DaemonStatus>(payload).map_err(|_| {
        CliError::new(
            "malformed_daemon_response",
            "daemon status response has an invalid shape",
        )
    })?;
    Ok(success(
        serde_json::to_value(status).expect("daemon status serializes"),
    ))
}

async fn request_json(method: &str, path: &str) -> Result<Value, CliError> {
    request_json_payload(method, path).await.map(success)
}

async fn request_json_payload(method: &str, path: &str) -> Result<Value, CliError> {
    let url = format!("{}{}", daemon_url()?, path);
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| CliError::new("usage_error", "invalid HTTP method"))?;
    let request = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| CliError::new("http_error", error.to_string()))?
        .request(method, url);
    let response = authorize_request(request)?.send().await.map_err(|error| {
        CliError::new(
            "daemon_unavailable",
            format!("could not reach daemon: {error}"),
        )
    })?;
    let status = response.status();
    let payload = bounded_response_json(response).await?;
    if !status.is_success() {
        return Err(daemon_error(status.as_u16(), payload));
    }
    Ok(payload)
}

async fn bounded_response_json(mut response: reqwest::Response) -> Result<Value, CliError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(CliError::new(
            "daemon_response_too_large",
            "daemon response exceeds 1 MiB",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        CliError::new(
            "http_error",
            format!("could not read daemon response: {error}"),
        )
    })? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(CliError::new(
                "daemon_response_too_large",
                "daemon response exceeds 1 MiB",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        CliError::new(
            "malformed_daemon_response",
            "daemon response is not valid JSON",
        )
    })
}

fn daemon_error(status: u16, payload: Value) -> CliError {
    let code = payload
        .pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("daemon_rejected");
    let message = payload
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("daemon rejected the request");
    let mut details = payload
        .pointer("/error/details")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    details.insert("http_status".into(), json!(status));
    CliError::new(code, message).with_details(Value::Object(details))
}

fn authorize_request(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::RequestBuilder, CliError> {
    let token = crate::daemon::resolve_client_access_token().map_err(|error| {
        CliError::new(
            "configuration_error",
            format!("could not resolve daemon access token: {error}"),
        )
    })?;
    Ok(match token {
        Some(token) => request.bearer_auth(token.expose()),
        None => request,
    })
}

fn daemon_url() -> Result<String, CliError> {
    let value = std::env::var("GLIM_DAEMON_URL").unwrap_or_else(|_| "http://127.0.0.1:3030".into());
    let parsed = reqwest::Url::parse(&value)
        .map_err(|_| CliError::new("configuration_error", "GLIM_DAEMON_URL is not a valid URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CliError::new(
            "configuration_error",
            "GLIM_DAEMON_URL must use HTTP or HTTPS",
        ));
    }
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return Err(CliError::new(
            "configuration_error",
            "GLIM_DAEMON_URL must contain only an HTTP origin",
        ));
    }
    Ok(value.trim_end_matches('/').to_owned())
}

async fn list_command(args: &[String]) -> Result<Value, CliError> {
    let mut scope = None::<String>;
    let mut limit = None::<String>;
    let mut cursor = None::<String>;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--global" => {
                if scope.replace("/api/v1/posts".into()).is_some() {
                    return Err(CliError::new(
                        "usage_error",
                        "list requires exactly one scope",
                    ));
                }
                index += 1;
            }
            "--session" | "--project" => {
                let flag = &args[index];
                let value = args.get(index + 1).ok_or_else(|| {
                    CliError::new("usage_error", format!("{flag} requires a value"))
                })?;
                let path = if flag == "--session" {
                    format!("/api/v1/sessions/{}/posts", session_public_id(value)?)
                } else {
                    format!(
                        "/api/v1/projects/{}/posts",
                        positive_integer(value, "project ID")?
                    )
                };
                if scope.replace(path).is_some() {
                    return Err(CliError::new(
                        "usage_error",
                        "list requires exactly one scope",
                    ));
                }
                index += 2;
            }
            "--limit" | "--cursor" => {
                let flag = &args[index];
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| {
                        CliError::new("usage_error", format!("{flag} requires a value"))
                    })?
                    .clone();
                if flag == "--limit" {
                    if limit.is_some() {
                        return Err(CliError::new(
                            "usage_error",
                            "duplicate list option: --limit",
                        ));
                    }
                    if !(1..=100).contains(&positive_integer(&value, "limit")?) {
                        return Err(CliError::new("usage_error", "limit must be in 1..=100"));
                    }
                    limit = Some(value);
                } else {
                    if cursor.is_some() {
                        return Err(CliError::new(
                            "usage_error",
                            "duplicate list option: --cursor",
                        ));
                    }
                    cursor = Some(value);
                }
                index += 2;
            }
            option => {
                return Err(CliError::new(
                    "usage_error",
                    format!("unknown list option: {option}"),
                ));
            }
        }
    }
    let mut path = scope.ok_or_else(|| {
        CliError::new(
            "usage_error",
            "list requires --session, --project, or --global",
        )
    })?;
    let mut query = Vec::new();
    if let Some(limit) = limit {
        query.push(format!("limit={limit}"));
    }
    if let Some(cursor) = cursor {
        query.push(format!(
            "cursor={}",
            percent_encoding::utf8_percent_encode(&cursor, percent_encoding::NON_ALPHANUMERIC)
        ));
    }
    if !query.is_empty() {
        path.push('?');
        path.push_str(&query.join("&"));
    }
    request_json("GET", &path).await
}

async fn open_command(args: &[String]) -> Result<Value, CliError> {
    let value = exactly_one(args, "open requires one public session ID or session URL")?;
    let url = if value.starts_with("http://") || value.starts_with("https://") {
        let parsed = reqwest::Url::parse(value)
            .map_err(|_| CliError::new("usage_error", "session URL is invalid"))?;
        if parsed.host_str().is_none() {
            return Err(CliError::new("usage_error", "session URL is invalid"));
        }
        value.to_owned()
    } else {
        format!("{}/sessions/{}", daemon_url()?, session_public_id(value)?)
    };
    launch_browser(&url)?;
    Ok(success(json!({"viewer_url": url})))
}

fn launch_browser(url: &str) -> Result<(), CliError> {
    let command = std::env::var("GLIM_BROWSER_COMMAND").unwrap_or_else(|_| {
        if cfg!(target_os = "macos") {
            "open".into()
        } else {
            "xdg-open".into()
        }
    });
    let mut child = Command::new(command)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            CliError::new(
                "browser_launch_failed",
                format!("could not launch browser: {error}"),
            )
        })?;
    let deadline = Instant::now() + Duration::from_millis(100);
    loop {
        match child.try_wait().map_err(|error| {
            CliError::new(
                "browser_launch_failed",
                format!("could not wait for browser: {error}"),
            )
        })? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => {
                return Err(CliError::new(
                    "browser_launch_failed",
                    format!("browser command exited with {status}"),
                ));
            }
            None if Instant::now() >= deadline => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn success(result: Value) -> Value {
    json!({"schema_version": CLI_SCHEMA_VERSION, "ok": true, "result": result})
}
