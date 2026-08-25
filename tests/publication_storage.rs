use std::{
    fs,
    io::{self, Cursor, Read},
    os::unix::fs::PermissionsExt,
    sync::{Arc, Barrier, mpsc},
    thread,
    time::Duration,
};

use glim::storage::{
    PublicationFile, PublicationIdentity, PublicationRequest, PublicationSupportAsset, Store,
    StoreError, StoreLimits,
};
use rusqlite::Connection;
use tempfile::TempDir;

fn database(root: &TempDir) -> Connection {
    Connection::open(root.path().join("metadata.sqlite3")).unwrap()
}

fn one_file_request(
    store: &Store,
    session_public_id: &str,
    title: &str,
    commentary: &str,
    bytes: &[u8],
) -> PublicationRequest {
    PublicationRequest {
        session_public_id: session_public_id.to_owned(),
        title: title.to_owned(),
        commentary: commentary.to_owned(),
        predecessor_post_id: None,
        files: vec![PublicationFile {
            filename: "artifact.bin".to_owned(),
            caption: None,
            blob: store.stage_publication_blob(Cursor::new(bytes)).unwrap(),
            support_assets: vec![],
        }],
    }
}

fn publication_staging_files(root: &TempDir) -> Vec<std::path::PathBuf> {
    fs::read_dir(root.path().join("blobs/publication-staging"))
        .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
        .unwrap_or_default()
}

#[test]
fn publication_preserves_content_order_timestamp_and_advances_activity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "publication", "Glim", "/tmp/glim")
        .unwrap();
    database(&root)
        .execute(
            "UPDATE sessions SET last_activity_at = 100 WHERE public_id = ?1",
            [&session.public_id],
        )
        .unwrap();

    let first = store
        .stage_publication_blob(Cursor::new(b"first file"))
        .unwrap();
    let support = store
        .stage_publication_blob(Cursor::new(b"support bytes"))
        .unwrap();
    let second = store
        .stage_publication_blob(Cursor::new(b"second file"))
        .unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "  Analysis title  ".to_owned(),
                commentary: "  **Result**\n".to_owned(),
                predecessor_post_id: None,
                files: vec![
                    PublicationFile {
                        filename: "index.md".to_owned(),
                        caption: Some("Entry document".to_owned()),
                        blob: first,
                        support_assets: vec![PublicationSupportAsset {
                            relative_path: "images/plot.png".to_owned(),
                            blob: support,
                        }],
                    },
                    PublicationFile {
                        filename: "raw.csv".to_owned(),
                        caption: None,
                        blob: second,
                        support_assets: vec![],
                    },
                ],
            },
            200,
        )
        .unwrap();

    assert_eq!(post.session_id, session.id);
    assert_eq!(post.session_public_id, session.public_id);
    assert_eq!(post.predecessor_post_id, None);
    assert_eq!(post.published_at, 200);

    let connection = database(&root);
    let stored_post = connection
        .query_row(
            "SELECT title, commentary, published_at FROM posts WHERE id = ?1",
            [post.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        stored_post,
        (
            "  Analysis title  ".to_owned(),
            "  **Result**\n".to_owned(),
            200
        )
    );

    let files = connection
        .prepare(
            "SELECT position, filename, caption FROM post_files
             WHERE post_id = ?1 ORDER BY position",
        )
        .unwrap()
        .query_map([post.id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        files,
        [
            (0, "index.md".to_owned(), Some("Entry document".to_owned())),
            (1, "raw.csv".to_owned(), None),
        ]
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT relative_path FROM support_assets WHERE post_id = ?1",
                [post.id],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "images/plot.png"
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT last_activity_at FROM sessions WHERE id = ?1",
                [session.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        200
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM blob_references WHERE post_id = ?1",
                [post.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        3
    );
}

#[test]
fn startup_removes_abandoned_prejournal_publication_staging() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let staging = root.path().join("blobs/publication-staging");
    std::fs::write(staging.join("abandoned.lock"), b"").unwrap();
    std::fs::write(staging.join("abandoned.part"), b"partial").unwrap();

    Store::open(root.path()).unwrap();

    assert_eq!(std::fs::read_dir(staging).unwrap().count(), 0);
}

#[test]
fn startup_removes_an_interrupted_publication_journal_transition() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let staging = root.path().join("blobs/publication-staging");
    fs::write(staging.join("interrupted.lock"), b"").unwrap();
    fs::write(staging.join("interrupted.part"), b"abc").unwrap();
    fs::write(staging.join("interrupted.next"), b"staged incomplete").unwrap();

    Store::open(root.path()).unwrap();

    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn duplicate_content_has_one_physical_blob_and_one_reference_per_occurrence() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "duplicates", "Glim", "/tmp/glim")
        .unwrap();
    let first = store.stage_publication_blob(Cursor::new(b"same")).unwrap();
    let support = store.stage_publication_blob(Cursor::new(b"same")).unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "First".to_owned(),
                commentary: "Result".to_owned(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "entry.html".to_owned(),
                    caption: None,
                    blob: first,
                    support_assets: vec![PublicationSupportAsset {
                        relative_path: "same.bin".to_owned(),
                        blob: support,
                    }],
                }],
            },
            10,
        )
        .unwrap();
    store
        .publish_at(
            one_file_request(&store, &session.public_id, "Second", "Again", b"same"),
            11,
        )
        .unwrap();

    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT COUNT(*) FROM blob_references WHERE post_id = ?1",
                [post.id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        2
    );
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        4
    );
}

#[test]
fn publication_validation_returns_stable_errors_without_visible_metadata() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "validation", "Glim", "/tmp/glim")
        .unwrap();

    let blank_title = store.publish_at(
        one_file_request(&store, &session.public_id, " \n", "comment", b"a"),
        1,
    );
    assert!(matches!(
        blank_title,
        Err(StoreError::BlankPublicationTitle)
    ));
    let blank_commentary = store.publish_at(
        one_file_request(&store, &session.public_id, "title", "\t", b"b"),
        1,
    );
    assert!(matches!(
        blank_commentary,
        Err(StoreError::BlankPublicationCommentary)
    ));
    let no_files = store.publish_at(
        PublicationRequest {
            session_public_id: session.public_id.clone(),
            title: "title".to_owned(),
            commentary: "comment".to_owned(),
            predecessor_post_id: None,
            files: vec![],
        },
        1,
    );
    assert!(matches!(no_files, Err(StoreError::PublicationRequiresFile)));

    let entry = store.stage_publication_blob(Cursor::new(b"entry")).unwrap();
    let first = store.stage_publication_blob(Cursor::new(b"one")).unwrap();
    let second = store.stage_publication_blob(Cursor::new(b"two")).unwrap();
    let duplicate_path = store.publish_at(
        PublicationRequest {
            session_public_id: session.public_id.clone(),
            title: "title".to_owned(),
            commentary: "comment".to_owned(),
            predecessor_post_id: None,
            files: vec![PublicationFile {
                filename: "entry.md".to_owned(),
                caption: None,
                blob: entry,
                support_assets: vec![
                    PublicationSupportAsset {
                        relative_path: "asset.png".to_owned(),
                        blob: first,
                    },
                    PublicationSupportAsset {
                        relative_path: "asset.png".to_owned(),
                        blob: second,
                    },
                ],
            }],
        },
        1,
    );
    assert!(matches!(
        duplicate_path,
        Err(StoreError::DuplicateSupportPath { ref relative_path }) if relative_path == "asset.png"
    ));
    let missing = store.publish_at(
        one_file_request(&store, "missing", "title", "comment", b"c"),
        1,
    );
    assert!(matches!(
        missing,
        Err(StoreError::SessionNotFound { ref public_id }) if public_id == "missing"
    ));

    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn publishing_store_revalidates_staged_size_against_its_upload_limit() {
    const FOUR_HASH: &str = "04efaf080f5a3e74e1c29d1ca6a48569382cbbcd324e8d59d2b83ef21c039f00";
    let root = TempDir::new().unwrap();
    let mut unlimited = Store::open(root.path()).unwrap();
    let session = unlimited
        .resolve_session("pi", "publisher-limit", "Glim", "/tmp/glim")
        .unwrap();
    let staged = unlimited
        .stage_publication_blob(Cursor::new(b"four"))
        .unwrap();
    drop(unlimited);
    let mut limited = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 3,
            max_finalized_blob_bytes: u64::MAX,
        },
    )
    .unwrap();

    let error = limited
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Too large".to_owned(),
                commentary: "Reject the consumed stage".to_owned(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "four.bin".to_owned(),
                    caption: None,
                    blob: staged,
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        StoreError::UploadLimitExceeded {
            limit: 3,
            attempted: 4
        }
    ));
    let connection = database(&root);
    for table in ["posts", "post_files", "blob_references", "blobs"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap(),
            0,
            "unexpected row in {table}"
        );
    }
    assert!(!root.path().join("blobs/04/ef").join(FOUR_HASH).exists());
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn publishing_store_revalidates_support_asset_size_against_its_upload_limit() {
    let root = TempDir::new().unwrap();
    let mut unlimited = Store::open(root.path()).unwrap();
    let session = unlimited
        .resolve_session("pi", "support-limit", "Glim", "/tmp/glim")
        .unwrap();
    let visible = unlimited
        .stage_publication_blob(Cursor::new(b"ok"))
        .unwrap();
    let support = unlimited
        .stage_publication_blob(Cursor::new(b"four"))
        .unwrap();
    drop(unlimited);
    let mut limited = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 3,
            max_finalized_blob_bytes: u64::MAX,
        },
    )
    .unwrap();

    let error = limited
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Support too large".to_owned(),
                commentary: "Reject all consumed stages".to_owned(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "entry.md".to_owned(),
                    caption: None,
                    blob: visible,
                    support_assets: vec![PublicationSupportAsset {
                        relative_path: "asset.bin".to_owned(),
                        blob: support,
                    }],
                }],
            },
            10,
        )
        .unwrap_err();

    assert!(matches!(
        error,
        StoreError::UploadLimitExceeded {
            limit: 3,
            attempted: 4
        }
    ));
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn publication_rejects_and_cleans_a_stage_from_another_store_root() {
    let source_root = TempDir::new().unwrap();
    let target_root = TempDir::new().unwrap();
    let source = Store::open(source_root.path()).unwrap();
    let staged = source
        .stage_publication_blob(Cursor::new(b"foreign"))
        .unwrap();
    let mut target = Store::open(target_root.path()).unwrap();
    let session = target
        .resolve_session("pi", "target", "Glim", "/tmp/glim")
        .unwrap();

    let error = target
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Wrong root".to_owned(),
                commentary: "Reject the foreign stage".to_owned(),
                predecessor_post_id: None,
                files: vec![PublicationFile {
                    filename: "foreign.bin".to_owned(),
                    caption: None,
                    blob: staged,
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap_err();

    assert!(matches!(error, StoreError::PublicationStagingStoreMismatch));
    assert!(publication_staging_files(&source_root).is_empty());
    assert!(publication_staging_files(&target_root).is_empty());
    for root in [&source_root, &target_root] {
        let connection = database(root);
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn revisions_link_new_posts_and_reject_missing_or_cross_session_predecessors() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first_session = store
        .resolve_session("pi", "first", "Glim", "/tmp/glim")
        .unwrap();
    let second_session = store
        .resolve_session("pi", "second", "Glim", "/tmp/glim")
        .unwrap();
    let predecessor = store
        .publish_at(
            one_file_request(&store, &first_session.public_id, "Original", "One", b"one"),
            10,
        )
        .unwrap();

    let mut revision =
        one_file_request(&store, &first_session.public_id, "Revision", "Two", b"two");
    revision.predecessor_post_id = Some(predecessor.id);
    let revised = store.publish_at(revision, 20).unwrap();
    assert_eq!(revised.predecessor_post_id, Some(predecessor.id));

    let mut missing = one_file_request(&store, &first_session.public_id, "Missing", "No", b"x");
    missing.predecessor_post_id = Some(9999);
    assert!(matches!(
        store.publish_at(missing, 30),
        Err(StoreError::PredecessorNotFound { post_id: 9999 })
    ));

    let mut cross = one_file_request(&store, &second_session.public_id, "Cross", "No", b"y");
    cross.predecessor_post_id = Some(predecessor.id);
    assert!(matches!(
        store.publish_at(cross, 30),
        Err(StoreError::CrossSessionPredecessor { post_id }) if post_id == predecessor.id
    ));

    let connection = database(&root);
    assert_eq!(
        connection
            .query_row(
                "SELECT title, predecessor_post_id FROM posts WHERE id = ?1",
                [predecessor.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)),
            )
            .unwrap(),
        ("Original".to_owned(), None)
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn each_staged_source_enforces_upload_limit_and_drop_cleans_other_parts() {
    struct FailingReader(bool);
    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::Error::other("reader failed"));
            }
            self.0 = true;
            buffer[..2].copy_from_slice(b"ab");
            Ok(2)
        }
    }

    let root = TempDir::new().unwrap();
    let store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 2,
            max_finalized_blob_bytes: u64::MAX,
        },
    )
    .unwrap();
    let first = store.stage_publication_blob(Cursor::new(b"ok")).unwrap();
    assert!(matches!(
        store.stage_publication_blob(Cursor::new(b"too")),
        Err(StoreError::UploadLimitExceeded {
            limit: 2,
            attempted: 3
        })
    ));
    assert!(matches!(
        store.stage_publication_blob(FailingReader(false)),
        Err(StoreError::Io(_))
    ));
    drop(first);

    assert!(publication_staging_files(&root).is_empty());
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn aggregate_quota_counts_distinct_new_hashes_once_at_the_exact_boundary() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 3,
            max_finalized_blob_bytes: 6,
        },
    )
    .unwrap();
    let session = store
        .resolve_session("pi", "quota", "Glim", "/tmp/glim")
        .unwrap();
    let first = store.stage_publication_blob(Cursor::new(b"abc")).unwrap();
    let repeated = store.stage_publication_blob(Cursor::new(b"abc")).unwrap();
    let second = store.stage_publication_blob(Cursor::new(b"def")).unwrap();
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Boundary".to_owned(),
                commentary: "Six unique bytes".to_owned(),
                predecessor_post_id: None,
                files: vec![
                    PublicationFile {
                        filename: "a".to_owned(),
                        caption: None,
                        blob: first,
                        support_assets: vec![PublicationSupportAsset {
                            relative_path: "copy".to_owned(),
                            blob: repeated,
                        }],
                    },
                    PublicationFile {
                        filename: "b".to_owned(),
                        caption: None,
                        blob: second,
                        support_assets: vec![],
                    },
                ],
            },
            10,
        )
        .unwrap();

    let error = store
        .publish_at(
            one_file_request(&store, &session.public_id, "Over", "One more", b"x"),
            11,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        StoreError::GlobalBlobBudgetExceeded {
            limit: 6,
            current: 6,
            additional: 1
        }
    ));
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        6
    );
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn sqlite_failure_after_final_creation_rolls_back_and_removes_new_files() {
    const NEW_HASH: &str = "11507a0e2f5e69d5dfa40a62a1bd7b6ee57e6bcd85c67c9b8431b36fff21c437";
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "rollback", "Glim", "/tmp/glim")
        .unwrap();
    let existing = store.store_blob(Cursor::new(b"existing")).unwrap();
    database(&root)
        .execute_batch(
            "CREATE TRIGGER reject_publication BEFORE INSERT ON posts
             BEGIN SELECT RAISE(ABORT, 'forced publication failure'); END;",
        )
        .unwrap();
    let existing_stage = store
        .stage_publication_blob(Cursor::new(b"existing"))
        .unwrap();
    let new_stage = store.stage_publication_blob(Cursor::new(b"new")).unwrap();

    let result = store.publish_at(
        PublicationRequest {
            session_public_id: session.public_id.clone(),
            title: "Fail".to_owned(),
            commentary: "Rollback".to_owned(),
            predecessor_post_id: None,
            files: vec![
                PublicationFile {
                    filename: "existing".to_owned(),
                    caption: None,
                    blob: existing_stage,
                    support_assets: vec![],
                },
                PublicationFile {
                    filename: "new".to_owned(),
                    caption: None,
                    blob: new_stage,
                    support_assets: vec![],
                },
            ],
        },
        10,
    );
    assert!(matches!(result, Err(StoreError::Sqlite(_))));

    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blob_references", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(store.open_blob(&existing).is_ok());
    assert!(!root.path().join("blobs/11/50").join(NEW_HASH).exists());
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn failed_final_cleanup_retains_journal_for_startup_recovery() {
    const CLEANUP_HASH: &str = "611496f412cac947be720d17a0ee6d7463221d14731fbc18244756271e8f5189";
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "cleanup-failure", "Glim", "/tmp/glim")
        .unwrap();
    database(&root)
        .execute_batch(
            "CREATE TRIGGER reject_after_delay BEFORE INSERT ON posts
             BEGIN
                 SELECT sum(value) FROM (
                     WITH RECURSIVE counter(value) AS (
                         VALUES(1)
                         UNION ALL
                         SELECT value + 1 FROM counter WHERE value < 10000000
                     )
                     SELECT value FROM counter
                 );
                 SELECT RAISE(ABORT, 'forced publication failure');
             END;",
        )
        .unwrap();
    let request = one_file_request(
        &store,
        &session.public_id,
        "Cleanup failure",
        "Retain recovery evidence",
        b"cleanup",
    );
    let final_path = root.path().join("blobs/61/14").join(CLEANUP_HASH);
    let final_parent = final_path.parent().unwrap().to_owned();
    let watched_path = final_path.clone();
    let watcher = thread::spawn(move || {
        for _ in 0..10_000 {
            if watched_path.exists() {
                fs::set_permissions(
                    watched_path.parent().unwrap(),
                    fs::Permissions::from_mode(0o500),
                )
                .unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("publication final link was never observed");
    });

    let error = store.publish_at(request, 10).unwrap_err();
    watcher.join().unwrap();
    fs::set_permissions(&final_parent, fs::Permissions::from_mode(0o700)).unwrap();

    assert!(matches!(error, StoreError::Io(_)));
    assert!(final_path.exists());
    let retained = publication_staging_files(&root);
    assert!(
        retained
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "journal"))
    );
    assert!(
        retained
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "part"))
    );
    assert!(
        retained
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "lock"))
    );
    drop(store);

    Store::open(root.path()).unwrap();
    assert!(!final_path.exists());
    assert!(publication_staging_files(&root).is_empty());
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn failed_publication_racing_same_blob_ingestion_never_deletes_the_accepted_final() {
    for round in 0..24 {
        let root = TempDir::new().unwrap();
        let mut setup = Store::open(root.path()).unwrap();
        let session = setup
            .resolve_session("pi", "race", "Glim", "/tmp/glim")
            .unwrap();
        database(&root)
            .execute_batch(
                "CREATE TRIGGER reject_racing_publication BEFORE INSERT ON posts
                 BEGIN SELECT RAISE(ABORT, 'forced publication failure'); END;",
            )
            .unwrap();
        let request = one_file_request(
            &setup,
            &session.public_id,
            "Rejected",
            "Race cleanup against accepted ingestion",
            b"shared-race",
        );
        drop(setup);
        let barrier = Arc::new(Barrier::new(2));
        let publication_barrier = barrier.clone();
        let publication_root = root.path().to_owned();
        let publisher = thread::spawn(move || {
            let mut store = Store::open(publication_root).unwrap();
            publication_barrier.wait();
            store.publish_at(request, i64::from(round))
        });
        let ingestion_root = root.path().to_owned();
        let ingester = thread::spawn(move || {
            let mut store = Store::open(ingestion_root).unwrap();
            barrier.wait();
            store.store_blob(Cursor::new(b"shared-race"))
        });

        assert!(matches!(
            publisher.join().unwrap(),
            Err(StoreError::Sqlite(_))
        ));
        let accepted = ingester.join().unwrap().unwrap();
        let reopened = Store::open(root.path()).unwrap();
        assert!(
            reopened.open_blob(&accepted).is_ok(),
            "accepted final missing in race round {round}"
        );
        assert_eq!(
            database(&root)
                .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn startup_recovers_publication_crash_states_without_scanning_finalized_tree() {
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let staging = root.path().join("blobs/publication-staging");
    fs::write(staging.join("before.lock"), b"").unwrap();
    fs::write(staging.join("before.part"), b"abc").unwrap();
    fs::write(
        staging.join("before.journal"),
        format!("finalizing {ABC} 3\n"),
    )
    .unwrap();
    let final_path = root.path().join("blobs/ba/78").join(ABC);
    fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    fs::write(&final_path, b"abc").unwrap();
    let unrelated = root.path().join("blobs/ff/ff/unrelated");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    fs::write(&unrelated, b"untouched").unwrap();

    Store::open(root.path()).unwrap();
    assert!(!final_path.exists());
    assert_eq!(fs::read(&unrelated).unwrap(), b"untouched");
    assert!(publication_staging_files(&root).is_empty());

    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "committed", "Glim", "/tmp/glim")
        .unwrap();
    store
        .publish_at(
            one_file_request(&store, &session.public_id, "Committed", "Keep", b"abc"),
            10,
        )
        .unwrap();
    drop(store);
    fs::write(staging.join("after.lock"), b"").unwrap();
    fs::write(staging.join("after.part"), b"abc").unwrap();
    fs::write(
        staging.join("after.journal"),
        format!("finalizing {ABC} 3\n"),
    )
    .unwrap();

    Store::open(root.path()).unwrap();
    assert!(final_path.exists());
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn another_store_open_does_not_clean_an_active_publication_stage() {
    struct PausingReader {
        sent: bool,
        started: mpsc::Sender<()>,
        resume: mpsc::Receiver<()>,
    }
    impl Read for PausingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.sent {
                return Ok(0);
            }
            self.sent = true;
            buffer[..3].copy_from_slice(b"abc");
            self.started.send(()).unwrap();
            self.resume.recv().unwrap();
            Ok(3)
        }
    }

    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let path = root.path().to_owned();
    let writer = thread::spawn(move || {
        Store::open(path)
            .unwrap()
            .stage_publication_blob(PausingReader {
                sent: false,
                started: started_tx,
                resume: resume_rx,
            })
    });
    started_rx.recv().unwrap();
    assert!(!publication_staging_files(&root).is_empty());
    drop(Store::open(root.path()).unwrap());
    assert!(!publication_staging_files(&root).is_empty());
    resume_tx.send(()).unwrap();
    drop(writer.join().unwrap().unwrap());
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn concurrent_publications_cannot_overcommit_quota_or_expose_partial_posts() {
    let root = TempDir::new().unwrap();
    let mut setup = Store::open(root.path()).unwrap();
    let sessions = ["one", "two"].map(|key| {
        setup
            .resolve_session("pi", key, "Glim", "/tmp/glim")
            .unwrap()
            .public_id
    });
    drop(setup);
    let limits = StoreLimits {
        max_upload_bytes: 3,
        max_finalized_blob_bytes: 3,
    };
    let barrier = Arc::new(Barrier::new(2));
    let handles = sessions
        .into_iter()
        .zip([b"abc", b"def"])
        .map(|(session, bytes)| {
            let path = root.path().to_owned();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut store = Store::open_with_limits(path, limits).unwrap();
                let request = one_file_request(&store, &session, "Concurrent", "Atomic", bytes);
                barrier.wait();
                store.publish_at(request, 10)
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::GlobalBlobBudgetExceeded { .. })))
            .count(),
        1
    );
    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM post_files", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blob_references", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(publication_staging_files(&root).is_empty());
}

fn atomic_request(store: &Store, title: &str, bytes: &[u8]) -> PublicationRequest {
    one_file_request(store, "", title, "atomic commentary", bytes)
}

fn identity(key: &str, label: &str, directory: &str) -> PublicationIdentity {
    PublicationIdentity {
        integration_namespace: "pi".into(),
        external_key: key.into(),
        project_label: label.into(),
        working_directory: directory.into(),
    }
}

#[test]
fn stateless_publication_resolves_reuses_and_updates_identity_atomically() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first = store
        .publish_resolving_at(
            identity("same", "Old", "/project/a"),
            atomic_request(&store, "One", b"one"),
            10,
        )
        .unwrap();
    let reused = store
        .publish_resolving_at(
            identity("same", "New", "/project/a"),
            atomic_request(&store, "Two", b"two"),
            20,
        )
        .unwrap();
    let isolated = store
        .publish_resolving_at(
            identity("same", "Other", "/project/b"),
            atomic_request(&store, "Three", b"three"),
            30,
        )
        .unwrap();

    assert_eq!(first.session.id, reused.session.id);
    assert_eq!(reused.session.project.label, "New");
    assert_ne!(first.session.id, isolated.session.id);
    assert_eq!(reused.post.session_id, reused.session.id);
    assert_eq!(reused.post.title, "Two");
}

#[test]
fn stateless_publication_rolls_back_new_identity_and_label_on_quota_or_predecessor_failure() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 16,
            max_finalized_blob_bytes: 3,
        },
    )
    .unwrap();
    let accepted = store
        .publish_resolving_at(
            identity("existing", "Original", "/existing"),
            atomic_request(&store, "Accepted", b"abc"),
            1,
        )
        .unwrap();

    let quota = store.publish_resolving_at(
        identity("new", "New", "/new"),
        atomic_request(&store, "Rejected", b"def"),
        2,
    );
    assert!(matches!(
        quota,
        Err(StoreError::GlobalBlobBudgetExceeded { .. })
    ));
    let mut cross = atomic_request(&store, "Cross", b"abc");
    cross.predecessor_post_id = Some(accepted.post.id);
    let conflict = store.publish_resolving_at(identity("other", "Changed", "/existing"), cross, 3);
    assert!(matches!(
        conflict,
        Err(StoreError::CrossSessionPredecessor { .. })
    ));

    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM projects", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT label FROM projects WHERE working_directory='/existing'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
        "Original"
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(publication_staging_files(&root).is_empty());
}

#[test]
fn concurrent_stateless_identity_publications_converge_on_one_session() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = [b"same".as_slice(), b"same".as_slice()]
        .into_iter()
        .map(|bytes| {
            let path = root.path().to_owned();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                let request = atomic_request(&store, "Concurrent", bytes);
                barrier.wait();
                store
                    .publish_resolving_at(identity("shared", "Glim", "/same"), request, 10)
                    .unwrap()
                    .session
                    .id
            })
        })
        .collect::<Vec<_>>();
    let ids = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(ids[0], ids[1]);
    let connection = database(&root);
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM posts", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn lifecycle_retains_shared_publication_blob_then_releases_final_quota() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first = store
        .resolve_session("pi", "life-one", "Glim", "/tmp/glim")
        .unwrap();
    let second = store
        .resolve_session("pi", "life-two", "Glim", "/tmp/glim")
        .unwrap();
    store
        .publish_at(
            one_file_request(&store, &first.public_id, "First", "Shared", b"shared"),
            10,
        )
        .unwrap();
    store
        .publish_at(
            one_file_request(&store, &second.public_id, "Second", "Shared", b"shared"),
            10,
        )
        .unwrap();

    let first_report = store.close_session(&first.public_id).unwrap();
    assert_eq!(first_report.blobs_deleted, 0);
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        6
    );
    let final_report = store.close_session(&second.public_id).unwrap();
    assert_eq!(final_report.blobs_deleted, 1);
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        0
    );
}
