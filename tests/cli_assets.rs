use std::{
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
};

use glim::cli::collect_support_assets;
use tempfile::TempDir;

#[test]
fn markdown_and_html_assets_preserve_first_use_order_and_normalized_paths() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("images")).unwrap();
    fs::write(root.path().join("images/one.png"), b"one").unwrap();
    fs::write(root.path().join("images/two.svg"), b"<svg/>").unwrap();
    fs::write(root.path().join("app.js"), b"console.log(1)").unwrap();
    fs::write(
        root.path().join("entry.md"),
        "![one](images/one.png?size=2#x) ![again](images/one.png) <img src=\"images/two.svg\">",
    )
    .unwrap();
    fs::write(
        root.path().join("entry.html"),
        "<script src=\"app.js\"></script><img srcset=\"images/two.svg 1x, images/one.png 2x\"><img src=\"https://example.test/x.png\"><img src=\"//example.test/x.png\"><img src=\"data:image/png,x\">",
    )
    .unwrap();

    let markdown = collect_support_assets(&root.path().join("entry.md")).unwrap();
    assert_eq!(
        markdown
            .iter()
            .map(|asset| asset.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["images/one.png", "images/two.svg"]
    );
    let html = collect_support_assets(&root.path().join("entry.html")).unwrap();
    assert_eq!(
        html.iter()
            .map(|asset| asset.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["app.js", "images/two.svg", "images/one.png"]
    );
}

#[test]
fn non_document_artifact_is_not_interpreted_as_utf8() {
    let root = TempDir::new().unwrap();
    let entry = root.path().join("plot.png");
    fs::write(&entry, b"\x89PNG\r\n\x1a\n\xff").unwrap();

    let assets = collect_support_assets(&entry).unwrap();

    assert!(assets.is_empty());
}

#[test]
fn markdown_and_html_entries_still_require_utf8() {
    let root = TempDir::new().unwrap();
    for name in ["entry.md", "entry.html"] {
        let entry = root.path().join(name);
        fs::write(&entry, b"text\xff").unwrap();
        let error = collect_support_assets(&entry).unwrap_err();
        assert!(
            error.contains("entry file must be UTF-8 text"),
            "{name}: {error}"
        );
    }
}

#[test]
fn large_markdown_has_no_text_specific_collection_limit() {
    let root = TempDir::new().unwrap();
    fs::write(root.path().join("image.png"), b"image").unwrap();
    let mut entry = fs::File::create(root.path().join("large.md")).unwrap();
    entry.write_all(&vec![b'a'; 3 * 1024 * 1024]).unwrap();
    entry.write_all(b"\n![image](image.png)\n").unwrap();

    let assets = collect_support_assets(&root.path().join("large.md")).unwrap();
    assert_eq!(
        assets
            .iter()
            .map(|asset| asset.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["image.png"]
    );
}

#[test]
fn html_stylesheet_recursively_collects_css_closure_in_first_use_order() {
    let root = TempDir::new().unwrap();
    fs::create_dir_all(root.path().join("styles/nested")).unwrap();
    fs::create_dir_all(root.path().join("images")).unwrap();
    fs::create_dir_all(root.path().join("fonts")).unwrap();
    fs::write(
        root.path().join("entry.html"),
        r#"<link rel="stylesheet" href="styles/main.css">"#,
    )
    .unwrap();
    fs::write(root.path().join("styles/main.css"), r#"@import "nested/theme.css"; .hero { background: url('../images/main.png') } .again { background: url('../images/main.png') }"#).unwrap();
    fs::write(root.path().join("styles/nested/theme.css"), r#"@import url('../main.css'); @font-face { src: url('../../fonts/site.woff2') } .theme { background: url('../../images/theme.png') } .remote { background: url(https://example.test/x.png) } .data { background: url(data:image/png,x) }"#).unwrap();
    fs::write(root.path().join("images/main.png"), b"main").unwrap();
    fs::write(root.path().join("images/theme.png"), b"theme").unwrap();
    fs::write(root.path().join("fonts/site.woff2"), b"wOF2font").unwrap();

    let assets = collect_support_assets(&root.path().join("entry.html")).unwrap();
    assert_eq!(
        assets
            .iter()
            .map(|asset| asset.relative_path.as_str())
            .collect::<Vec<_>>(),
        [
            "styles/main.css",
            "styles/nested/theme.css",
            "fonts/site.woff2",
            "images/theme.png",
            "images/main.png",
        ]
    );
}

#[test]
fn nested_css_escape_rejects_the_entry_before_publication() {
    let root = TempDir::new().unwrap();
    fs::write(
        root.path().join("entry.html"),
        r#"<link rel="stylesheet" href="style.css">"#,
    )
    .unwrap();
    fs::write(
        root.path().join("style.css"),
        ".x { background: url('../outside.png') }",
    )
    .unwrap();
    assert!(collect_support_assets(&root.path().join("entry.html")).is_err());
}

#[test]
fn unsafe_or_unsupported_local_assets_reject_the_whole_entry() {
    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("outside.png"), b"outside").unwrap();
    symlink(
        outside.path().join("outside.png"),
        root.path().join("escape.png"),
    )
    .unwrap();
    fs::write(root.path().join("unsupported.exe"), b"x").unwrap();
    fs::create_dir(root.path().join("directory.png")).unwrap();
    let fifo = root.path().join("pipe.png");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(status.success());

    for (name, reference) in [
        ("parent", "../outside.png"),
        ("encoded_parent", "%2e%2e/outside.png"),
        ("encoded_separator", "images%2fone.png"),
        ("absolute", "/tmp/outside.png"),
        ("file_url", "file:///tmp/outside.png"),
        ("windows_absolute", "C:\\\\outside.png"),
        ("malformed", "%zz.png"),
        ("symlink", "escape.png"),
        ("unsupported", "unsupported.exe"),
        ("directory", "directory.png"),
        ("special", "pipe.png"),
    ] {
        let entry = root.path().join(format!("{name}.md"));
        fs::write(&entry, format!("![x]({reference})")).unwrap();
        assert!(collect_support_assets(&entry).is_err(), "accepted {name}");
    }

    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
}
