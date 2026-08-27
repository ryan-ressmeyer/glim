use std::{hint::black_box, io::Cursor, sync::Arc, thread};

use axum::{
    body::Body,
    http::{Request, header},
};
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use http_body_util::BodyExt;
use tempfile::TempDir;
use tower::ServiceExt;

use glim::storage::{PageRequest, PublicationFile, PublicationIdentity, PublicationRequest, Store};

fn request(
    blob: glim::storage::StagedPublicationBlob,
    session_public_id: String,
    index: usize,
) -> PublicationRequest {
    PublicationRequest {
        session_public_id,
        title: format!("Benchmark {index}"),
        commentary: "Bounded benchmark fixture".into(),
        predecessor_post_id: None,
        git: None,
        files: vec![PublicationFile {
            filename: format!("artifact-{index}.bin"),
            caption: None,
            blob,
            support_assets: vec![],
        }],
    }
}

fn bench_hashing_and_ingestion(criterion: &mut Criterion) {
    let root = TempDir::new().unwrap();
    let store = Store::open(root.path()).unwrap();
    let bytes = vec![0x5a; 1024 * 1024];
    criterion.bench_function("hashing_blob_ingestion/1_mib", |bencher| {
        bencher.iter(|| {
            let staged = store
                .stage_publication_blob(Cursor::new(black_box(&bytes)))
                .unwrap();
            black_box(staged);
        });
    });
}

fn bench_concurrent_publication(criterion: &mut Criterion) {
    criterion.bench_function("concurrent_publication_upload/4x64_kib", |bencher| {
        bencher.iter_batched(
            || {
                let root = TempDir::new().unwrap();
                drop(Store::open(root.path()).unwrap());
                root
            },
            |root| {
                let root = Arc::new(root);
                let workers = (0..4)
                    .map(|index| {
                        let root = Arc::clone(&root);
                        thread::spawn(move || {
                            let mut store = Store::open(root.path()).unwrap();
                            let blob = store
                                .stage_publication_blob(Cursor::new(vec![index as u8; 64 * 1024]))
                                .unwrap();
                            store
                                .publish_resolving_at(
                                    PublicationIdentity {
                                        integration_namespace: "benchmark".into(),
                                        external_key: format!("worker-{index}"),
                                        project_label: "Benchmarks".into(),
                                        working_directory: "/tmp/glim-bench".into(),
                                    },
                                    request(blob, String::new(), index),
                                    index as i64 + 1,
                                )
                                .unwrap()
                        })
                    })
                    .collect::<Vec<_>>();
                for worker in workers {
                    black_box(worker.join().unwrap());
                }
            },
            BatchSize::SmallInput,
        );
    });
}

fn populated_store(post_count: usize) -> (TempDir, Store) {
    let root = TempDir::new().unwrap();
    let mut store = Store::open(root.path()).unwrap();
    let session = store
        .resolve_session("benchmark", "feed", "Benchmarks", "/tmp/glim-feed-bench")
        .unwrap();
    for index in 0..post_count {
        let blob = store
            .stage_publication_blob(Cursor::new(format!("post-{index}")))
            .unwrap();
        store
            .publish_at(
                request(blob, session.public_id.clone(), index),
                index as i64 + 1,
            )
            .unwrap();
    }
    (root, store)
}

fn bench_feed_queries(criterion: &mut Criterion) {
    let (_root, store) = populated_store(100);
    criterion.bench_function("feed_queries/latest_100", |bencher| {
        bencher.iter(|| {
            black_box(
                store
                    .global_posts(PageRequest {
                        limit: Some(100),
                        cursor: None,
                    })
                    .unwrap(),
            )
        });
    });
}

fn bench_cleanup(criterion: &mut Criterion) {
    criterion.bench_function("cleanup_garbage_collection/20_posts", |bencher| {
        bencher.iter_batched(
            || populated_store(20),
            |(_root, mut store)| {
                let public_id = store
                    .global_posts(PageRequest {
                        limit: Some(1),
                        cursor: None,
                    })
                    .unwrap()
                    .posts[0]
                    .session_public_id
                    .clone();
                black_box(store.close_session(&public_id).unwrap());
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_media_range_serving(criterion: &mut Criterion) {
    let (root, mut store) = populated_store(0);
    let session = store
        .resolve_session("benchmark", "media", "Benchmarks", "/tmp/glim-media-bench")
        .unwrap();
    let blob = store
        .stage_publication_blob(Cursor::new(vec![0x33; 1024 * 1024]))
        .unwrap();
    store
        .publish_at(request(blob, session.public_id, 0), 1)
        .unwrap();
    let app = glim::app_with_store(store);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    criterion.bench_function("media_range_serving/64_kib_of_1_mib", |bencher| {
        bencher.iter(|| {
            let response = runtime
                .block_on(
                    app.clone().oneshot(
                        Request::builder()
                            .uri("/api/v1/posts/1/files/0/content")
                            .header(header::RANGE, "bytes=131072-196607")
                            .body(Body::empty())
                            .unwrap(),
                    ),
                )
                .unwrap();
            black_box(runtime.block_on(response.into_body().collect()).unwrap());
        });
    });
    black_box(root);
}

criterion_group!(
    hardening,
    bench_hashing_and_ingestion,
    bench_concurrent_publication,
    bench_feed_queries,
    bench_cleanup,
    bench_media_range_serving,
);
criterion_main!(hardening);
