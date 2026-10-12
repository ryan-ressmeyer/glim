use glim::storage::{
    PageRequest, PublicationFile, PublicationIdentity, PublicationRequest, PublicationSupportAsset,
    Store, StoreError,
};
use std::io::Cursor;
use tempfile::TempDir;

fn publish(
    store: &mut Store,
    session: &str,
    predecessor: Option<i64>,
    bytes: &'static [u8],
) -> i64 {
    let blob = store.stage_publication_blob(Cursor::new(bytes)).unwrap();
    store
        .publish_at(
            PublicationRequest {
                session_public_id: session.into(),
                title: "Plot".into(),
                commentary: "Result".into(),
                predecessor_post_id: predecessor,
                git: None,
                files: vec![PublicationFile {
                    filename: "plot.txt".into(),
                    caption: None,
                    blob,
                    support_assets: vec![],
                }],
            },
            100,
        )
        .unwrap()
        .id
}

#[test]
fn saved_revision_survives_close_and_retention_without_its_predecessor() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let first = publish(&mut store, &session.public_id, None, b"shared");
    let revision = publish(&mut store, &session.public_id, Some(first), b"shared");
    store.set_saved(revision, true).unwrap();
    let report = store.close_session(&session.public_id).unwrap();
    assert_eq!(report.posts_deleted, 1);
    assert!(matches!(
        store.post(first),
        Err(StoreError::PostNotFound { .. })
    ));
    let saved = store.post(revision).unwrap();
    assert!(saved.saved);
    assert_eq!(saved.predecessor_post_id, Some(first));
    assert_eq!(saved.files[0].blob.byte_size, 6);
    assert!(store.session(&session.public_id).unwrap().closed);
    assert_eq!(
        store.session(&session.public_id).unwrap().external_key,
        "key"
    );
    let status = store.status_snapshot(i64::MAX).unwrap();
    assert_eq!(status.saved_posts, 1);
    assert_eq!(status.total_posts, 1);
    assert_eq!(status.sessions_due_for_purge, 0);
    assert_eq!(
        store
            .purge_inactive_sessions(i64::MAX)
            .unwrap()
            .posts_deleted,
        0
    );
    let fresh = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    assert_ne!(fresh.public_id, session.public_id);
    assert!(!store.session(&fresh.public_id).unwrap().closed);
    drop(store);
    let store = Store::open(root.path()).unwrap();
    assert!(store.post(revision).unwrap().saved);
    assert_eq!(
        store.saved_posts(PageRequest::default()).unwrap().posts[0].id,
        revision
    );
}

#[test]
fn deleted_post_ids_are_never_reused_even_after_emptying_and_reopening_the_store() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let old = publish(&mut store, &session.public_id, None, b"old");
    store.set_saved(old, true).unwrap();
    store.delete_post(old, true).unwrap();
    store.close_session(&session.public_id).unwrap();
    drop(store);

    let mut store = Store::open(root.path()).unwrap();
    let replacement = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let new = publish(&mut store, &replacement.public_id, None, b"new");
    assert!(new > old);
    store.set_saved(new, true).unwrap();
    assert!(matches!(
        store.delete_post(old, true),
        Err(StoreError::PostNotFound { .. })
    ));
    assert!(matches!(
        store.post(old),
        Err(StoreError::PostNotFound { .. })
    ));
    assert_eq!(
        store.post(new).unwrap().session_public_id,
        replacement.public_id
    );
    assert!(store.post(new).unwrap().saved);
}

#[test]
fn saved_delete_requires_explicit_confirmation_and_unsave_never_deletes() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let post = publish(&mut store, &session.public_id, None, b"bytes");
    store.set_saved(post, true).unwrap();
    assert!(matches!(
        store.delete_post(post, false),
        Err(StoreError::SavedPostRequiresConfirmation { .. })
    ));
    assert!(store.post(post).unwrap().saved);
    store.set_saved(post, false).unwrap();
    assert!(!store.post(post).unwrap().saved);
    store.set_saved(post, true).unwrap();
    assert_eq!(store.delete_post(post, true).unwrap().posts_deleted, 1);
    assert!(matches!(
        store.post(post),
        Err(StoreError::PostNotFound { .. })
    ));
}

#[test]
fn deleting_last_saved_post_keeps_unsaved_visible_sibling_and_directory_context() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let a = publish(&mut store, &session.public_id, None, b"a");
    let b = publish(&mut store, &session.public_id, None, b"b");
    store.set_saved(a, true).unwrap();
    store.set_saved(b, true).unwrap();
    assert_eq!(
        store
            .close_session(&session.public_id)
            .unwrap()
            .sessions_retained,
        1
    );
    store.set_saved(a, false).unwrap();
    let report = store.delete_post(b, true).unwrap();
    assert_eq!(report.posts_deleted, 1);
    assert_eq!(report.sessions_deleted, 0);
    assert!(!store.post(a).unwrap().saved);
    assert_eq!(
        store
            .session_posts(&session.public_id, PageRequest::default())
            .unwrap()
            .posts[0]
            .id,
        a
    );
    let listing = store.sessions(PageRequest::default()).unwrap();
    assert_eq!(listing.sessions[0].public_id, session.public_id);
    assert_eq!(listing.sessions[0].post_count, 1);
    assert_eq!(listing.sessions[0].saved_count, 0);
}

#[test]
fn retained_support_asset_and_shared_blob_survive_lifecycle_cleanup() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let first = publish(&mut store, &session.public_id, None, b"shared");
    let support = store
        .stage_publication_blob(Cursor::new(b"shared"))
        .unwrap();
    let visible = store
        .stage_publication_blob(Cursor::new(b"visible"))
        .unwrap();
    let saved = store
        .publish_at(
            PublicationRequest {
                session_public_id: session.public_id.clone(),
                title: "Saved".into(),
                commentary: "Result".into(),
                predecessor_post_id: Some(first),
                git: None,
                files: vec![PublicationFile {
                    filename: "page.md".into(),
                    caption: None,
                    blob: visible,
                    support_assets: vec![PublicationSupportAsset {
                        relative_path: "shared.txt".into(),
                        blob: support,
                    }],
                }],
            },
            101,
        )
        .unwrap()
        .id;
    store.set_saved(saved, true).unwrap();
    store.purge_inactive_sessions(i64::MAX).unwrap();
    assert_eq!(
        store.post(saved).unwrap().files[0].support_assets[0].relative_path,
        "shared.txt"
    );
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        13
    );
    assert_eq!(store.reclaim_unused_data().unwrap(), 0);
}

#[test]
fn retained_identity_never_collides_with_a_user_supplied_external_key() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let other = store
        .resolve_session("pi", "__glim_retained_1_0", "Project", "/project")
        .unwrap();
    let post = publish(&mut store, &session.public_id, None, b"saved");
    store.set_saved(post, true).unwrap();
    store.close_session(&session.public_id).unwrap();
    assert_eq!(
        store.session(&session.public_id).unwrap().external_key,
        "key"
    );
    assert_eq!(
        store.session(&other.public_id).unwrap().external_key,
        "__glim_retained_1_0"
    );
    assert_ne!(
        store
            .resolve_session("pi", "key", "Project", "/project")
            .unwrap()
            .public_id,
        session.public_id
    );
}

#[test]
fn closed_synthetic_looking_external_key_can_be_reused_by_resolution_and_publication() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first = store
        .resolve_session("pi", "__glim_retained_2_0", "Project", "/project")
        .unwrap();
    let first_post = publish(&mut store, &first.public_id, None, b"first");
    store.set_saved(first_post, true).unwrap();
    store.close_session(&first.public_id).unwrap();

    let second = store
        .resolve_session("pi", "second", "Project", "/project")
        .unwrap();
    let second_post = publish(&mut store, &second.public_id, None, b"second");
    store.set_saved(second_post, true).unwrap();
    store.close_session(&second.public_id).unwrap();
    assert_eq!(
        store.session(&first.public_id).unwrap().external_key,
        "__glim_retained_2_0"
    );
    let fresh = store
        .resolve_session("pi", "__glim_retained_2_0", "Project", "/project")
        .unwrap();
    assert_ne!(fresh.public_id, first.public_id);
    assert_ne!(fresh.public_id, second.public_id);
    store.close_session(&fresh.public_id).unwrap();

    let published = store
        .publish_resolving_at(
            PublicationIdentity {
                integration_namespace: "pi".into(),
                external_key: "__glim_retained_2_0".into(),
                project_label: "Project".into(),
                working_directory: "/project".into(),
            },
            PublicationRequest {
                session_public_id: String::new(),
                title: "New".into(),
                commentary: "New".into(),
                predecessor_post_id: None,
                git: None,
                files: vec![PublicationFile {
                    filename: "new.txt".into(),
                    caption: None,
                    blob: store.stage_publication_blob(Cursor::new(b"new")).unwrap(),
                    support_assets: vec![],
                }],
            },
            102,
        )
        .unwrap();
    assert_ne!(published.session.public_id, first.public_id);
    assert_ne!(published.session.public_id, fresh.public_id);
    assert_eq!(published.session.external_key, "__glim_retained_2_0");
    assert!(store.post(first_post).unwrap().saved);
    assert!(store.post(second_post).unwrap().saved);
}

#[test]
fn unavailable_predecessor_cannot_be_used_for_new_revision() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let first = publish(&mut store, &session.public_id, None, b"first");
    publish(&mut store, &session.public_id, Some(first), b"second");
    store.delete_post(first, false).unwrap();
    let blob = store.stage_publication_blob(Cursor::new(b"third")).unwrap();
    let result = store.publish_at(
        PublicationRequest {
            session_public_id: session.public_id,
            title: "Third".into(),
            commentary: "Third".into(),
            predecessor_post_id: Some(first),
            git: None,
            files: vec![PublicationFile {
                filename: "third.txt".into(),
                caption: None,
                blob,
                support_assets: vec![],
            }],
        },
        101,
    );
    assert!(matches!(
        result,
        Err(StoreError::PredecessorNotFound { .. })
    ));
}

#[test]
fn retention_preserves_saved_then_reclaims_closed_context_after_unsave() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let post = publish(&mut store, &session.public_id, None, b"saved");
    store.set_saved(post, true).unwrap();
    let report = store.purge_inactive_sessions(i64::MAX).unwrap();
    assert_eq!(report.sessions_deleted, 0);
    assert_eq!(report.sessions_retained, 1);
    assert!(store.post(post).unwrap().saved);
    store.set_saved(post, false).unwrap();
    assert!(store.post(post).is_ok());
    assert_eq!(
        store
            .purge_inactive_sessions(i64::MAX)
            .unwrap()
            .sessions_deleted,
        1
    );
    assert!(matches!(
        store.post(post),
        Err(StoreError::PostNotFound { .. })
    ));
}

#[test]
fn retention_removes_unsaved_closed_siblings_without_removing_saved_posts() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let unsaved = publish(&mut store, &session.public_id, None, b"unsaved");
    let saved = publish(&mut store, &session.public_id, None, b"saved");
    store.set_saved(unsaved, true).unwrap();
    store.set_saved(saved, true).unwrap();
    store.close_session(&session.public_id).unwrap();
    store.set_saved(unsaved, false).unwrap();
    assert!(store.post(unsaved).is_ok());
    assert_eq!(
        store
            .status_snapshot(i64::MAX)
            .unwrap()
            .sessions_due_for_purge,
        1
    );
    let report = store.purge_inactive_sessions(i64::MAX).unwrap();
    assert_eq!(report.posts_deleted, 1);
    assert_eq!(report.sessions_deleted, 0);
    assert!(matches!(
        store.post(unsaved),
        Err(StoreError::PostNotFound { .. })
    ));
    assert!(store.post(saved).unwrap().saved);
    assert_eq!(
        store
            .status_snapshot(i64::MAX)
            .unwrap()
            .sessions_due_for_purge,
        0
    );
}

#[test]
fn reclaim_removes_only_unreferenced_finalized_blobs() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let orphan = store.store_blob(Cursor::new(b"orphan")).unwrap();
    let session = store
        .resolve_session("pi", "key", "Project", "/project")
        .unwrap();
    let post = publish(&mut store, &session.public_id, None, b"referenced");
    let staged = store
        .stage_publication_blob(Cursor::new(b"in flight"))
        .unwrap();
    assert_eq!(store.reclaim_unused_data().unwrap(), 1);
    assert!(store.blob_record(orphan.hash()).unwrap().is_none());
    assert_eq!(store.post(post).unwrap().files[0].blob.byte_size, 10);
    drop(staged);
}

#[test]
fn session_directory_is_bounded_and_does_not_touch_activity() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let a = store.resolve_session("pi", "a", "A", "/a").unwrap();
    let b = store.resolve_session("pi", "b", "B", "/b").unwrap();
    publish(&mut store, &a.public_id, None, b"plot");
    let before = store.session(&a.public_id).unwrap().last_activity_at;
    let page = store
        .sessions(PageRequest {
            limit: Some(1),
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.sessions.len(), 1);
    assert!(page.next_cursor.is_some());
    let next = store
        .sessions(PageRequest {
            limit: Some(1),
            cursor: page.next_cursor,
        })
        .unwrap();
    assert_eq!(next.sessions.len(), 1);
    assert_ne!(page.sessions[0].public_id, next.sessions[0].public_id);
    let entries = [page.sessions[0].clone(), next.sessions[0].clone()];
    let entry = entries.iter().find(|s| s.public_id == a.public_id).unwrap();
    assert_eq!(entry.post_count, 1);
    assert_eq!(entry.saved_count, 0);
    assert_eq!(
        store.session(&a.public_id).unwrap().last_activity_at,
        before
    );
    assert_eq!(store.session(&b.public_id).unwrap().external_key, "b");
    assert!(matches!(
        store.sessions(PageRequest {
            limit: Some(101),
            cursor: None
        }),
        Err(StoreError::InvalidPageLimit { .. })
    ));
}
