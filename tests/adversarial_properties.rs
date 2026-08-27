use std::{
    io::Cursor,
    process::{Command, Stdio},
};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use proptest::{
    prelude::*,
    test_runner::{Config, RngAlgorithm, TestRng, TestRunner},
};
use tempfile::TempDir;
use tower::ServiceExt;

use glim::storage::{ArtifactRenderer, PublicationFile, PublicationRequest, Store};

const SEED: [u8; 32] = [0x6d; 32];

fn runner(cases: u32) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &SEED),
    )
}

fn published_app(bytes: &[u8], filename: &str) -> (TempDir, axum::Router) {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("property", "ranges", "Properties", "/tmp/properties")
        .unwrap();
    let blob = store.stage_publication_blob(Cursor::new(bytes)).unwrap();
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Generated".into(),
                commentary: "Generated contract input".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: filename.into(),
                    caption: None,
                    blob,
                    support_assets: vec![],
                }],
            },
            1,
        )
        .unwrap();
    let app = glim::app_with_store(store);
    (root, app)
}

#[test]
fn generated_local_support_paths_normalize_without_escape() {
    let mut runner = runner(32);
    let strategy = prop::collection::vec("[a-z][a-z0-9]{0,7}", 1..5);
    runner
        .run(&strategy, |segments| {
            let root = TempDir::new().unwrap();
            let relative = format!("{}.png", segments.join("/"));
            let asset = root.path().join(&relative);
            std::fs::create_dir_all(asset.parent().unwrap()).unwrap();
            std::fs::write(&asset, b"\x89PNG\r\n\x1a\n").unwrap();
            let reference = format!("./{}?cache=1#fragment", relative);
            std::fs::write(
                root.path().join("entry.html"),
                format!(r#"<img src="{reference}">"#),
            )
            .unwrap();
            let assets =
                glim::cli::collect_support_assets(&root.path().join("entry.html")).unwrap();
            prop_assert_eq!(assets.len(), 1);
            prop_assert_eq!(&assets[0].relative_path, &relative);
            prop_assert!(assets[0].canonical_path.starts_with(root.path()));
            Ok(())
        })
        .unwrap();
}

#[test]
fn generated_signature_extension_pairs_keep_their_renderer_contract() {
    let cases: &[(&[u8], &str, ArtifactRenderer, &str)] = &[
        (
            b"\x89PNG\r\n\x1a\n",
            "png",
            ArtifactRenderer::Image,
            "image/png",
        ),
        (
            b"\xff\xd8\xff\xe0",
            "jpeg",
            ArtifactRenderer::Image,
            "image/jpeg",
        ),
        (b"GIF89a", "gif", ArtifactRenderer::Image, "image/gif"),
        (b"%PDF-1.7", "pdf", ArtifactRenderer::Pdf, "application/pdf"),
        (b"fLaC", "flac", ArtifactRenderer::Audio, "audio/flac"),
    ];
    let mut runner = runner(32);
    runner
        .run(
            &(
                0..cases.len(),
                any::<bool>(),
                prop::collection::vec(any::<u8>(), 0..32),
            ),
            |(index, uppercase, suffix)| {
                let (signature, extension, renderer, media_type) = cases[index];
                let mut bytes = signature.to_vec();
                bytes.extend(suffix);
                let extension = if uppercase {
                    extension.to_ascii_uppercase()
                } else {
                    extension.to_owned()
                };
                let root = TempDir::new().unwrap();
                let mut store = Store::open(root.path()).unwrap();
                let session = store
                    .resolve_session("property", "mime", "Properties", "/tmp/mime")
                    .unwrap();
                let blob = store.stage_publication_blob(Cursor::new(bytes)).unwrap();
                let record = store
                    .publish_at(
                        PublicationRequest {
                            session_public_id: session.public_id,
                            title: "Classified".into(),
                            commentary: "Signature contract".into(),
                            predecessor_post_id: None,
                            git: None,
                            files: vec![PublicationFile {
                                filename: format!("artifact.{extension}"),
                                caption: None,
                                blob,
                                support_assets: vec![],
                            }],
                        },
                        1,
                    )
                    .unwrap();
                let post = store.post(record.id).unwrap();
                let file = &post.files[0];
                prop_assert_eq!(file.renderer, renderer);
                prop_assert_eq!(&file.media_type, media_type);
                Ok(())
            },
        )
        .unwrap();
}

#[test]
fn generated_single_byte_ranges_return_exact_slices() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let bytes: Vec<u8> = (0..=127).collect();
    let (_root, app) = published_app(&bytes, "bytes.bin");
    let mut runner = runner(40);
    runner
        .run(&(0u64..128, 0u64..192), |(start, span)| {
            let end = start.saturating_add(span);
            let response = runtime
                .block_on(
                    app.clone().oneshot(
                        Request::builder()
                            .uri("/api/v1/posts/1/files/0/content")
                            .header(header::RANGE, format!("bytes={start}-{end}"))
                            .body(Body::empty())
                            .unwrap(),
                    ),
                )
                .unwrap();
            prop_assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            let actual_end = end.min(127);
            let body = runtime
                .block_on(response.into_body().collect())
                .unwrap()
                .to_bytes();
            prop_assert_eq!(body.as_ref(), &bytes[start as usize..=actual_end as usize]);
            Ok(())
        })
        .unwrap();
}

#[test]
fn generated_invalid_public_page_paths_are_rejected() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let app = glim::app();
    let mut runner = runner(40);
    let short_or_forbidden = prop_oneof!["[1-9A-HJ-NP-Za-km-z]{0,5}", "[0OIl_/\\%]{1,12}"];
    runner
        .run(&short_or_forbidden, |id| {
            let response = runtime
                .block_on(
                    app.clone().oneshot(
                        Request::builder()
                            .uri(format!("/sessions/{id}"))
                            .body(Body::empty())
                            .unwrap(),
                    ),
                )
                .unwrap();
            prop_assert_eq!(response.status(), StatusCode::NOT_FOUND);
            Ok(())
        })
        .unwrap();
}

#[test]
fn generated_config_and_cli_schema_versions_fail_closed() {
    let mut runner = runner(24);
    runner
        .run(
            &(0u32..=u32::MAX).prop_filter("version one is supported", |version| *version != 1),
            |version| {
                let config = format!(r#"{{"schema_version":{version}}}"#);
                prop_assert!(
                    glim::daemon::resolve_daemon_configuration_limit_values(
                        Some(config.as_bytes()),
                        None,
                        None
                    )
                    .is_err()
                );

                let mut child = Command::new(env!("CARGO_BIN_EXE_glim"))
                    .args(["publish", "--json"])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap();
                use std::io::Write;
                write!(
                    child.stdin.take().unwrap(),
                    r#"{{"schema_version":{version}}}"#
                )
                .unwrap();
                let output = child.wait_with_output().unwrap();
                prop_assert!(!output.status.success());
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                prop_assert_eq!(
                    value["error"]["code"].as_str(),
                    Some("unsupported_schema_version")
                );
                Ok(())
            },
        )
        .unwrap();
}
