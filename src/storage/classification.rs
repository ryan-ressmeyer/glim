use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use serde::{Deserialize, Serialize};

use super::{StoreError, publication::StagedPublicationBlob};

const PREFIX_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRenderer {
    Image,
    Svg,
    Pdf,
    Video,
    Audio,
    Markdown,
    Text,
    Json,
    Csv,
    Html,
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileClassification {
    pub media_type: String,
    pub renderer: ArtifactRenderer,
}

#[derive(Clone, Copy)]
struct FilenameExpectation {
    media_type: &'static str,
    specialized: bool,
}

pub(crate) fn classify_staged(
    staged: &StagedPublicationBlob,
    filename: &str,
    declared: Option<&str>,
) -> Result<FileClassification, StoreError> {
    let mut file = File::open(&staged.data_path)?;
    let (prefix, truncated) = read_prefix(&mut file)?;
    classify_visible_prefix(&prefix, truncated, filename, declared)
}

pub(crate) fn safe_support_media_type(filename: &str, file: &File) -> Result<String, StoreError> {
    let mut file = file.try_clone()?;
    let (prefix, truncated) = read_prefix(&mut file)?;
    Ok(classify_support_prefix(&prefix, truncated, filename)
        .unwrap_or("application/octet-stream")
        .to_owned())
}

fn read_prefix(file: &mut File) -> Result<(Vec<u8>, bool), StoreError> {
    file.seek(SeekFrom::Start(0))?;
    let total = file.metadata()?.len();
    let mut prefix = Vec::new();
    file.take(PREFIX_LIMIT).read_to_end(&mut prefix)?;
    file.seek(SeekFrom::Start(0))?;
    let truncated = total > u64::try_from(prefix.len()).expect("prefix length exceeds u64");
    Ok((prefix, truncated))
}

fn classify_visible_prefix(
    prefix: &[u8],
    truncated: bool,
    filename: &str,
    declared: Option<&str>,
) -> Result<FileClassification, StoreError> {
    let extension = extension(filename);
    let expected = filename_expectation(extension.as_deref());
    let declared = declared.map(normalize_declared).transpose()?;

    if let Some(strong) = strong_signature(prefix) {
        if expected.is_some_and(|value| value.media_type != strong.media_type)
            || declared
                .as_ref()
                .is_some_and(|value| value.media_type != strong.media_type)
        {
            return Err(StoreError::ArtifactClassificationFailed);
        }
        return Ok(strong);
    }

    let text = bounded_utf8_text(prefix, truncated);
    let classified = match (extension.as_deref(), text) {
        (Some("svg"), Some(value)) if looks_svg(value) => c("image/svg+xml", ArtifactRenderer::Svg),
        (Some("json"), Some(value)) if plausible_json(value.as_bytes(), truncated) => {
            c("application/json", ArtifactRenderer::Json)
        }
        (Some("html" | "htm"), Some(_)) => c("text/html; charset=utf-8", ArtifactRenderer::Html),
        (Some("md" | "markdown"), Some(_)) => {
            c("text/markdown; charset=utf-8", ArtifactRenderer::Markdown)
        }
        (Some("csv"), Some(_)) => c("text/csv; charset=utf-8", ArtifactRenderer::Csv),
        (Some(value), Some(_)) if is_safe_text_extension(value) => {
            c("text/plain; charset=utf-8", ArtifactRenderer::Text)
        }
        (None, Some(_)) => c("text/plain; charset=utf-8", ArtifactRenderer::Text),
        _ => c("application/octet-stream", ArtifactRenderer::Download),
    };

    if expected.is_some_and(|value| value.specialized && value.media_type != classified.media_type)
    {
        return Err(StoreError::ArtifactClassificationFailed);
    }
    if let Some(declared) = declared
        && declared.media_type != classified.media_type
    {
        return Err(StoreError::ArtifactClassificationFailed);
    }
    Ok(classified)
}

fn classify_support_prefix(prefix: &[u8], truncated: bool, filename: &str) -> Option<&'static str> {
    let extension = extension(filename);
    let expected = filename_expectation(extension.as_deref());
    if let Some(strong) = strong_signature(prefix) {
        return expected
            .is_none_or(|value| value.media_type == strong.media_type)
            .then_some(strong_media_type(&strong));
    }

    match extension.as_deref()? {
        "css" if bounded_utf8_text(prefix, truncated).is_some() => Some("text/css; charset=utf-8"),
        "js" | "mjs" | "cjs" if bounded_utf8_text(prefix, truncated).is_some() => {
            Some("application/javascript; charset=utf-8")
        }
        "json"
            if bounded_utf8_text(prefix, truncated)
                .is_some_and(|value| plausible_json(value.as_bytes(), truncated)) =>
        {
            Some("application/json")
        }
        "svg" if bounded_utf8_text(prefix, truncated).is_some_and(looks_svg) => {
            Some("image/svg+xml")
        }
        "wasm" if prefix.starts_with(b"\0asm\x01\0\0\0") => Some("application/wasm"),
        "woff" if prefix.starts_with(b"wOFF") => Some("font/woff"),
        "woff2" if prefix.starts_with(b"wOF2") => Some("font/woff2"),
        "ttf" if prefix.starts_with(b"\0\x01\0\0") => Some("font/ttf"),
        "otf" if prefix.starts_with(b"OTTO") => Some("font/otf"),
        _ => None,
    }
}

fn strong_signature(bytes: &[u8]) -> Option<FileClassification> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(c("image/png", ArtifactRenderer::Image))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(c("image/jpeg", ArtifactRenderer::Image))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(c("image/gif", ArtifactRenderer::Image))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(c("image/webp", ArtifactRenderer::Image))
    } else if bytes.starts_with(b"%PDF-") {
        Some(c("application/pdf", ArtifactRenderer::Pdf))
    } else if is_mp4_audio(bytes) {
        Some(c("audio/mp4", ArtifactRenderer::Audio))
    } else if is_mp4_video(bytes) {
        Some(c("video/mp4", ArtifactRenderer::Video))
    } else if is_webm(bytes) {
        Some(c("video/webm", ArtifactRenderer::Video))
    } else if is_mp3(bytes) {
        Some(c("audio/mpeg", ArtifactRenderer::Audio))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        Some(c("audio/wav", ArtifactRenderer::Audio))
    } else if is_supported_ogg_audio(bytes) {
        Some(c("audio/ogg", ArtifactRenderer::Audio))
    } else if bytes.starts_with(b"fLaC") {
        Some(c("audio/flac", ArtifactRenderer::Audio))
    } else {
        None
    }
}

fn is_mp4_audio(bytes: &[u8]) -> bool {
    bmff_brands(bytes).is_some_and(|(major, compatible)| {
        matches!(major, b"M4A " | b"M4B ")
            || compatible
                .chunks_exact(4)
                .any(|brand| matches!(brand, b"M4A " | b"M4B "))
    })
}

fn is_mp4_video(bytes: &[u8]) -> bool {
    let Some((major, compatible)) = bmff_brands(bytes) else {
        return false;
    };
    let supported = [b"isom", b"iso2", b"mp41", b"mp42", b"avc1", b"dash"];
    supported.contains(&major.try_into().expect("four-byte brand"))
        || compatible
            .chunks_exact(4)
            .any(|brand| supported.contains(&brand.try_into().expect("four-byte brand")))
}

fn bmff_brands(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    let declared_size =
        u32::from_be_bytes(bytes[..4].try_into().expect("four-byte slice")) as usize;
    let box_end = declared_size.min(bytes.len());
    if declared_size < 16 || box_end < 16 {
        return None;
    }
    Some((&bytes[8..12], &bytes[16..box_end]))
}

fn is_webm(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1aE\xdf\xa3")
        && bytes.windows(7).any(|window| {
            window[..3] == [0x42, 0x82, 0x84] && window[3..].eq_ignore_ascii_case(b"webm")
        })
}

fn is_supported_ogg_audio(bytes: &[u8]) -> bool {
    bytes.starts_with(b"OggS")
        && (bytes.windows(8).any(|window| window == b"OpusHead")
            || bytes.windows(7).any(|window| window == b"\x01vorbis")
            || bytes.windows(4).any(|window| window == b"fLaC"))
}

fn is_mp3(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"ID3") {
        return true;
    }
    if bytes.len() < 4 || bytes[0] != 0xff || bytes[1] & 0xe0 != 0xe0 {
        return false;
    }
    let version = (bytes[1] >> 3) & 0x03;
    let layer = (bytes[1] >> 1) & 0x03;
    let bitrate = bytes[2] >> 4;
    let sample_rate = (bytes[2] >> 2) & 0x03;
    version != 0x01 && layer != 0 && !matches!(bitrate, 0 | 0x0f) && sample_rate != 0x03
}

fn filename_expectation(extension: Option<&str>) -> Option<FilenameExpectation> {
    let (media_type, specialized) = match extension? {
        "png" => ("image/png", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "gif" => ("image/gif", true),
        "webp" => ("image/webp", true),
        "svg" => ("image/svg+xml", true),
        "pdf" => ("application/pdf", true),
        "mp4" => ("video/mp4", true),
        "m4a" | "m4b" => ("audio/mp4", true),
        "webm" => ("video/webm", true),
        "mp3" => ("audio/mpeg", true),
        "wav" => ("audio/wav", true),
        "ogg" | "oga" => ("audio/ogg", true),
        "flac" => ("audio/flac", true),
        "md" | "markdown" => ("text/markdown; charset=utf-8", true),
        "json" => ("application/json", true),
        "csv" => ("text/csv; charset=utf-8", true),
        "html" | "htm" => ("text/html; charset=utf-8", true),
        value if is_safe_text_extension(value) => ("text/plain; charset=utf-8", false),
        _ => return None,
    };
    Some(FilenameExpectation {
        media_type,
        specialized,
    })
}

fn normalize_declared(value: &str) -> Result<FileClassification, StoreError> {
    Ok(match value.trim().to_ascii_lowercase().as_str() {
        "image/png" => c("image/png", ArtifactRenderer::Image),
        "image/jpeg" => c("image/jpeg", ArtifactRenderer::Image),
        "image/gif" => c("image/gif", ArtifactRenderer::Image),
        "image/webp" => c("image/webp", ArtifactRenderer::Image),
        "image/svg+xml" => c("image/svg+xml", ArtifactRenderer::Svg),
        "application/pdf" => c("application/pdf", ArtifactRenderer::Pdf),
        "video/mp4" => c("video/mp4", ArtifactRenderer::Video),
        "video/webm" => c("video/webm", ArtifactRenderer::Video),
        "audio/mp4" => c("audio/mp4", ArtifactRenderer::Audio),
        "audio/mpeg" => c("audio/mpeg", ArtifactRenderer::Audio),
        "audio/wav" => c("audio/wav", ArtifactRenderer::Audio),
        "audio/ogg" => c("audio/ogg", ArtifactRenderer::Audio),
        "audio/flac" => c("audio/flac", ArtifactRenderer::Audio),
        "text/markdown" => c("text/markdown; charset=utf-8", ArtifactRenderer::Markdown),
        "text/plain" => c("text/plain; charset=utf-8", ArtifactRenderer::Text),
        "application/json" => c("application/json", ArtifactRenderer::Json),
        "text/csv" => c("text/csv; charset=utf-8", ArtifactRenderer::Csv),
        "text/html" => c("text/html; charset=utf-8", ArtifactRenderer::Html),
        _ => return Err(StoreError::ArtifactClassificationFailed),
    })
}

fn plausible_json(bytes: &[u8], truncated: bool) -> bool {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(_) => true,
        Err(error) => truncated && error.classify() == serde_json::error::Category::Eof,
    }
}

fn extension(filename: &str) -> Option<String> {
    Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

fn bounded_utf8_text(bytes: &[u8], truncated: bool) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(value) => Some(value),
        Err(error) if truncated && error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

fn looks_svg(text: &str) -> bool {
    let mut value = text.trim_start();
    if value.starts_with("<?xml") {
        let Some(end) = value.find("?>") else {
            return false;
        };
        value = value[end + 2..].trim_start();
    }
    value
        .get(..4)
        .is_some_and(|start| start.eq_ignore_ascii_case("<svg"))
}

fn is_safe_text_extension(extension: &str) -> bool {
    matches!(
        extension,
        "txt"
            | "rs"
            | "py"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "css"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "sh"
            | "c"
            | "h"
            | "cpp"
            | "java"
            | "r"
            | "m"
    )
}

fn strong_media_type(classification: &FileClassification) -> &'static str {
    match classification.media_type.as_str() {
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/webp" => "image/webp",
        "application/pdf" => "application/pdf",
        "video/mp4" => "video/mp4",
        "video/webm" => "video/webm",
        "audio/mp4" => "audio/mp4",
        "audio/mpeg" => "audio/mpeg",
        "audio/wav" => "audio/wav",
        "audio/ogg" => "audio/ogg",
        "audio/flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

fn c(media_type: &str, renderer: ArtifactRenderer) -> FileClassification {
    FileClassification {
        media_type: media_type.into(),
        renderer,
    }
}
