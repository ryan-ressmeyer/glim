PRAGMA foreign_keys = ON;

CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    label TEXT NOT NULL,
    working_directory TEXT NOT NULL UNIQUE
);
CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE,
    integration_namespace TEXT NOT NULL,
    external_key TEXT NOT NULL,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL DEFAULT 0,
    last_activity_at INTEGER NOT NULL DEFAULT 0,
    UNIQUE (integration_namespace, external_key, project_id)
);
CREATE TABLE posts (
    id INTEGER PRIMARY KEY,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    commentary TEXT NOT NULL,
    predecessor_post_id INTEGER,
    published_at INTEGER NOT NULL DEFAULT 0,
    UNIQUE (id, session_id),
    FOREIGN KEY (predecessor_post_id, session_id) REFERENCES posts(id, session_id),
    CHECK (predecessor_post_id IS NULL OR predecessor_post_id <> id)
);
CREATE TRIGGER posts_are_immutable
BEFORE UPDATE ON posts
BEGIN
    SELECT RAISE(ABORT, 'posts are immutable');
END;
CREATE TABLE blobs (
    hash TEXT PRIMARY KEY,
    byte_size INTEGER NOT NULL CHECK (byte_size >= 0)
);
CREATE TABLE blob_references (
    id INTEGER PRIMARY KEY,
    post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    blob_hash TEXT NOT NULL REFERENCES blobs(hash),
    UNIQUE (id, post_id)
);
CREATE TABLE post_files (
    id INTEGER PRIMARY KEY,
    post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    blob_reference_id INTEGER NOT NULL UNIQUE,
    position INTEGER NOT NULL CHECK (position >= 0),
    filename TEXT NOT NULL,
    caption TEXT,
    UNIQUE (id, post_id),
    UNIQUE (post_id, position),
    FOREIGN KEY (blob_reference_id, post_id) REFERENCES blob_references(id, post_id)
);
CREATE TABLE support_assets (
    id INTEGER PRIMARY KEY,
    post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
    entry_file_id INTEGER NOT NULL,
    blob_reference_id INTEGER NOT NULL UNIQUE,
    relative_path TEXT NOT NULL,
    UNIQUE (entry_file_id, relative_path),
    FOREIGN KEY (entry_file_id, post_id) REFERENCES post_files(id, post_id),
    FOREIGN KEY (blob_reference_id, post_id) REFERENCES blob_references(id, post_id)
);
CREATE TABLE blob_deletion_queue (
    blob_hash TEXT PRIMARY KEY REFERENCES blobs(hash) ON DELETE CASCADE
);

INSERT INTO projects (id, label, working_directory)
VALUES (1, 'Legacy fixture', '/tmp/legacy-v3');
INSERT INTO sessions
    (id, public_id, integration_namespace, external_key, project_id, created_at, last_activity_at)
VALUES (1, 'legacy3', 'pi', 'legacy-v3', 1, 100, 200);
INSERT INTO posts
    (id, session_id, title, commentary, published_at)
VALUES (1, 1, 'Legacy post', 'Pre-v4 support assets', 150);

INSERT INTO blobs (hash, byte_size) VALUES
    ('0000000000000000000000000000000000000000000000000000000000000000', 10),
    ('1111111111111111111111111111111111111111111111111111111111111111', 11),
    ('2222222222222222222222222222222222222222222222222222222222222222', 12),
    ('3333333333333333333333333333333333333333333333333333333333333333', 13),
    ('4444444444444444444444444444444444444444444444444444444444444444', 14),
    ('5555555555555555555555555555555555555555555555555555555555555555', 15),
    ('6666666666666666666666666666666666666666666666666666666666666666', 16);
INSERT INTO blob_references (id, post_id, blob_hash) VALUES
    (10, 1, '0000000000000000000000000000000000000000000000000000000000000000'),
    (20, 1, '1111111111111111111111111111111111111111111111111111111111111111'),
    (101, 1, '2222222222222222222222222222222222222222222222222222222222222222'),
    (102, 1, '3333333333333333333333333333333333333333333333333333333333333333'),
    (103, 1, '4444444444444444444444444444444444444444444444444444444444444444'),
    (201, 1, '5555555555555555555555555555555555555555555555555555555555555555'),
    (202, 1, '6666666666666666666666666666666666666666666666666666666666666666');
INSERT INTO post_files
    (id, post_id, blob_reference_id, position, filename, caption)
VALUES
    (10, 1, 10, 0, 'index.html', 'Entry document'),
    (20, 1, 20, 1, 'report.html', NULL);

-- Pre-v4 insertion order deliberately differs from the v4 lexical-path backfill order.
INSERT INTO support_assets
    (id, post_id, entry_file_id, blob_reference_id, relative_path)
VALUES (103, 1, 10, 103, 'z-last.css');
INSERT INTO support_assets
    (id, post_id, entry_file_id, blob_reference_id, relative_path)
VALUES (101, 1, 10, 101, 'a-first.js');
INSERT INTO support_assets
    (id, post_id, entry_file_id, blob_reference_id, relative_path)
VALUES (102, 1, 10, 102, 'm-middle.js');
INSERT INTO support_assets
    (id, post_id, entry_file_id, blob_reference_id, relative_path)
VALUES (202, 1, 20, 202, 'images/z.png');
INSERT INTO support_assets
    (id, post_id, entry_file_id, blob_reference_id, relative_path)
VALUES (201, 1, 20, 201, 'images/a.png');

PRAGMA user_version = 3;
