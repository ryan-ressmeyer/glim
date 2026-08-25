use std::io;

use rusqlite::{OptionalExtension, params, params_from_iter, types::Value};
use serde::{Deserialize, Serialize};

use super::{Store, StoreError};

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
pub struct PostFileRead {
    pub position: u32,
    pub filename: String,
    pub caption: Option<String>,
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
    pub files: Vec<PostFileRead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostPage {
    pub posts: Vec<PostRead>,
    pub next_cursor: Option<String>,
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
    let Some(mut post) = connection.query_row(
        "SELECT p.id, p.session_id, s.public_id, p.title, p.commentary, p.predecessor_post_id, p.published_at
         FROM posts p JOIN sessions s ON s.id = p.session_id WHERE p.id = ?1",
        [post_id],
        |row| Ok(PostRead { id: row.get(0)?, session_id: row.get(1)?, session_public_id: row.get(2)?, title: row.get(3)?, commentary: row.get(4)?, predecessor_post_id: row.get(5)?, published_at: row.get(6)?, files: vec![] }),
    ).optional()? else { return Ok(None); };
    let raw_files = connection
        .prepare(
            "SELECT f.id, f.position, f.filename, f.caption, b.hash, b.byte_size
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
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut files = Vec::with_capacity(raw_files.len());
    for (file_id, position, filename, caption, hash, byte_size) in raw_files {
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
        files.push(PostFileRead {
            position,
            filename,
            caption,
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
