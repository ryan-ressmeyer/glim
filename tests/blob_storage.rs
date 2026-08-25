use std::{
    fs,
    io::{self, Cursor, Read},
    sync::{Arc, Barrier, mpsc},
    thread,
};

use glim::storage::{BlobHash, BlobIntegrityError, Store, StoreError, StoreLimits};
use rusqlite::Connection;
use tempfile::TempDir;

const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn database(root: &TempDir) -> Connection {
    Connection::open(root.path().join("metadata.sqlite3")).unwrap()
}

fn staging_files(root: &TempDir) -> Vec<std::path::PathBuf> {
    let staging = root.path().join("blobs").join("staging");
    fs::read_dir(staging)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

#[test]
fn per_file_limit_accepts_exact_boundary_and_rejects_first_byte_over() {
    let root = TempDir::new().unwrap();
    let limits = StoreLimits {
        max_upload_bytes: 3,
        max_finalized_blob_bytes: u64::MAX,
    };
    let mut store = Store::open_with_limits(root.path(), limits).unwrap();

    let accepted = store.store_blob(Cursor::new(b"abc")).unwrap();
    assert_eq!(accepted.byte_size(), 3);

    let error = store.store_blob(Cursor::new(b"abcd")).unwrap_err();
    assert!(matches!(
        error,
        StoreError::UploadLimitExceeded {
            limit: 3,
            attempted: 4
        }
    ));
    assert_eq!(
        error.to_string(),
        "upload byte limit exceeded: limit 3, attempted 4"
    );
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(staging_files(&root).is_empty());
}

#[test]
fn global_budget_accepts_exact_boundary_and_rejects_first_unique_byte_over() {
    let root = TempDir::new().unwrap();
    let limits = StoreLimits {
        max_upload_bytes: u64::MAX,
        max_finalized_blob_bytes: 3,
    };
    let mut store = Store::open_with_limits(root.path(), limits).unwrap();

    store.store_blob(Cursor::new(b"abc")).unwrap();
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        3
    );

    let error = store.store_blob(Cursor::new(b"d")).unwrap_err();
    assert!(matches!(
        error,
        StoreError::GlobalBlobBudgetExceeded {
            limit: 3,
            current: 3,
            additional: 1
        }
    ));
    assert_eq!(
        error.to_string(),
        "global finalized blob budget exceeded: limit 3, current usage 3, additional unique bytes 1"
    );
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        3
    );
    assert!(staging_files(&root).is_empty());
}

#[test]
fn duplicate_at_full_budget_succeeds_without_increasing_usage() {
    let root = TempDir::new().unwrap();
    let limits = StoreLimits {
        max_upload_bytes: 3,
        max_finalized_blob_bytes: 3,
    };
    let mut store = Store::open_with_limits(root.path(), limits).unwrap();
    let first = store.store_blob(Cursor::new(b"abc")).unwrap();

    let duplicate = store.store_blob(Cursor::new(b"abc")).unwrap();

    assert_eq!(duplicate, first);
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        3
    );
}

#[test]
fn upload_limit_applies_even_when_identical_content_is_already_finalized() {
    let root = TempDir::new().unwrap();
    Store::open(root.path())
        .unwrap()
        .store_blob(Cursor::new(b"abcd"))
        .unwrap();
    let mut limited = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 3,
            max_finalized_blob_bytes: u64::MAX,
        },
    )
    .unwrap();

    assert!(matches!(
        limited.store_blob(Cursor::new(b"abcd")),
        Err(StoreError::UploadLimitExceeded {
            limit: 3,
            attempted: 4
        })
    ));
    assert_eq!(
        limited
            .physical_usage()
            .unwrap()
            .finalized_unique_blob_bytes,
        4
    );
    assert!(staging_files(&root).is_empty());
}

#[test]
fn zero_byte_blob_is_accounted_once_at_zero_bytes() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: 0,
            max_finalized_blob_bytes: 0,
        },
    )
    .unwrap();

    let first = store.store_blob(Cursor::new([])).unwrap();
    let second = store.store_blob(Cursor::new([])).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.byte_size(), 0);
    assert_eq!(
        store.physical_usage().unwrap().finalized_unique_blob_bytes,
        0
    );
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn concurrent_unique_ingestions_cannot_collectively_exceed_global_budget() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let limits = StoreLimits {
        max_upload_bytes: 3,
        max_finalized_blob_bytes: 3,
    };
    let barrier = Arc::new(Barrier::new(2));
    let handles = [b"abc".as_slice(), b"def".as_slice()].map(|bytes| {
        let path = root.path().to_owned();
        let barrier = barrier.clone();
        thread::spawn(move || {
            let mut store = Store::open_with_limits(path, limits).unwrap();
            barrier.wait();
            store.store_blob(Cursor::new(bytes))
        })
    });

    let results = handles.map(|handle| handle.join().unwrap());
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::GlobalBlobBudgetExceeded { .. })))
            .count(),
        1
    );
    assert_eq!(
        Store::open_with_limits(root.path(), limits)
            .unwrap()
            .physical_usage()
            .unwrap()
            .finalized_unique_blob_bytes,
        3
    );
}

#[test]
fn lowered_budget_allows_read_and_dedup_but_rejects_new_unique_content() {
    let root = TempDir::new().unwrap();
    let original = Store::open(root.path())
        .unwrap()
        .store_blob(Cursor::new(b"above"))
        .unwrap();
    let mut lowered = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: u64::MAX,
            max_finalized_blob_bytes: 3,
        },
    )
    .unwrap();

    assert!(lowered.open_blob(&original).is_ok());
    assert_eq!(lowered.store_blob(Cursor::new(b"above")).unwrap(), original);
    assert!(matches!(
        lowered.store_blob(Cursor::new(b"x")),
        Err(StoreError::GlobalBlobBudgetExceeded {
            limit: 3,
            current: 5,
            additional: 1
        })
    ));
    assert_eq!(
        lowered
            .physical_usage()
            .unwrap()
            .finalized_unique_blob_bytes,
        5
    );
}

#[test]
fn new_zero_byte_blob_succeeds_above_lowered_budget_without_increasing_usage() {
    let root = TempDir::new().unwrap();
    Store::open(root.path())
        .unwrap()
        .store_blob(Cursor::new(b"above"))
        .unwrap();
    let mut lowered = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: u64::MAX,
            max_finalized_blob_bytes: 3,
        },
    )
    .unwrap();

    let empty = lowered.store_blob(Cursor::new([])).unwrap();

    assert_eq!(empty.hash().as_str(), EMPTY_SHA256);
    assert_eq!(empty.byte_size(), 0);
    assert_eq!(
        lowered
            .physical_usage()
            .unwrap()
            .finalized_unique_blob_bytes,
        5
    );
}

#[test]
fn recovery_adopts_zero_byte_final_blob_above_lowered_budget() {
    let root = TempDir::new().unwrap();
    Store::open(root.path())
        .unwrap()
        .store_blob(Cursor::new(b"above"))
        .unwrap();
    let staging = root.path().join("blobs/staging");
    fs::write(staging.join("interrupted-empty.lock"), b"").unwrap();
    fs::write(staging.join("interrupted-empty.part"), b"").unwrap();
    let final_path = root.path().join("blobs/e3/b0").join(EMPTY_SHA256);
    fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    fs::write(&final_path, b"").unwrap();

    let reopened = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: u64::MAX,
            max_finalized_blob_bytes: 3,
        },
    )
    .unwrap();

    let empty = reopened
        .blob_record(&BlobHash::parse(EMPTY_SHA256).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(empty.byte_size(), 0);
    assert!(reopened.open_blob(&empty).is_ok());
    assert_eq!(
        reopened
            .physical_usage()
            .unwrap()
            .finalized_unique_blob_bytes,
        5
    );
    assert!(final_path.exists());
    assert!(staging_files(&root).is_empty());
}

#[test]
fn recovery_removes_uncommitted_final_blob_that_would_exceed_budget() {
    let root = TempDir::new().unwrap();
    Store::open(root.path())
        .unwrap()
        .store_blob(Cursor::new(b"full"))
        .unwrap();
    let staging = root.path().join("blobs/staging");
    fs::write(staging.join("interrupted.lock"), b"").unwrap();
    fs::write(staging.join("interrupted.part"), b"abc").unwrap();
    let final_path = root.path().join("blobs/ba/78").join(ABC_SHA256);
    fs::create_dir_all(final_path.parent().unwrap()).unwrap();
    fs::write(&final_path, b"abc").unwrap();

    let reopened = Store::open_with_limits(
        root.path(),
        StoreLimits {
            max_upload_bytes: u64::MAX,
            max_finalized_blob_bytes: 4,
        },
    )
    .unwrap();

    assert_eq!(
        reopened
            .physical_usage()
            .unwrap()
            .finalized_unique_blob_bytes,
        4
    );
    assert!(
        reopened
            .blob_record(&BlobHash::parse(ABC_SHA256).unwrap())
            .unwrap()
            .is_none()
    );
    assert!(!final_path.exists());
    assert!(staging_files(&root).is_empty());
}

#[test]
fn streams_multiple_chunks_and_reopens_identical_bytes() {
    struct ChunkedReader {
        chunks: Vec<&'static [u8]>,
        next: usize,
    }
    impl Read for ChunkedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.next == self.chunks.len() {
                return Ok(0);
            }
            let chunk = self.chunks[self.next];
            self.next += 1;
            buffer[..chunk.len()].copy_from_slice(chunk);
            Ok(chunk.len())
        }
    }

    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let bytes = b"streamed in several chunks";
    let record = store
        .store_blob(ChunkedReader {
            chunks: vec![b"streamed ", b"in several ", b"chunks"],
            next: 0,
        })
        .unwrap();

    assert_eq!(record.byte_size(), bytes.len() as u64);
    assert_eq!(record.hash().as_str().len(), 64);
    let mut reopened = store.open_blob(&record).unwrap();
    let mut actual = Vec::new();
    reopened.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, bytes);
}

#[test]
fn identical_content_deduplicates_to_one_file_and_metadata_row() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();

    let first = store.store_blob(Cursor::new(b"abc")).unwrap();
    let second = store.store_blob(Cursor::new(b"abc")).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.hash().as_str(), ABC_SHA256);
    assert!(root.path().join("blobs/ba/78").join(ABC_SHA256).is_file());
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn independent_connections_concurrently_ingesting_identical_content_converge() {
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let barrier = Arc::new(Barrier::new(8));
    let handles = (0..8)
        .map(|_| {
            let path = root.path().to_owned();
            let barrier = barrier.clone();
            thread::spawn(move || {
                let mut store = Store::open(path).unwrap();
                barrier.wait();
                store.store_blob(Cursor::new(vec![42_u8; 128 * 1024]))
            })
        })
        .collect::<Vec<_>>();
    let records = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect::<Vec<_>>();

    assert!(records.iter().all(|record| record == &records[0]));
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(staging_files(&root).is_empty());
}

#[test]
fn distinct_content_produces_distinct_records_and_files() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let first = store.store_blob(Cursor::new(b"first")).unwrap();
    let second = store.store_blob(Cursor::new(b"second")).unwrap();

    assert_ne!(first, second);
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert!(store.open_blob(&first).is_ok());
    assert!(store.open_blob(&second).is_ok());
}

#[test]
fn reader_failure_leaves_no_blob_metadata_final_file_or_staging_data() {
    struct FailingReader(bool);
    impl Read for FailingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                return Err(io::Error::other("source failed"));
            }
            self.0 = true;
            buffer[..4].copy_from_slice(b"part");
            Ok(4)
        }
    }

    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    assert!(matches!(
        store.store_blob(FailingReader(false)),
        Err(StoreError::Io(_))
    ));
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(staging_files(&root).is_empty());
}

#[test]
fn startup_removes_unlocked_interrupted_staging_data() {
    let root = TempDir::new().unwrap();
    let staging = root.path().join("blobs/staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("abandoned.lock"), b"").unwrap();
    fs::write(staging.join("abandoned.part"), b"partial bytes").unwrap();

    let store = Store::open(root.path()).unwrap();
    drop(store);

    assert!(staging_files(&root).is_empty());
}

#[test]
fn recorded_blob_corruption_does_not_prevent_store_open() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let record = store.store_blob(Cursor::new(b"abc")).unwrap();
    drop(store);
    fs::write(
        root.path().join("blobs/ba/78").join(ABC_SHA256),
        b"changed bytes",
    )
    .unwrap();

    let reopened = Store::open(root.path()).unwrap();
    assert!(matches!(
        reopened.open_blob(&record),
        Err(StoreError::Integrity(
            BlobIntegrityError::SizeMismatch { .. }
        ))
    ));
}

#[test]
fn startup_recovers_only_finalized_blobs_with_unlocked_staging_journals() {
    const UNRELATED_HASH: &str = "c2703a7ddf6c74b39505339af20dd6dd4f0794720e038b78ba395600c72417d4";
    let root = TempDir::new().unwrap();
    Store::open(root.path()).unwrap();
    let staging = root.path().join("blobs/staging");
    fs::write(staging.join("interrupted.lock"), b"").unwrap();
    fs::write(staging.join("interrupted.part"), b"abc").unwrap();
    fs::create_dir_all(root.path().join("blobs/ba/78")).unwrap();
    fs::write(root.path().join("blobs/ba/78").join(ABC_SHA256), b"abc").unwrap();
    fs::create_dir_all(root.path().join("blobs/c2/70")).unwrap();
    fs::write(
        root.path().join("blobs/c2/70").join(UNRELATED_HASH),
        b"corrupt",
    )
    .unwrap();

    let store = Store::open(root.path()).unwrap();
    let hash = BlobHash::parse(ABC_SHA256).unwrap();
    let record = store.blob_record(&hash).unwrap().unwrap();

    assert_eq!(record.byte_size(), 3);
    assert!(store.open_blob(&record).is_ok());
    assert!(staging_files(&root).is_empty());
    drop(store);

    fs::write(staging.join("committed.lock"), b"").unwrap();
    fs::write(staging.join("committed.part"), b"abc").unwrap();
    let reopened = Store::open(root.path()).unwrap();
    drop(reopened);
    assert!(staging_files(&root).is_empty());
}

#[test]
fn opening_another_store_does_not_remove_an_active_writers_staging_file() {
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
        let mut store = Store::open(path).unwrap();
        store.store_blob(PausingReader {
            sent: false,
            started: started_tx,
            resume: resume_rx,
        })
    });
    started_rx.recv().unwrap();
    assert!(!staging_files(&root).is_empty());

    let other = Store::open(root.path()).unwrap();
    drop(other);
    assert!(!staging_files(&root).is_empty());
    resume_tx.send(()).unwrap();

    let record = writer.join().unwrap().unwrap();
    assert_eq!(record.hash().as_str(), ABC_SHA256);
    assert!(staging_files(&root).is_empty());
}

#[test]
fn source_panic_drops_and_removes_staging_data() {
    struct PanickingReader(bool);
    impl Read for PanickingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.0 {
                panic!("cancelled source");
            }
            self.0 = true;
            buffer[..4].copy_from_slice(b"part");
            Ok(4)
        }
    }

    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.store_blob(PanickingReader(false));
    }));

    assert!(unwind.is_err());
    assert!(staging_files(&root).is_empty());
    assert_eq!(
        database(&root)
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn database_insert_failure_leaves_no_final_metadata_or_staging() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let connection = database(&root);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_blob_insert BEFORE INSERT ON blobs
             BEGIN SELECT RAISE(ABORT, 'forced database failure'); END;",
        )
        .unwrap();

    assert!(matches!(
        store.store_blob(Cursor::new(b"abc")),
        Err(StoreError::Sqlite(_))
    ));
    assert!(staging_files(&root).is_empty());
    assert!(!root.path().join("blobs/ba/78").join(ABC_SHA256).exists());
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM blobs", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn existing_final_file_is_never_overwritten() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let path = root.path().join("blobs/ba/78").join(ABC_SHA256);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"xyz").unwrap();

    assert!(matches!(
        store.store_blob(Cursor::new(b"abc")),
        Err(StoreError::Integrity(
            BlobIntegrityError::HashMismatch { .. }
        ))
    ));
    assert_eq!(fs::read(path).unwrap(), b"xyz");
    assert!(staging_files(&root).is_empty());
}

#[test]
fn missing_and_size_mismatched_files_return_typed_integrity_errors() {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let missing = store.store_blob(Cursor::new(b"abc")).unwrap();
    let path = root.path().join("blobs/ba/78").join(ABC_SHA256);
    fs::remove_file(&path).unwrap();
    assert!(matches!(
        store.open_blob(&missing),
        Err(StoreError::Integrity(BlobIntegrityError::Missing { .. }))
    ));

    let mismatch = store.store_blob(Cursor::new(b"different")).unwrap();
    let mismatch_path = root
        .path()
        .join("blobs")
        .join(&mismatch.hash().as_str()[..2])
        .join(&mismatch.hash().as_str()[2..4])
        .join(mismatch.hash().as_str());
    fs::write(mismatch_path, b"wrong size").unwrap();
    assert!(matches!(
        store.open_blob(&mismatch),
        Err(StoreError::Integrity(
            BlobIntegrityError::SizeMismatch { .. }
        ))
    ));
}
