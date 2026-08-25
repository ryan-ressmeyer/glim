use std::io::Cursor;

use glim::storage::{GitProvenance, PublicationFile, PublicationRequest, Store, StoreError};
use rusqlite::Connection;
use tempfile::TempDir;

#[test]
fn publication_persists_immutable_optional_git_provenance_and_migrates_existing_posts() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store.resolve_session("pi", "git", "Glim", "/work").unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "Provenance".into(),
                commentary: "Bounded Git identity".into(),
                predecessor_post_id: None,
                git: Some(GitProvenance {
                    root: "/work".into(),
                    branch: Some("phase-2-cli".into()),
                    commit: Some("0123456789abcdef0123456789abcdef01234567".into()),
                }),
                files: vec![PublicationFile {
                    filename: "result.txt".into(),
                    caption: None,
                    blob: store
                        .stage_publication_blob(Cursor::new(b"result"))
                        .unwrap(),
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap();

    let read = store.post(post.id).unwrap();
    assert_eq!(read.git.as_ref().unwrap().root, "/work");
    assert_eq!(
        read.git.as_ref().unwrap().branch.as_deref(),
        Some("phase-2-cli")
    );
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    assert!(
        connection
            .execute(
                "UPDATE posts SET git_root = '/changed' WHERE id = ?1",
                [post.id]
            )
            .is_err()
    );
}

fn request_with_git(store: &Store, session: String, git: GitProvenance) -> PublicationRequest {
    PublicationRequest {
        session_public_id: session,
        title: "Git validation".into(),
        commentary: "Reject malformed inert metadata".into(),
        predecessor_post_id: None,
        git: Some(git),
        files: vec![PublicationFile {
            filename: "result.txt".into(),
            caption: None,
            blob: store
                .stage_publication_blob(Cursor::new(b"result"))
                .unwrap(),
            support_assets: vec![],
        }],
    }
}

#[test]
fn provenance_requires_absolute_control_free_root_branch_and_full_object_id() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "invalid", "Glim", "/tmp")
        .unwrap()
        .public_id;
    let invalid = [
        GitProvenance {
            root: "relative".into(),
            branch: Some("main".into()),
            commit: Some("a".repeat(40)),
        },
        GitProvenance {
            root: "/work\nleak".into(),
            branch: Some("main".into()),
            commit: Some("a".repeat(40)),
        },
        GitProvenance {
            root: "/work".into(),
            branch: Some("bad\nbranch".into()),
            commit: Some("a".repeat(40)),
        },
        GitProvenance {
            root: "/work".into(),
            branch: Some("main".into()),
            commit: Some("a".repeat(39)),
        },
        GitProvenance {
            root: "/work".into(),
            branch: Some("main".into()),
            commit: Some("g".repeat(40)),
        },
    ];
    for git in invalid {
        let error = store
            .publish_at(request_with_git(&store, session.clone(), git), 10)
            .unwrap_err();
        assert!(matches!(error, StoreError::InvalidGitProvenance));
    }
    let accepted = store.publish_at(
        request_with_git(
            &store,
            session,
            GitProvenance {
                root: "/work".into(),
                branch: None,
                commit: Some("a".repeat(64)),
            },
        ),
        10,
    );
    assert!(accepted.is_ok());
}

#[test]
fn corrupt_persisted_provenance_returns_typed_error_instead_of_dropping_fields() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "corrupt-git", "Glim", "/tmp")
        .unwrap();
    let post = store
        .publish_at(
            request_with_git(
                &store,
                session.public_id,
                GitProvenance {
                    root: "/work".into(),
                    branch: Some("main".into()),
                    commit: Some("a".repeat(40)),
                },
            ),
            10,
        )
        .unwrap();
    let connection = Connection::open(root.path().join("metadata.sqlite3")).unwrap();
    connection.execute_batch("DROP TRIGGER posts_are_immutable; UPDATE posts SET git_root=NULL, git_branch='main', git_commit='bad';").unwrap();

    assert!(matches!(
        store.post(post.id),
        Err(StoreError::InvalidPostMetadata)
    ));
}

#[test]
fn publication_without_git_provenance_reads_cleanly_as_absent() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store.resolve_session("pi", "none", "Glim", "/tmp").unwrap();
    let post = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id,
                title: "No repository".into(),
                commentary: "Git is optional".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "result.txt".into(),
                    caption: None,
                    blob: store
                        .stage_publication_blob(Cursor::new(b"result"))
                        .unwrap(),
                    support_assets: vec![],
                }],
            },
            10,
        )
        .unwrap();
    assert_eq!(store.post(post.id).unwrap().git, None);
}
