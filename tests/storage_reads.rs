use std::io::Cursor;

use glim::storage::{
    DEFAULT_PAGE_LIMIT, MAX_PAGE_LIMIT, PageRequest, PublicationFile, PublicationRequest,
    PublicationSupportAsset, Store, StoreError,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn publish(store: &mut Store, session: &str, title: &str, at: i64) -> i64 {
    let blob = store
        .stage_publication_blob(Cursor::new(title.as_bytes()))
        .unwrap();
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.to_owned(),
                title: title.to_owned(),
                commentary: format!("line one\n\n{title}"),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: format!("{title}.txt"),
                    caption: Some(format!("caption\n{title}")),
                    blob,
                    support_assets: vec![],
                }],
            },
            at,
        )
        .unwrap()
        .id
}

#[test]
fn reconstructs_session_and_nested_immutable_post_without_refreshing_activity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "external", "Project", "/work/project")
        .unwrap();
    let visible = store
        .stage_publication_blob(Cursor::new(b"visible"))
        .unwrap();
    let z_asset = store.stage_publication_blob(Cursor::new(b"z")).unwrap();
    let a_asset = store.stage_publication_blob(Cursor::new(b"asset")).unwrap();
    let second = store
        .stage_publication_blob(Cursor::new(b"second"))
        .unwrap();
    let first = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Original".into(),
                commentary: "first\n\nsecond".into(),
                predecessor_post_id: None,
                files: vec![
                    PublicationFile {
                        filename: "entry.md".into(),
                        caption: Some("caption\ncontinued".into()),
                        blob: visible,
                        support_assets: vec![
                            PublicationSupportAsset {
                                relative_path: "z.png".into(),
                                blob: z_asset,
                            },
                            PublicationSupportAsset {
                                relative_path: "a.png".into(),
                                blob: a_asset,
                            },
                        ],
                    },
                    PublicationFile {
                        filename: "second.csv".into(),
                        caption: None,
                        blob: second,
                        support_assets: vec![],
                    },
                ],
            },
            200,
        )
        .unwrap();
    let revision = PublicationRequest {
        session_public_id: session.public_id.clone(),
        title: "Revision".into(),
        commentary: "revised".into(),
        predecessor_post_id: Some(first.id),
        files: vec![PublicationFile {
            filename: "revision.txt".into(),
            caption: None,
            blob: store
                .stage_publication_blob(Cursor::new(b"revision"))
                .unwrap(),
            support_assets: vec![],
        }],
    };
    let revision = store.publish_at(revision, 201).unwrap();
    Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at = 123 WHERE public_id = ?1",
            [&session.public_id],
        )
        .unwrap();

    let found_session = store.session(&session.public_id).unwrap();
    assert_eq!(found_session.integration_namespace, "pi");
    assert_eq!(found_session.external_key, "external");
    assert_eq!(found_session.project.label, "Project");
    assert_eq!(found_session.project.working_directory, "/work/project");
    assert_eq!(found_session.last_activity_at, 123);

    let post = store.post(first.id).unwrap();
    assert_eq!(post.commentary, "first\n\nsecond");
    assert_eq!(
        post.files
            .iter()
            .map(|f| f.filename.as_str())
            .collect::<Vec<_>>(),
        ["entry.md", "second.csv"]
    );
    assert_eq!(post.files[0].caption.as_deref(), Some("caption\ncontinued"));
    assert_eq!(post.files[0].blob.byte_size, 7);
    assert_eq!(
        post.files[0]
            .support_assets
            .iter()
            .map(|a| a.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["z.png", "a.png"]
    );
    assert_eq!(post.files[0].support_assets[0].blob.byte_size, 1);
    assert_eq!(
        store.post(revision.id).unwrap().predecessor_post_id,
        Some(first.id)
    );
    assert_eq!(
        store.session(&session.public_id).unwrap().last_activity_at,
        123
    );

    assert!(matches!(
        store.session("missing"),
        Err(StoreError::SessionNotFound { .. })
    ));
    assert!(matches!(
        store.post(999),
        Err(StoreError::PostNotFound { post_id: 999 })
    ));
}

#[test]
fn corrupt_negative_blob_sizes_return_typed_errors_without_panicking() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "corrupt", "Corrupt", "/corrupt")
        .unwrap();
    let visible = store.stage_publication_blob(Cursor::new(b"v")).unwrap();
    let support = store.stage_publication_blob(Cursor::new(b"s")).unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Corrupt".into(),
                commentary: "Metadata".into(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "entry.md".into(),
                    caption: None,
                    blob: visible,
                    support_assets: vec![PublicationSupportAsset {
                        relative_path: "asset.bin".into(),
                        blob: support,
                    }],
                }],
            },
            10,
        )
        .unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection
        .execute_batch("PRAGMA ignore_check_constraints = ON;")
        .unwrap();
    let visible_hash = connection.query_row(
        "SELECT r.blob_hash FROM post_files f JOIN blob_references r ON r.id = f.blob_reference_id WHERE f.post_id = ?1",
        [post.id],
        |row| row.get::<_, String>(0),
    ).unwrap();
    let support_hash = connection.query_row(
        "SELECT r.blob_hash FROM support_assets a JOIN blob_references r ON r.id = a.blob_reference_id WHERE a.post_id = ?1",
        [post.id],
        |row| row.get::<_, String>(0),
    ).unwrap();

    connection
        .execute(
            "UPDATE blobs SET byte_size = -1 WHERE hash = ?1",
            [&support_hash],
        )
        .unwrap();
    let support_error = store.post(post.id).unwrap_err();
    assert!(
        matches!(support_error, StoreError::Io(ref source) if source.kind() == std::io::ErrorKind::InvalidData)
    );

    connection
        .execute(
            "UPDATE blobs SET byte_size = 1 WHERE hash = ?1",
            [&support_hash],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE blobs SET byte_size = -1 WHERE hash = ?1",
            [&visible_hash],
        )
        .unwrap();
    let visible_error = store.post(post.id).unwrap_err();
    assert!(
        matches!(visible_error, StoreError::Io(ref source) if source.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_eq!(
        store.session(&session.public_id).unwrap().public_id,
        session.public_id
    );
}

#[test]
fn default_and_maximum_page_sizes_are_enforced_by_results() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "pages", "Pages", "/pages")
        .unwrap();
    for index in 0..25 {
        publish(
            &mut store,
            &session.public_id,
            &format!("post-{index}"),
            i64::from(index),
        );
    }

    let default_page = store.global_posts(PageRequest::default()).unwrap();
    assert_eq!(default_page.posts.len(), DEFAULT_PAGE_LIMIT as usize);
    assert!(default_page.next_cursor.is_some());
    let maximum_page = store
        .global_posts(PageRequest {
            limit: Some(MAX_PAGE_LIMIT),
            cursor: None,
        })
        .unwrap();
    assert_eq!(maximum_page.posts.len(), 25);
    assert!(maximum_page.next_cursor.is_none());
}

#[test]
fn scoped_pages_are_isolated_stable_and_bounded() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let a1 = store.resolve_session("pi", "a1", "A", "/a").unwrap();
    let a2 = store.resolve_session("pi", "a2", "A", "/a").unwrap();
    let b = store.resolve_session("pi", "b", "B", "/b").unwrap();
    Connection::open(root.path().join("metadata.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE sessions SET last_activity_at = 123 WHERE public_id = ?1",
            [&a1.public_id],
        )
        .unwrap();
    let ids = [
        publish(&mut store, &a1.public_id, "old", 9),
        publish(&mut store, &a1.public_id, "equal-one", 10),
        publish(&mut store, &a2.public_id, "equal-two", 10),
        publish(&mut store, &b.public_id, "other", 11),
    ];

    let first = store
        .global_posts(PageRequest {
            limit: Some(2),
            cursor: None,
        })
        .unwrap();
    assert_eq!(
        first.posts.iter().map(|p| p.id).collect::<Vec<_>>(),
        [ids[3], ids[2]]
    );
    let second = store
        .global_posts(PageRequest {
            limit: Some(2),
            cursor: first.next_cursor,
        })
        .unwrap();
    assert_eq!(
        second.posts.iter().map(|p| p.id).collect::<Vec<_>>(),
        [ids[1], ids[0]]
    );

    let session_page = store
        .session_posts(&a1.public_id, PageRequest::default())
        .unwrap();
    assert_eq!(
        session_page.posts.iter().map(|p| p.id).collect::<Vec<_>>(),
        [ids[1], ids[0]]
    );
    let project_page = store
        .project_posts(a1.project_id, PageRequest::default())
        .unwrap();
    assert_eq!(
        project_page.posts.iter().map(|p| p.id).collect::<Vec<_>>(),
        [ids[2], ids[1], ids[0]]
    );
    assert!(matches!(
        store.project_posts(999, PageRequest::default()),
        Err(StoreError::ProjectNotFound { project_id: 999 })
    ));

    assert_eq!(DEFAULT_PAGE_LIMIT, 20);
    assert_eq!(MAX_PAGE_LIMIT, 100);
    assert!(matches!(
        store.global_posts(PageRequest {
            limit: Some(0),
            cursor: None
        }),
        Err(StoreError::InvalidPageLimit { .. })
    ));
    assert!(matches!(
        store.global_posts(PageRequest {
            limit: Some(101),
            cursor: None
        }),
        Err(StoreError::InvalidPageLimit { .. })
    ));
    assert!(matches!(
        store.global_posts(PageRequest {
            limit: None,
            cursor: Some("bad".into())
        }),
        Err(StoreError::InvalidPageCursor)
    ));
    let activity_before_reads = store.session(&a1.public_id).unwrap().last_activity_at;
    store
        .session_posts(&a1.public_id, PageRequest::default())
        .unwrap();
    store
        .project_posts(a1.project_id, PageRequest::default())
        .unwrap();
    store.global_posts(PageRequest::default()).unwrap();
    assert_eq!(
        store.session(&a1.public_id).unwrap().last_activity_at,
        activity_before_reads
    );
}
