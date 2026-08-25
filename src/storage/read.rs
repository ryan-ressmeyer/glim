use std::{fs::File, io};

use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use super::{ArtifactRenderer, BlobHash, BlobRecord, GitProvenance, Store, StoreError};

pub const DEFAULT_PAGE_LIMIT: u32 = 20;
pub const MAX_PAGE_LIMIT: u32 = 100;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PageRequest {
    pub limit: Option<u32>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRead {
    pub id: i64,
    pub label: String,
    pub working_directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRead {
    pub id: i64,
    pub public_id: String,
    pub integration_namespace: String,
    pub external_key: String,
    pub project: ProjectRead,
    pub created_at: i64,
    pub last_activity_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRead {
    pub hash: String,
    pub byte_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportAssetRead {
    pub relative_path: String,
    pub blob: BlobRead,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostFileRead {
    pub position: u32,
    pub filename: String,
    pub caption: Option<String>,
    pub media_type: String,
    pub renderer: ArtifactRenderer,
    pub blob: BlobRead,
    pub support_assets: Vec<SupportAssetRead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostRead {
    pub id: i64,
    pub session_id: i64,
    pub session_public_id: String,
    pub title: String,
    pub commentary: String,
    pub predecessor_post_id: Option<i64>,
    pub published_at: i64,
    pub git: Option<GitProvenance>,
    pub files: Vec<PostFileRead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostPage {
    pub posts: Vec<PostRead>,
    pub next_cursor: Option<String>,
}

#[derive(Debug)]
pub(crate) struct AssociatedArtifact {
    pub file: File,
    pub filename: String,
    pub media_type: String,
    pub byte_size: u64,
}

#[derive(Clone, Copy)]
struct Cursor {
    published_at: i64,
    post_id: i64,
}

impl Store {
    pub fn session(&self, public_id: &str) -> Result<SessionRead, StoreError> {
        self.connection
            .query_row(
                "SELECT s.id, s.public_id, s.integration_namespace, s.external_key,
                    p.id, p.label, p.working_directory, s.created_at, s.last_activity_at
             FROM sessions s JOIN projects p ON p.id = s.project_id
             WHERE s.public_id = ?1",
                [public_id],
                |row| {
                    Ok(SessionRead {
                        id: row.get(0)?,
                        public_id: row.get(1)?,
                        integration_namespace: row.get(2)?,
                        external_key: row.get(3)?,
                        project: ProjectRead {
                            id: row.get(4)?,
                            label: row.get(5)?,
                            working_directory: row.get(6)?,
                        },
                        created_at: row.get(7)?,
                        last_activity_at: row.get(8)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| StoreError::SessionNotFound {
                public_id: public_id.to_owned(),
            })
    }

    pub fn post(&self, post_id: i64) -> Result<PostRead, StoreError> {
        load_post(&self.connection, post_id)?.ok_or(StoreError::PostNotFound { post_id })
    }

    pub fn session_posts(
        &self,
        public_id: &str,
        page: PageRequest,
    ) -> Result<PostPage, StoreError> {
        let session = self.session(public_id)?;
        self.posts_page("p.session_id = ?", session.id, page)
    }

    pub fn project_posts(
        &self,
        project_id: i64,
        page: PageRequest,
    ) -> Result<PostPage, StoreError> {
        let exists = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE id = ?1)",
            [project_id],
            |row| row.get::<_, bool>(0),
        )?;
        if !exists {
            return Err(StoreError::ProjectNotFound { project_id });
        }
        self.posts_page("s.project_id = ?", project_id, page)
    }

    pub fn global_posts(&self, page: PageRequest) -> Result<PostPage, StoreError> {
        self.posts_page("1 = ?", 1_i64, page)
    }

    pub(crate) fn open_visible_artifact(
        &self,
        post_id: i64,
        position: u32,
    ) -> Result<AssociatedArtifact, StoreError> {
        let found = self
            .connection
            .query_row(
                "SELECT f.filename, f.media_type, b.hash, b.byte_size FROM post_files f
             JOIN blob_references r ON r.id=f.blob_reference_id JOIN blobs b ON b.hash=r.blob_hash
             WHERE f.post_id=?1 AND f.position=?2",
                params![post_id, i64::from(position)],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?
            .ok_or(StoreError::ArtifactNotFound)?;
        self.open_associated(found.0, found.1, found.2, found.3)
    }

    pub(crate) fn open_support_artifact(
        &self,
        post_id: i64,
        position: u32,
        relative_path: &str,
    ) -> Result<AssociatedArtifact, StoreError> {
        let found = self.connection.query_row(
            "SELECT a.relative_path, b.hash, b.byte_size FROM support_assets a
             JOIN post_files f ON f.id=a.entry_file_id JOIN blob_references r ON r.id=a.blob_reference_id
             JOIN blobs b ON b.hash=r.blob_hash WHERE a.post_id=?1 AND f.position=?2 AND a.relative_path=?3",
            params![post_id, i64::from(position), relative_path],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?)),
        ).optional()?.ok_or(StoreError::ArtifactNotFound)?;
        let hash = BlobHash::parse(&found.1).map_err(|_| invalid_metadata("blob hash"))?;
        let byte_size = u64::try_from(found.2).map_err(|_| invalid_metadata("blob byte size"))?;
        let file = self.open_blob(&BlobRecord::from_parts(hash, byte_size))?;
        let media_type = super::classification::safe_support_media_type(&found.0, &file)?;
        Ok(AssociatedArtifact {
            file,
            filename: found.0,
            media_type,
            byte_size,
        })
    }

    fn open_associated(
        &self,
        filename: String,
        media_type: String,
        hash: String,
        byte_size: i64,
    ) -> Result<AssociatedArtifact, StoreError> {
        let hash = BlobHash::parse(&hash).map_err(|_| invalid_metadata("blob hash"))?;
        let byte_size = u64::try_from(byte_size).map_err(|_| invalid_metadata("blob byte size"))?;
        let file = self.open_blob(&BlobRecord::from_parts(hash, byte_size))?;
        Ok(AssociatedArtifact {
            file,
            filename,
            media_type,
            byte_size,
        })
    }

    fn posts_page(
        &self,
        scope: &str,
        scope_value: i64,
        page: PageRequest,
    ) -> Result<PostPage, StoreError> {
        let limit = page.limit.unwrap_or(DEFAULT_PAGE_LIMIT);
        if limit == 0 || limit > MAX_PAGE_LIMIT {
            return Err(StoreError::InvalidPageLimit {
                limit,
                maximum: MAX_PAGE_LIMIT,
            });
        }
        let cursor = page.cursor.as_deref().map(parse_cursor).transpose()?;
        let mut sql = format!(
            "SELECT p.id, p.published_at FROM posts p JOIN sessions s ON s.id = p.session_id
             WHERE {scope}"
        );
        let mut values = vec![Value::Integer(scope_value)];
        if let Some(cursor) = cursor {
            sql.push_str(" AND (p.published_at < ? OR (p.published_at = ? AND p.id < ?))");
            values.extend([
                Value::Integer(cursor.published_at),
                Value::Integer(cursor.published_at),
                Value::Integer(cursor.post_id),
            ]);
        }
        sql.push_str(" ORDER BY p.published_at DESC, p.id DESC LIMIT ?");
        values.push(Value::Integer(i64::from(limit) + 1));
        let mut statement = self.connection.prepare(&sql)?;
        let mut headers = statement
            .query_map(params_from_iter(values), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = headers.len() > limit as usize;
        headers.truncate(limit as usize);
        let next_cursor = if has_more {
            headers.last().map(|(id, at)| format!("{at}:{id}"))
        } else {
            None
        };
        let posts = headers
            .into_iter()
            .map(|(id, _)| {
                load_post(&self.connection, id)?.ok_or(StoreError::PostNotFound { post_id: id })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(PostPage { posts, next_cursor })
    }
}

fn parse_cursor(value: &str) -> Result<Cursor, StoreError> {
    let (published_at, post_id) = value.split_once(':').ok_or(StoreError::InvalidPageCursor)?;
    if published_at.is_empty() || post_id.is_empty() || post_id.contains(':') {
        return Err(StoreError::InvalidPageCursor);
    }
    let published_at = published_at
        .parse()
        .map_err(|_| StoreError::InvalidPageCursor)?;
    let post_id = post_id.parse().map_err(|_| StoreError::InvalidPageCursor)?;
    if post_id <= 0 {
        return Err(StoreError::InvalidPageCursor);
    }
    Ok(Cursor {
        published_at,
        post_id,
    })
}

fn load_post(
    connection: &rusqlite::Connection,
    post_id: i64,
) -> Result<Option<PostRead>, StoreError> {
    let Some(raw) = connection
        .query_row(
            "SELECT p.id, p.session_id, s.public_id, p.title, p.commentary, p.predecessor_post_id, p.published_at,
                    p.git_root, p.git_branch, p.git_commit
             FROM posts p JOIN sessions s ON s.id = p.session_id WHERE p.id = ?1",
            [post_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )
        .optional()?
    else {
        return Ok(None);
    };
    let git = match (raw.7, raw.8, raw.9) {
        (None, None, None) => None,
        (Some(root), branch, commit) => {
            let git = GitProvenance {
                root,
                branch,
                commit,
            };
            if !git.is_valid() {
                return Err(StoreError::InvalidPostMetadata);
            }
            Some(git)
        }
        _ => return Err(StoreError::InvalidPostMetadata),
    };
    let mut post = PostRead {
        id: raw.0,
        session_id: raw.1,
        session_public_id: raw.2,
        title: raw.3,
        commentary: raw.4,
        predecessor_post_id: raw.5,
        published_at: raw.6,
        git,
        files: vec![],
    };
    let raw_files = connection
        .prepare(
            "SELECT f.id, f.position, f.filename, f.caption, f.media_type, f.renderer, b.hash, b.byte_size
         FROM post_files f JOIN blob_references r ON r.id = f.blob_reference_id
         JOIN blobs b ON b.hash = r.blob_hash WHERE f.post_id = ?1 ORDER BY f.position ASC",
        )?
        .query_map([post_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut files = Vec::with_capacity(raw_files.len());
    for (file_id, position, filename, caption, media_type, renderer, hash, byte_size) in raw_files {
        let position = u32::try_from(position).map_err(|_| invalid_metadata("file position"))?;
        let byte_size = u64::try_from(byte_size).map_err(|_| invalid_metadata("blob byte size"))?;
        let raw_assets = connection.prepare(
            "SELECT a.relative_path, b.hash, b.byte_size FROM support_assets a
             JOIN blob_references r ON r.id = a.blob_reference_id JOIN blobs b ON b.hash = r.blob_hash
             WHERE a.entry_file_id = ?1 ORDER BY a.position ASC"
        )?.query_map(params![file_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?.collect::<Result<Vec<_>, _>>()?;
        let support_assets = raw_assets
            .into_iter()
            .map(|(relative_path, hash, byte_size)| {
                Ok(SupportAssetRead {
                    relative_path,
                    blob: BlobRead {
                        hash,
                        byte_size: u64::try_from(byte_size)
                            .map_err(|_| invalid_metadata("support asset blob byte size"))?,
                    },
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let renderer = serde_json::from_value(serde_json::Value::String(renderer))
            .map_err(|_| invalid_metadata("file renderer"))?;
        files.push(PostFileRead {
            position,
            filename,
            caption,
            media_type,
            renderer,
            blob: BlobRead { hash, byte_size },
            support_assets,
        });
    }
    post.files = files;
    Ok(Some(post))
}

fn invalid_metadata(field: &'static str) -> StoreError {
    StoreError::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("invalid persisted {field}"),
    ))
}
