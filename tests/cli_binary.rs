use std::{
    fs,
    io::Write,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[cfg(unix)]
use std::{ffi::OsString, os::unix::ffi::OsStringExt, os::unix::fs::symlink};

use serde_json::{Value, json};
use tempfile::TempDir;

fn run_glim(
    args: &[&str],
    input: Option<&Value>,
    daemon_url: &str,
    browser: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glim"));
    command
        .args(args)
        .env("GLIM_DAEMON_URL", daemon_url)
        .stdin(Stdio::piped());
    if let Some(browser) = browser {
        command.env("GLIM_BROWSER_COMMAND", browser);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn one_json(output: &std::process::Output) -> Value {
    assert!(
        output.stderr.is_empty(),
        "stderr was {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout.clone()).unwrap();
    assert_eq!(text.lines().count(), 1, "output was {text:?}");
    serde_json::from_str(text.trim()).unwrap()
}

struct DaemonProcess(Child);

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_daemon_process(store_root: &std::path::Path) -> DaemonProcess {
    let port = std::net::TcpListener::bind("127.0.0.1:3030")
        .expect("real-process CLI regression requires port 3030");
    drop(port);
    let child = Command::new(env!("CARGO_BIN_EXE_glim"))
        .arg("daemon")
        .env("GLIM_STORE_ROOT", store_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let daemon = DaemonProcess(child);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if reqwest::get("http://127.0.0.1:3030/api/v1/health")
            .await
            .is_ok()
        {
            return daemon;
        }
        assert!(Instant::now() < deadline, "daemon did not become ready");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn canonical_and_flag_publication_share_the_streaming_contract_and_open_is_explicit() {
    let store_root = TempDir::new().unwrap();
    let store = glim::storage::Store::open(store_root.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, glim::app_with_store(store))
            .await
            .unwrap();
    });
    let daemon_url = format!("http://{address}");
    let files = TempDir::new().unwrap();
    fs::write(files.path().join("one.txt"), b"one").unwrap();
    fs::write(files.path().join("two.txt"), b"two").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "CLI Test"],
        vec!["config", "user.email", "cli@example.invalid"],
        vec!["add", "one.txt", "two.txt"],
        vec!["commit", "-qm", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(files.path())
                .status()
                .unwrap()
                .success()
        );
    }
    let marker = files.path().join("browser-called");
    let browser = files.path().join("browser.sh");
    fs::write(
        &browser,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", marker.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&browser, fs::Permissions::from_mode(0o700)).unwrap();
    }

    let input = json!({
        "schema_version": 1,
        "integration_namespace": "pi",
        "external_session_key": "cli-session",
        "project_label": "Glim",
        "working_directory": files.path(),
        "title": "Ordered files",
        "commentary": "first line\n\nsecond line",
        "files": [
            {"source_path": files.path().join("one.txt"), "caption": "one"},
            {"source_path": files.path().join("two.txt"), "published_filename": "renamed.txt", "caption": "two", "media_type": "text/plain"}
        ]
    });
    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        &daemon_url,
        Some(browser.to_str().unwrap()),
    );
    assert!(output.status.success());
    let payload = one_json(&output);
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["ok"], true);
    assert_eq!(
        payload["result"]["post"]["commentary"],
        "first line\n\nsecond line"
    );
    assert_eq!(payload["result"]["post"]["files"][0]["caption"], "one");
    assert_eq!(
        payload["result"]["post"]["files"][1]["filename"],
        "renamed.txt"
    );
    assert_eq!(
        payload["result"]["post"]["git"]["root"],
        files
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .as_ref()
    );
    assert!(payload["result"]["post"]["git"]["branch"].is_string());
    assert!(payload["result"]["post"]["git"]["commit"].is_string());
    assert!(payload["result"]["post"]["git"].get("remote").is_none());
    assert!(payload["result"]["post"]["git"].get("diff").is_none());
    assert_eq!(
        payload["result"]["viewer_url"],
        format!(
            "{daemon_url}/sessions/{}",
            payload["result"]["session"]["public_id"].as_str().unwrap()
        )
    );
    assert_eq!(
        payload["result"]["post_url"],
        format!(
            "{daemon_url}/sessions/{}#post-{}",
            payload["result"]["session"]["public_id"].as_str().unwrap(),
            payload["result"]["post"]["id"].as_i64().unwrap()
        )
    );
    assert!(!marker.exists(), "browser launched without --open");

    let revision_input = json!({
        "schema_version": 1,
        "integration_namespace": "pi",
        "external_session_key": "cli-session",
        "project_label": "Glim",
        "working_directory": files.path(),
        "title": "Revision",
        "commentary": "Revised result",
        "predecessor_post_id": payload["result"]["post"]["id"],
        "files": [{"source_path": files.path().join("one.txt")}]
    });
    let revision = run_glim(
        &["publish", "--json"],
        Some(&revision_input),
        &daemon_url,
        None,
    );
    assert!(revision.status.success());
    assert_eq!(
        one_json(&revision)["result"]["post"]["predecessor_post_id"],
        payload["result"]["post"]["id"]
    );

    let commentary = files.path().join("commentary.md");
    fs::write(&commentary, "flag\ncommentary").unwrap();
    let output = run_glim(
        &[
            "publish",
            "--file",
            files.path().join("one.txt").to_str().unwrap(),
            "--integration",
            "pi",
            "--external-key",
            "flag-session",
            "--project",
            "Glim",
            "--working-directory",
            files.path().to_str().unwrap(),
            "--title",
            "Flags",
            "--commentary-file",
            commentary.to_str().unwrap(),
            "--caption",
            "single",
            "--open",
        ],
        None,
        &daemon_url,
        Some(browser.to_str().unwrap()),
    );
    assert!(output.status.success());
    let flag_payload = one_json(&output);
    assert_eq!(
        flag_payload["result"]["post"]["commentary"],
        "flag\ncommentary"
    );
    assert_eq!(
        fs::read_to_string(marker).unwrap(),
        flag_payload["result"]["viewer_url"]
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn read_and_lifecycle_commands_use_versioned_urls_and_one_json_result() {
    let root = TempDir::new().unwrap();
    let mut store = glim::storage::Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "commands", "Glim", "/tmp")
        .unwrap();
    let public_id = session.public_id;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, glim::app_with_store(store))
            .await
            .unwrap();
    });
    let url = format!("http://{address}");
    for args in [
        vec!["status"],
        vec!["list", "--session", &public_id, "--limit", "1"],
        vec!["close", &public_id],
    ] {
        let output = run_glim(&args, None, &url, None);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(one_json(&output)["ok"], true);
    }
    let output = run_glim(&["show", "999"], None, &url, None);
    assert!(!output.status.success());
    let error = one_json(&output);
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "post_not_found");
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn publish_open_failure_preserves_committed_success_and_prevents_retry() {
    let root = TempDir::new().unwrap();
    let store = glim::storage::Store::open(root.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, glim::app_with_store(store))
            .await
            .unwrap();
    });
    let files = TempDir::new().unwrap();
    fs::write(files.path().join("result.txt"), b"result").unwrap();
    let browser = files.path().join("fail-browser.sh");
    fs::write(&browser, "#!/bin/sh\nexit 7\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&browser, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let input = json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":"open-failure",
        "project_label":"Glim", "working_directory":files.path(), "title":"Committed",
        "commentary":"Browser failure must not hide success", "files":[{"source_path":files.path().join("result.txt")}]
    });

    let output = run_glim(
        &["publish", "--json", "--open"],
        Some(&input),
        &format!("http://{address}"),
        Some(browser.to_str().unwrap()),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let payload = one_json(&output);
    assert_eq!(payload["ok"], true);
    assert!(payload["result"]["post"]["id"].as_i64().is_some());
    assert!(payload["result"]["viewer_url"].is_string());
    assert!(payload["result"]["post_url"].is_string());
    assert_eq!(payload["result"]["browser_launch"]["requested"], true);
    assert_eq!(payload["result"]["browser_launch"]["opened"], false);
    assert!(payload["result"]["browser_launch"]["error"].is_object());
    let connection = rusqlite::Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn large_markdown_publishes_and_streams_collected_support_bytes() {
    let root = TempDir::new().unwrap();
    let store = glim::storage::Store::open(root.path()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, glim::app_with_store(store))
            .await
            .unwrap();
    });
    let files = TempDir::new().unwrap();
    fs::write(files.path().join("image.png"), b"\x89PNG\r\n\x1a\nstreamed").unwrap();
    let mut markdown = fs::File::create(files.path().join("large.md")).unwrap();
    markdown.write_all(&vec![b'a'; 3 * 1024 * 1024]).unwrap();
    markdown.write_all(b"\n![image](image.png)\n").unwrap();
    let input = json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":"large",
        "project_label":"Glim", "working_directory":files.path(), "title":"Large",
        "commentary":"Stream the entry", "files":[{"source_path":files.path().join("large.md")}]
    });

    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        &format!("http://{address}"),
        None,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let payload = one_json(&output);
    assert_eq!(
        payload["result"]["post"]["files"][0]["blob"]["byte_size"],
        3 * 1024 * 1024 + 21
    );
    assert_eq!(
        payload["result"]["post"]["files"][0]["support_assets"][0]["relative_path"],
        "image.png"
    );
    let post_id = payload["result"]["post"]["id"].as_i64().unwrap();
    let bytes = reqwest::get(format!(
        "http://{address}/api/v1/posts/{post_id}/files/0/support/image.png"
    ))
    .await
    .unwrap()
    .bytes()
    .await
    .unwrap();
    assert_eq!(bytes.as_ref(), b"\x89PNG\r\n\x1a\nstreamed");
    server.abort();
}

fn publication_input(files: &TempDir, external_key: &str) -> Value {
    fs::write(files.path().join("result.txt"), b"result").unwrap();
    json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":external_key,
        "project_label":"Glim", "working_directory":files.path(), "title":"Maybe committed",
        "commentary":"Do not retry blindly", "files":[{"source_path":files.path().join("result.txt")}]
    })
}

fn complete_created_response(public_id: &str, post_id: i64, post_session_id: &str) -> Value {
    json!({
        "session": {
            "id": 1, "public_id": public_id, "integration_namespace": "pi",
            "external_key": "response", "project": {"id": 1, "label": "Glim", "working_directory": "/tmp"},
            "created_at": 1, "last_activity_at": 1
        },
        "post": {
            "id": post_id, "session_id": 1, "session_public_id": post_session_id,
            "title": "Result", "commentary": "Published", "predecessor_post_id": null,
            "published_at": 1, "git": null, "files": []
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn real_process_publication_streams_recursive_css_support_bytes_from_offset_zero() {
    let store = TempDir::new().unwrap();
    let _daemon = start_daemon_process(store.path()).await;
    let files = TempDir::new().unwrap();
    fs::create_dir_all(files.path().join("styles/nested")).unwrap();
    fs::create_dir_all(files.path().join("images")).unwrap();
    fs::create_dir_all(files.path().join("fonts")).unwrap();
    let assets = [
        (
            "styles/main.css",
            b"@import \"nested/theme.css\"; .hero { background: url('../images/main.png') }".as_slice(),
        ),
        (
            "styles/nested/theme.css",
            b"@font-face { src: url('../../fonts/site.woff2') } .nested { background: url('../../images/nested.png') }".as_slice(),
        ),
        ("fonts/site.woff2", b"wOF2exact-font".as_slice()),
        ("images/nested.png", b"\x89PNG\r\n\x1a\nnested".as_slice()),
        ("images/main.png", b"\x89PNG\r\n\x1a\nmain".as_slice()),
    ];
    let entry = b"<html><head><link rel=\"stylesheet\" href=\"styles/main.css\"></head><body>Result</body></html>";
    fs::write(files.path().join("entry.html"), entry).unwrap();
    for (path, bytes) in assets {
        fs::write(files.path().join(path), bytes).unwrap();
    }
    let input = json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":"recursive-css",
        "project_label":"Glim", "working_directory":files.path(), "title":"Recursive CSS",
        "commentary":"Retained handles start at zero", "files":[{"source_path":files.path().join("entry.html")}]
    });

    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        "http://127.0.0.1:3030",
        None,
    );
    assert!(
        output.status.success(),
        "publication failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let payload = one_json(&output);
    let post_id = payload["result"]["post"]["id"].as_i64().unwrap();
    let stored = payload["result"]["post"]["files"][0]["support_assets"]
        .as_array()
        .unwrap();
    assert_eq!(
        stored
            .iter()
            .map(|asset| asset["relative_path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        assets.iter().map(|(path, _)| *path).collect::<Vec<_>>()
    );
    let visible = reqwest::get(format!(
        "http://127.0.0.1:3030/api/v1/posts/{post_id}/files/0/content"
    ))
    .await
    .unwrap();
    assert_eq!(visible.status(), reqwest::StatusCode::OK);
    assert_eq!(visible.bytes().await.unwrap().as_ref(), entry);
    for (path, expected) in assets {
        let response = reqwest::get(format!(
            "http://127.0.0.1:3030/api/v1/posts/{post_id}/files/0/support/{path}"
        ))
        .await
        .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(response.bytes().await.unwrap().as_ref(), expected, "{path}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_created_response_warns_that_publication_may_have_succeeded() {
    let app = axum::Router::new().route(
        "/api/v1/posts",
        axum::routing::post(|| async {
            (
                axum::http::StatusCode::CREATED,
                axum::Json(json!({"unexpected":true})),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let files = TempDir::new().unwrap();
    fs::write(files.path().join("result.txt"), b"result").unwrap();
    let input = json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":"malformed-201",
        "project_label":"Glim", "working_directory":files.path(), "title":"Maybe committed",
        "commentary":"Do not retry blindly", "files":[{"source_path":files.path().join("result.txt")}]
    });
    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        &format!("http://{address}"),
        None,
    );
    assert!(!output.status.success());
    let payload = one_json(&output);
    assert_eq!(payload["error"]["code"], "malformed_daemon_response");
    assert_eq!(
        payload["error"]["details"]["publication_may_have_succeeded"],
        true
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn created_response_requires_safe_matching_ids_and_complete_structure() {
    let cases = [
        (
            "path-public-id",
            complete_created_response("abc/def", 1, "abc/def"),
        ),
        (
            "non-base58-public-id",
            complete_created_response("0abc12", 1, "0abc12"),
        ),
        (
            "zero-post-id",
            complete_created_response("abc123", 0, "abc123"),
        ),
        (
            "negative-post-id",
            complete_created_response("abc123", -1, "abc123"),
        ),
        (
            "mismatched-session-id",
            complete_created_response("abc123", 1, "def456"),
        ),
        (
            "missing-session-project",
            json!({
                "session": {"id":1,"public_id":"abc123","integration_namespace":"pi","external_key":"response","created_at":1,"last_activity_at":1},
                "post": complete_created_response("abc123", 1, "abc123")["post"]
            }),
        ),
        (
            "missing-post-files",
            json!({
                "session": complete_created_response("abc123", 1, "abc123")["session"],
                "post": {"id":1,"session_id":1,"session_public_id":"abc123","title":"Result","commentary":"Published","predecessor_post_id":null,"published_at":1,"git":null}
            }),
        ),
    ];

    for (name, response) in cases {
        let app = axum::Router::new().route(
            "/api/v1/posts",
            axum::routing::post(move || {
                let response = response.clone();
                async move { (axum::http::StatusCode::CREATED, axum::Json(response)) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let files = TempDir::new().unwrap();
        let input = publication_input(&files, name);

        let output = run_glim(
            &["publish", "--json"],
            Some(&input),
            &format!("http://{address}"),
            None,
        );
        assert!(
            !output.status.success(),
            "case {name} unexpectedly succeeded"
        );
        let payload = one_json(&output);
        assert_eq!(
            payload["error"]["code"], "malformed_daemon_response",
            "case {name}: {payload}"
        );
        assert_eq!(
            payload["error"]["details"]["publication_may_have_succeeded"], true,
            "case {name}: {payload}"
        );
        server.abort();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn consumed_publication_with_lost_response_is_ambiguous() {
    use tokio::io::AsyncReadExt;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let body_start = loop {
            let mut chunk = [0_u8; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0, "client closed before sending HTTP headers");
            request.extend_from_slice(&chunk[..count]);
            if let Some(index) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = std::str::from_utf8(&request[..body_start]).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap();
        while request.len() - body_start < content_length {
            let mut chunk = [0_u8; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0, "client closed before sending multipart body");
            request.extend_from_slice(&chunk[..count]);
        }
        assert!(
            request[body_start..]
                .windows(4)
                .any(|window| window == b"name")
        );
        drop(socket);
    });
    let files = TempDir::new().unwrap();
    let input = publication_input(&files, "lost-response");

    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        &format!("http://{address}"),
        None,
    );
    assert!(!output.status.success());
    let payload = one_json(&output);
    assert_eq!(payload["error"]["code"], "daemon_unavailable");
    assert_eq!(
        payload["error"]["details"]["publication_may_have_succeeded"],
        true
    );
    server.await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_created_response_and_daemon_rejection_are_structured() {
    let files = TempDir::new().unwrap();
    fs::write(files.path().join("result.txt"), b"result").unwrap();
    let input = json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":"responses",
        "project_label":"Glim", "working_directory":files.path(), "title":"Response",
        "commentary":"Bound response handling", "files":[{"source_path":files.path().join("result.txt")}]
    });

    let oversized = axum::Router::new().route(
        "/api/v1/posts",
        axum::routing::post(|| async {
            axum::http::Response::builder()
                .status(201)
                .header("content-type", "application/json")
                .body(axum::body::Body::from(format!(
                    "\"{}\"",
                    "x".repeat(1024 * 1024 + 1)
                )))
                .unwrap()
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, oversized).await.unwrap();
    });
    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        &format!("http://{address}"),
        None,
    );
    assert!(!output.status.success());
    let payload = one_json(&output);
    assert_eq!(payload["error"]["code"], "daemon_response_too_large");
    assert_eq!(
        payload["error"]["details"]["publication_may_have_succeeded"],
        true
    );
    server.abort();

    let rejected = axum::Router::new().route("/api/v1/posts", axum::routing::post(|| async {
        (axum::http::StatusCode::UNPROCESSABLE_ENTITY, axum::Json(json!({"error":{"code":"validation_failed","message":"Rejected","details":{"field":"title"}}})))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, rejected).await.unwrap();
    });
    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        &format!("http://{address}"),
        None,
    );
    assert!(!output.status.success());
    let payload = one_json(&output);
    assert_eq!(payload["error"]["code"], "validation_failed");
    assert_eq!(payload["error"]["details"]["field"], "title");
    assert_eq!(payload["error"]["details"]["http_status"], 422);
    server.abort();
}

#[test]
fn open_browser_helper_is_bounded_and_accepts_a_still_running_opener() {
    let files = TempDir::new().unwrap();
    let browser = files.path().join("hang-browser.sh");
    fs::write(&browser, "#!/bin/sh\nsleep 2\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&browser, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let started = Instant::now();
    let output = run_glim(
        &["open", "abc123"],
        None,
        "http://127.0.0.1:3030",
        Some(browser.to_str().unwrap()),
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(output.status.success());
    assert_eq!(one_json(&output)["ok"], true);
}

#[test]
fn open_browser_helper_still_reports_an_immediate_nonzero_exit() {
    let files = TempDir::new().unwrap();
    let browser = files.path().join("fail-browser.sh");
    fs::write(&browser, "#!/bin/sh\nexit 7\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&browser, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let output = run_glim(
        &["open", "abc123"],
        None,
        "http://127.0.0.1:3030",
        Some(browser.to_str().unwrap()),
    );
    assert!(!output.status.success());
    assert_eq!(one_json(&output)["error"]["code"], "browser_launch_failed");
}

#[cfg(unix)]
#[test]
fn publication_rejects_a_non_utf8_canonical_working_directory() {
    let parent = TempDir::new().unwrap();
    let invalid = parent
        .path()
        .join(OsString::from_vec(b"project-\xff".to_vec()));
    fs::create_dir(&invalid).unwrap();
    let alias = parent.path().join("project-link");
    symlink(&invalid, &alias).unwrap();
    let source = parent.path().join("result.txt");
    fs::write(&source, b"result").unwrap();
    let input = json!({
        "schema_version":1, "integration_namespace":"pi", "external_session_key":"non-utf8",
        "project_label":"Glim", "working_directory":alias, "title":"Identity",
        "commentary":"Exact path", "files":[{"source_path":source}]
    });

    let output = run_glim(
        &["publish", "--json"],
        Some(&input),
        "http://127.0.0.1:9",
        None,
    );
    assert!(!output.status.success());
    assert_eq!(one_json(&output)["error"]["code"], "validation_error");
}

#[test]
fn malformed_usage_stdin_and_unavailable_daemon_are_json_errors() {
    for (args, input, code) in [
        (
            vec!["publish", "--json"],
            Some(json!({"schema_version":2})),
            "unsupported_schema_version",
        ),
        (
            vec!["publish", "--json"],
            Some(json!({"schema_version":1,"unknown":true})),
            "invalid_publication_json",
        ),
        (
            vec!["list", "--global", "--limit", "1", "--limit", "2"],
            None,
            "usage_error",
        ),
        (vec!["publish", "--unknown"], None, "usage_error"),
        (vec!["status"], None, "daemon_unavailable"),
    ] {
        let output = run_glim(&args, input.as_ref(), "http://127.0.0.1:9", None);
        assert!(!output.status.success());
        assert_eq!(one_json(&output)["error"]["code"], code);
    }
    let https = run_glim(&["status"], None, "https://127.0.0.1:3030", None);
    assert!(!https.status.success());
    assert_eq!(one_json(&https)["error"]["code"], "configuration_error");
    for invalid in [
        "http://user@example.test",
        "http://example.test/path",
        "http://example.test?query=1",
        "http://example.test#fragment",
    ] {
        let output = run_glim(&["status"], None, invalid, None);
        assert!(!output.status.success());
        assert_eq!(
            one_json(&output)["error"]["code"],
            "configuration_error",
            "{invalid}"
        );
    }
}
