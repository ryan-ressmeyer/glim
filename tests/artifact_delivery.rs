use std::io::Cursor;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode, header},
};
use glim::storage::{PublicationFile, PublicationRequest, PublicationSupportAsset, Store};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

fn multipart(boundary: &str, manifest: &Value, parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"manifest\"\r\nContent-Type: application/json\r\n\r\n{manifest}\r\n").as_bytes());
    for (name, bytes) in parts {
        body.extend_from_slice(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"ignored.html\"\r\nContent-Type: text/html\r\n\r\n").as_bytes());
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

async fn publish(
    app: axum::Router,
    filename: &str,
    media_type: Option<&str>,
    bytes: &[u8],
) -> axum::response::Response {
    let mut file = json!({"part":"file", "filename":filename});
    if let Some(media_type) = media_type {
        file["media_type"] = json!(media_type);
    }
    let manifest = json!({
        "integration_namespace":"pi", "external_key":"artifact", "project_label":"Glim",
        "working_directory":"/tmp/artifact", "title":"Artifact", "commentary":"Delivery",
        "files":[file]
    });
    let boundary = "artifact-boundary";
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/api/v1/posts")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(multipart(
                boundary,
                &manifest,
                &[("file", bytes)],
            )))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn publication_classifies_signature_and_text_families_and_ignores_part_headers() {
    type Case<'a> = (&'a str, Option<&'a str>, &'a [u8], &'a str, &'a str);
    let cases: &[Case<'_>] = &[
        (
            "plot.png",
            Some("image/png"),
            b"\x89PNG\r\n\x1a\nrest",
            "image/png",
            "image",
        ),
        (
            "photo.jpg",
            None,
            b"\xff\xd8\xffrest",
            "image/jpeg",
            "image",
        ),
        ("anim.gif", None, b"GIF89arest", "image/gif", "image"),
        (
            "image.webp",
            None,
            b"RIFFxxxxWEBPrest",
            "image/webp",
            "image",
        ),
        (
            "vector.svg",
            None,
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
            "image/svg+xml",
            "svg",
        ),
        ("paper.pdf", None, b"%PDF-1.7\n", "application/pdf", "pdf"),
        (
            "movie.mp4",
            None,
            b"\0\0\0\x18ftypisom\0\0\0\0isommp42",
            "video/mp4",
            "video",
        ),
        (
            "movie.webm",
            None,
            b"\x1aE\xdf\xa3\x42\x82\x84webm",
            "video/webm",
            "video",
        ),
        ("sound.mp3", None, b"ID3rest", "audio/mpeg", "audio"),
        ("sound.wav", None, b"RIFFxxxxWAVErest", "audio/wav", "audio"),
        (
            "sound.ogg",
            None,
            b"OggS\0\x02container OpusHead rest",
            "audio/ogg",
            "audio",
        ),
        ("sound.flac", None, b"fLaCrest", "audio/flac", "audio"),
        (
            "notes.md",
            None,
            b"# Heading\n\ntext\n",
            "text/markdown; charset=utf-8",
            "markdown",
        ),
        (
            "code.rs",
            None,
            b"fn main() {}\n",
            "text/plain; charset=utf-8",
            "text",
        ),
        (
            "data.json",
            Some("application/json"),
            br#"{"ok":true}"#,
            "application/json",
            "json",
        ),
        (
            "data.csv",
            None,
            b"a,b\n1,2\n",
            "text/csv; charset=utf-8",
            "csv",
        ),
        (
            "page.html",
            None,
            b"<!doctype html><html></html>",
            "text/html; charset=utf-8",
            "html",
        ),
        (
            "unknown.bin",
            None,
            b"\0\x01\x02",
            "application/octet-stream",
            "download",
        ),
    ];
    for (filename, declared, bytes, media_type, renderer) in cases {
        let root = TempDir::new().unwrap();
        let app = glim::app_with_store(glim::storage::Store::open(root.path()).unwrap());
        let response = publish(app, filename, *declared, bytes).await;
        assert_eq!(response.status(), StatusCode::CREATED, "{filename}");
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["post"]["files"][0]["media_type"], *media_type);
        assert_eq!(payload["post"]["files"][0]["renderer"], *renderer);
    }
}

#[tokio::test]
async fn exact_subtype_and_known_active_extension_mismatches_fail_atomically() {
    let cases: &[(&str, Option<&str>, &[u8])] = &[
        ("photo.jpg", None, b"\x89PNG\r\n\x1a\nrest"),
        ("movie.webm", None, b"\0\0\0\x18ftypisom\0\0\0\0isommp42"),
        ("sound.mp3", None, b"RIFFxxxxWAVErest"),
        (
            "claim.png",
            None,
            b"<!doctype html><html><script>alert(1)</script></html>",
        ),
        ("data.json", None, b"not json"),
        ("vector.svg", None, b"<html></html>"),
    ];
    for (filename, declared, bytes) in cases {
        let root = TempDir::new().unwrap();
        let response = publish(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            filename,
            *declared,
            bytes,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{filename}"
        );
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            payload["error"]["code"], "artifact_classification_failed",
            "{filename}"
        );
        let db = rusqlite::Connection::open(root.path().join("metadata.sqlite3")).unwrap();
        for table in ["projects", "sessions", "posts", "blobs"] {
            assert_eq!(
                db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0,
                "{filename} {table}"
            );
        }
    }
}

#[tokio::test]
async fn bounded_text_accepts_only_a_split_terminal_utf8_code_point_and_valid_fragments() {
    let mut split_json = String::from("{\"text\":\"");
    split_json.push_str(&"a".repeat(65_535 - split_json.len()));
    assert_eq!(split_json.len(), 65_535);
    split_json.push('😀');
    split_json.push_str("\"}");

    let cases: &[(&str, Option<&str>, &[u8], &str)] = &[
        (
            "split.json",
            Some("application/json"),
            split_json.as_bytes(),
            "json",
        ),
        ("quoted.csv", None, b"name,note\nx,\"a,b\"\n", "csv"),
        (
            "fragment.html",
            None,
            b"<div class=\"result\">fragment</div>",
            "html",
        ),
    ];
    for (filename, declared, bytes, renderer) in cases {
        let root = TempDir::new().unwrap();
        let response = publish(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            filename,
            *declared,
            bytes,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{filename}");
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(
            payload["post"]["files"][0]["renderer"], *renderer,
            "{filename}"
        );
    }

    let mut invalid = b"{\"text\":\"".to_vec();
    invalid.extend(std::iter::repeat_n(b'a', 100));
    invalid.push(0xff);
    invalid.extend(std::iter::repeat_n(b'a', 70_000));
    invalid.extend_from_slice(b"\"}");
    let root = TempDir::new().unwrap();
    let response = publish(
        glim::app_with_store(Store::open(root.path()).unwrap()),
        "invalid.json",
        Some("application/json"),
        &invalid,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn m4a_and_m4b_brands_are_audio_and_exact_subtype_mismatches_are_rejected() {
    for brand in [b"M4A ", b"M4B "] {
        let mut bytes = b"\0\0\0\x18ftyp".to_vec();
        bytes.extend_from_slice(brand);
        bytes.extend_from_slice(b"\0\0\0\0isommp42");
        let extension = if brand == b"M4A " { "m4a" } else { "m4b" };
        let root = TempDir::new().unwrap();
        let response = publish(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            &format!("audio.{extension}"),
            Some("audio/mp4"),
            &bytes,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED, "{extension}");
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["post"]["files"][0]["media_type"], "audio/mp4");
        assert_eq!(payload["post"]["files"][0]["renderer"], "audio");

        let root = TempDir::new().unwrap();
        let response = publish(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            "wrong.mp4",
            None,
            &bytes,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    let video = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";
    let root = TempDir::new().unwrap();
    let response = publish(
        glim::app_with_store(Store::open(root.path()).unwrap()),
        "wrong.m4a",
        Some("audio/mp4"),
        video,
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn bounded_json_accepts_plausible_incomplete_prefix_and_rejects_observable_syntax_errors() {
    let large = format!(
        "{{\"items\":[{}]}}",
        (0..30_000)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",")
    );
    assert!(large.len() > 168_000);
    let root = TempDir::new().unwrap();
    let response = publish(
        glim::app_with_store(Store::open(root.path()).unwrap()),
        "large.json",
        Some("application/json"),
        large.as_bytes(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let payload: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["post"]["files"][0]["renderer"], "json");

    let root = TempDir::new().unwrap();
    let mut invalid = format!("{{\"items\":[{}]", "1,".repeat(40_000));
    invalid.replace_range(100..101, "!");
    let response = publish(
        glim::app_with_store(Store::open(root.path()).unwrap()),
        "large.json",
        Some("application/json"),
        invalid.as_bytes(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn container_lookalikes_are_not_promoted_to_supported_media() {
    let cases: &[(&str, &[u8])] = &[
        ("unknown.bin", b"xxxxftypheicxxxxmif1"),
        ("unknown.bin", b"xxxxftypqt  xxxx"),
        ("unknown.bin", b"\x1aE\xdf\xa3\x42\x82\x88matroska"),
        ("unknown.bin", b"\x1aE\xdf\xa3random-webm-text"),
        ("unknown.bin", b"OggS\0\x02arbitrary-container"),
    ];
    for (filename, bytes) in cases {
        let root = TempDir::new().unwrap();
        let response = publish(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            filename,
            None,
            bytes,
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["post"]["files"][0]["renderer"], "download");
    }
    for (filename, bytes) in [
        ("fake.mp4", b"xxxxftypheicxxxxmif1".as_slice()),
        ("fake.webm", b"\x1aE\xdf\xa3\x42\x82\x88matroska".as_slice()),
        ("fake.webm", b"\x1aE\xdf\xa3random-webm-text".as_slice()),
        ("fake.ogg", b"OggS\0\x02arbitrary-container".as_slice()),
    ] {
        let root = TempDir::new().unwrap();
        let response = publish(
            glim::app_with_store(Store::open(root.path()).unwrap()),
            filename,
            None,
            bytes,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{filename}"
        );
    }
}

#[tokio::test]
async fn contradictory_declared_media_is_rejected_without_persistent_or_staging_state() {
    let root = TempDir::new().unwrap();
    let app = glim::app_with_store(glim::storage::Store::open(root.path()).unwrap());
    let response = publish(
        app,
        "claim.png",
        Some("image/png"),
        b"<script>alert(1)</script>",
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let payload: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(payload["error"]["code"], "artifact_classification_failed");
    let db = rusqlite::Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    for table in ["projects", "sessions", "posts", "blobs"] {
        assert_eq!(
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0,
            "{table}"
        );
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("blobs/publication-staging"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn publication_rejects_traversal_and_control_support_paths_without_state() {
    for relative_path in [
        "../secret.txt",
        "nested/control\u{1}.css",
        "nested/line\n.css",
    ] {
        let root = TempDir::new().unwrap();
        let app = glim::app_with_store(Store::open(root.path()).unwrap());
        let manifest = json!({"integration_namespace":"pi","external_key":"unsafe","project_label":"Glim","working_directory":"/tmp/unsafe","title":"Unsafe","commentary":"Path","files":[{"part":"file","filename":"entry.md","support_assets":[{"part":"asset","relative_path":relative_path}]}]});
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/posts")
                    .header("content-type", "multipart/form-data; boundary=b")
                    .body(Body::from(multipart(
                        "b",
                        &manifest,
                        &[("file", b"# safe"), ("asset", b"secret")],
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{relative_path:?}"
        );
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["error"]["code"], "validation_failed");
        let db = rusqlite::Connection::open(root.path().join("metadata.sqlite3")).unwrap();
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

fn artifact_store(root: &TempDir, filename: &str, bytes: &[u8]) -> (Store, i64) {
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "delivery", "Glim", "/tmp/delivery")
        .unwrap();
    let visible = store.stage_publication_blob(Cursor::new(bytes)).unwrap();
    let support = store
        .stage_publication_blob(Cursor::new(b"support-data"))
        .unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Delivery".into(),
                commentary: "Bytes".into(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: filename.into(),
                    caption: None,
                    blob: visible,
                    support_assets: vec![PublicationSupportAsset {
                        relative_path: "nested/asset.png".into(),
                        blob: support,
                    }],
                }],
            },
            10,
        )
        .unwrap();
    (store, post.id)
}

async fn artifact_request(
    app: axum::Router,
    method: Method,
    uri: &str,
    range: Option<&str>,
) -> axum::response::Response {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }
    app.oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn support_dependencies_receive_safe_nosniff_compatible_media_types() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "support-types", "Glim", "/tmp/support-types")
        .unwrap();
    let assets: &[(&str, &[u8], &str)] = &[
        (
            "style.css",
            b"body { color: red; }",
            "text/css; charset=utf-8",
        ),
        (
            "module.js",
            b"export const value = 1;",
            "application/javascript; charset=utf-8",
        ),
        ("data.json", br#"{"value":1}"#, "application/json"),
        ("image.png", b"\x89PNG\r\n\x1a\nrest", "image/png"),
        (
            "image.svg",
            b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
            "image/svg+xml",
        ),
        ("module.wasm", b"\0asm\x01\0\0\0", "application/wasm"),
        ("bad.wasm", b"\0asmbad!", "application/octet-stream"),
        ("font.woff", b"wOFFrest", "font/woff"),
        ("font.woff2", b"wOF2rest", "font/woff2"),
        ("font.ttf", b"\0\x01\0\0rest", "font/ttf"),
        ("font.otf", b"OTTOrest", "font/otf"),
    ];
    let support_assets = assets
        .iter()
        .map(|(path, bytes, _)| PublicationSupportAsset {
            relative_path: format!("nested/{path}"),
            blob: store.stage_publication_blob(Cursor::new(*bytes)).unwrap(),
        })
        .collect();
    let visible = store
        .stage_publication_blob(Cursor::new(b"# entry"))
        .unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Support".into(),
                commentary: "Types".into(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "entry.md".into(),
                    caption: None,
                    blob: visible,
                    support_assets,
                }],
            },
            10,
        )
        .unwrap();
    let app = glim::app_with_store(store);
    for (path, bytes, media_type) in assets {
        let response = artifact_request(
            app.clone(),
            Method::GET,
            &format!("/api/v1/posts/{}/files/0/support/nested/{path}", post.id),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            *media_type,
            "{path}"
        );
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "private, max-age=31536000, immutable"
        );
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            *bytes
        );
    }
}

#[tokio::test]
async fn empty_artifact_filename_uses_nonempty_safe_disposition_fallback() {
    let root = TempDir::new().unwrap();
    let (store, post_id) = artifact_store(&root, "", b"text");
    let response = artifact_request(
        glim::app_with_store(store),
        Method::GET,
        &format!("/api/v1/posts/{post_id}/files/0/content"),
        None,
    )
    .await;
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "inline; filename=\"download\""
    );
}

#[tokio::test]
async fn visible_artifact_get_head_and_single_ranges_have_exact_headers_and_bytes() {
    let root = TempDir::new().unwrap();
    let (store, post_id) = artifact_store(&root, "unsafe\"\r\nname.txt", b"0123456789");
    let app = glim::app_with_store(store);
    let uri = format!("/api/v1/posts/{post_id}/files/0/content");
    let response = artifact_request(app.clone(), Method::GET, &uri, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(
        response.headers()[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    let disposition = response.headers()[header::CONTENT_DISPOSITION]
        .to_str()
        .unwrap();
    assert!(!disposition.contains(['\r', '\n']));
    assert!(disposition.starts_with("inline; filename=\"") && disposition.ends_with('\"'));
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "0123456789"
    );

    let head = artifact_request(app.clone(), Method::HEAD, &uri, None).await;
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
    assert!(
        head.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );

    for (range, status, expected, content_range) in [
        (
            "bytes=0-2",
            StatusCode::PARTIAL_CONTENT,
            "012",
            "bytes 0-2/10",
        ),
        (
            "bytes=3-5",
            StatusCode::PARTIAL_CONTENT,
            "345",
            "bytes 3-5/10",
        ),
        (
            "bytes=7-",
            StatusCode::PARTIAL_CONTENT,
            "789",
            "bytes 7-9/10",
        ),
        (
            "bytes=-4",
            StatusCode::PARTIAL_CONTENT,
            "6789",
            "bytes 6-9/10",
        ),
    ] {
        let response = artifact_request(app.clone(), Method::GET, &uri, Some(range)).await;
        assert_eq!(response.status(), status, "{range}");
        assert_eq!(response.headers()[header::CONTENT_RANGE], content_range);
        assert_eq!(
            response.headers()[header::CONTENT_LENGTH],
            expected.len().to_string()
        );
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            expected
        );
    }
}

#[tokio::test]
async fn invalid_ranges_zero_bytes_association_and_support_paths_are_isolated() {
    let root = TempDir::new().unwrap();
    let (mut store, post_id) = artifact_store(&root, "entry.txt", b"");
    let other_session = store
        .resolve_session("pi", "other", "Other", "/tmp/other")
        .unwrap();
    let other = store
        .publish_at(
            PublicationRequest {
                session_public_id: other_session.public_id,
                title: "Other".into(),
                commentary: "Other".into(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "other.txt".into(),
                    caption: None,
                    blob: store.stage_publication_blob(Cursor::new(b"other")).unwrap(),
                    support_assets: vec![],
                }],
            },
            11,
        )
        .unwrap();
    let app = glim::app_with_store(store);
    let visible = format!("/api/v1/posts/{post_id}/files/0/content");
    for range in [
        None,
        Some("bytes=0-0"),
        Some("bytes=0-1,3-4"),
        Some("nonsense"),
    ] {
        let response = artifact_request(app.clone(), Method::GET, &visible, range).await;
        assert_eq!(
            response.status(),
            if range.is_none() {
                StatusCode::OK
            } else {
                StatusCode::RANGE_NOT_SATISFIABLE
            }
        );
        if range.is_some() {
            assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */0");
        }
        assert!(
            response
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty()
        );
    }
    let support = format!("/api/v1/posts/{post_id}/files/0/support/nested/asset.png");
    let response = artifact_request(app.clone(), Method::GET, &support, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.into_body().collect().await.unwrap().to_bytes(),
        "support-data"
    );
    for uri in [
        format!(
            "/api/v1/posts/{}/files/0/support/nested/asset.png",
            other.id
        ),
        format!("/api/v1/posts/{post_id}/files/1/support/nested/asset.png"),
        format!("/api/v1/posts/{post_id}/files/0/support/nested%2Fasset.png"),
        format!("/api/v1/posts/{post_id}/files/0/support/../nested/asset.png"),
        format!("/api/v1/posts/{post_id}/files/0/support/nested//asset.png"),
    ] {
        let response = artifact_request(app.clone(), Method::GET, &uri, None).await;
        assert!(
            matches!(
                response.status(),
                StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
            ),
            "{} {uri}",
            response.status()
        );
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(matches!(
            payload["error"]["code"].as_str(),
            Some("artifact_not_found" | "malformed_path" | "api_route_not_found")
        ));
        assert!(!payload.to_string().contains("blobs/"));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_a_large_stream_releases_store_mutex_before_body_consumption() {
    let root = TempDir::new().unwrap();
    let bytes = vec![b'x'; 4 * 1024 * 1024];
    let (store, post_id) = artifact_store(&root, "large.bin", &bytes);
    let app = glim::app_with_store(store);
    let response = artifact_request(
        app.clone(),
        Method::GET,
        &format!("/api/v1/posts/{post_id}/files/0/content"),
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let concurrent = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        artifact_request(app, Method::GET, "/api/v1/posts", None),
    )
    .await;
    assert!(
        concurrent.is_ok(),
        "unconsumed stream retained the store mutex"
    );
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .len(),
        bytes.len()
    );
}

#[tokio::test]
async fn missing_or_corrupt_finalized_artifacts_return_sanitized_integrity_errors_before_streaming()
{
    for corrupt in [false, true] {
        let root = TempDir::new().unwrap();
        let (store, post_id) = artifact_store(&root, "entry.txt", b"original");
        let hash = store.post(post_id).unwrap().files[0].blob.hash.clone();
        let path = root
            .path()
            .join("blobs")
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(hash);
        if corrupt {
            std::fs::write(path, b"changed-size").unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        let response = artifact_request(
            glim::app_with_store(store),
            Method::GET,
            &format!("/api/v1/posts/{post_id}/files/0/content"),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let payload: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(payload["error"]["code"], "storage_integrity_error");
        assert!(!payload.to_string().contains("blobs"));
        assert!(!payload.to_string().contains(root.path().to_str().unwrap()));
    }
}
