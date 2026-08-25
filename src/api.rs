use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        OriginalUri, Path, Query, State,
        rejection::{BytesRejection, JsonRejection, QueryRejection},
    },
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::storage::{ActivityReport, LifecycleReport, PageRequest, Store, StoreError};

#[derive(Clone, Default)]
pub(crate) struct ApiState {
    store: Option<Arc<Mutex<Store>>>,
}

impl ApiState {
    pub(crate) fn with_store(store: Store) -> Self {
        Self {
            store: Some(Arc::new(Mutex::new(store))),
        }
    }
}

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/sessions", post(resolve_session))
        .route(
            "/sessions/{public_id}",
            get(get_session).delete(close_session),
        )
        .route("/sessions/{public_id}/posts", get(session_posts))
        .route("/sessions/{public_id}/heartbeat", post(heartbeat))
        .route("/projects/{project_id}/posts", get(project_posts))
        .route("/posts", get(global_posts))
        .route("/posts/{post_id}", get(get_post))
}

pub(crate) async fn route_not_found(OriginalUri(uri): OriginalUri) -> impl IntoResponse {
    v1_not_found(uri.path())
}

pub(crate) async fn root_not_found(OriginalUri(uri): OriginalUri) -> Response {
    if is_v1_path(uri.path()) {
        v1_not_found(uri.path()).into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

pub(crate) async fn validate_v1_path(request: Request<Body>, next: Next) -> Response {
    if is_v1_path(request.uri().path()) && has_malformed_percent(request.uri().path()) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "Path contains malformed percent encoding",
            json!({}),
        )
        .into_response();
    }
    next.run(request).await
}

fn is_v1_path(path: &str) -> bool {
    path == "/api/v1" || path.starts_with("/api/v1/")
}

fn has_malformed_percent(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return true;
            }
            let digits = &path[index + 1..index + 3];
            let Ok(byte) = u8::from_str_radix(digits, 16) else {
                return true;
            };
            decoded.push(byte);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    std::str::from_utf8(&decoded).is_err()
}

fn v1_not_found(path: &str) -> ApiError {
    if has_malformed_percent(path) {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "Path contains malformed percent encoding",
            json!({}),
        )
    } else {
        ApiError::new(
            StatusCode::NOT_FOUND,
            "api_route_not_found",
            "API route was not found",
            json!({}),
        )
    }
}

pub(crate) async fn method_not_allowed() -> impl IntoResponse {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "HTTP method is not allowed for this API route",
        json!({}),
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveSessionRequest {
    pub integration_namespace: String,
    pub external_key: String,
    pub project_label: String,
    pub working_directory: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    limit: Option<u32>,
    cursor: Option<String>,
}

async fn resolve_session(
    State(state): State<ApiState>,
    payload: Result<Json<ResolveSessionRequest>, JsonRejection>,
) -> Result<Json<crate::storage::SessionRead>, ApiError> {
    let Json(payload) = payload.map_err(|_| ApiError::malformed_json())?;
    validate_nonblank("integration_namespace", &payload.integration_namespace)?;
    validate_nonblank("external_key", &payload.external_key)?;
    validate_nonblank("project_label", &payload.project_label)?;
    validate_nonblank("working_directory", &payload.working_directory)?;
    let public_id = with_store(state.clone(), move |store| {
        store
            .resolve_session(
                &payload.integration_namespace,
                &payload.external_key,
                &payload.project_label,
                &payload.working_directory,
            )
            .map(|session| session.public_id)
    })
    .await?;
    Ok(Json(
        with_store(state, move |store| store.session(&public_id)).await?,
    ))
}

async fn get_session(
    State(state): State<ApiState>,
    Path(public_id): Path<String>,
) -> Result<Json<crate::storage::SessionRead>, ApiError> {
    Ok(Json(
        with_store(state, move |store| store.session(&public_id)).await?,
    ))
}

async fn get_post(
    State(state): State<ApiState>,
    Path(post_id): Path<String>,
) -> Result<Json<crate::storage::PostRead>, ApiError> {
    let post_id = positive_id(&post_id)?;
    Ok(Json(
        with_store(state, move |store| store.post(post_id)).await?,
    ))
}

async fn session_posts(
    State(state): State<ApiState>,
    Path(public_id): Path<String>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<crate::storage::PostPage>, ApiError> {
    let page = page_request(query)?;
    Ok(Json(
        with_store(state, move |store| store.session_posts(&public_id, page)).await?,
    ))
}

async fn project_posts(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<crate::storage::PostPage>, ApiError> {
    let project_id = positive_id(&project_id)?;
    let page = page_request(query)?;
    Ok(Json(
        with_store(state, move |store| store.project_posts(project_id, page)).await?,
    ))
}

async fn global_posts(
    State(state): State<ApiState>,
    query: Result<Query<PageQuery>, QueryRejection>,
) -> Result<Json<crate::storage::PostPage>, ApiError> {
    let page = page_request(query)?;
    Ok(Json(
        with_store(state, move |store| store.global_posts(page)).await?,
    ))
}

async fn heartbeat(
    State(state): State<ApiState>,
    Path(public_id): Path<String>,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<ActivityReport>, ApiError> {
    let body = body.map_err(|_| ApiError::malformed_json())?;
    if !body.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "unexpected_request_body",
            "Heartbeat requests must not include a body",
            json!({}),
        ));
    }
    Ok(Json(
        with_store(state, move |store| {
            store.record_visible_viewer_heartbeat_now(&public_id)
        })
        .await?,
    ))
}

async fn close_session(
    State(state): State<ApiState>,
    Path(public_id): Path<String>,
) -> Result<Json<LifecycleReport>, ApiError> {
    Ok(Json(
        with_store(state, move |store| store.close_session(&public_id)).await?,
    ))
}

fn page_request(query: Result<Query<PageQuery>, QueryRejection>) -> Result<PageRequest, ApiError> {
    let Query(query) = query.map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_query",
            "Query parameters are malformed",
            json!({}),
        )
    })?;
    Ok(PageRequest {
        limit: query.limit,
        cursor: query.cursor,
    })
}

fn positive_id(value: &str) -> Result<i64, ApiError> {
    value
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "malformed_path",
                "Path identifier must be a positive integer",
                json!({}),
            )
        })
}

fn validate_nonblank(field: &'static str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "A required field is blank",
            json!({"field": field}),
        ))
    } else {
        Ok(())
    }
}

async fn with_store<T, F>(state: ApiState, operation: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&mut Store) -> Result<T, StoreError> + Send + 'static,
{
    let store = state.store.ok_or_else(ApiError::unavailable)?;
    tokio::task::spawn_blocking(move || {
        let mut store = store.lock().map_err(|_| ApiError::internal())?;
        operation(&mut store).map_err(ApiError::from)
    })
    .await
    .map_err(|_| ApiError::internal())?
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorEnvelope {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub details: serde_json::Map<String, Value>,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    body: ErrorEnvelope,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: &'static str, details: Value) -> Self {
        let details = match details {
            Value::Object(details) => details,
            _ => serde_json::Map::new(),
        };
        Self {
            status,
            body: ErrorEnvelope {
                error: ErrorBody {
                    code: code.to_owned(),
                    message: message.to_owned(),
                    details,
                },
            },
        }
    }
    fn malformed_json() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "malformed_json",
            "JSON request body is malformed or contains unknown fields",
            json!({}),
        )
    }
    fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            "Persistent storage is not configured",
            json!({}),
        )
    }
    fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "An internal storage error occurred",
            json!({}),
        )
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::SessionNotFound { public_id } => Self::new(
                StatusCode::NOT_FOUND,
                "session_not_found",
                "Session was not found",
                json!({"public_id": public_id}),
            ),
            StoreError::ProjectNotFound { project_id } => Self::new(
                StatusCode::NOT_FOUND,
                "project_not_found",
                "Project was not found",
                json!({"project_id": project_id}),
            ),
            StoreError::PostNotFound { post_id } | StoreError::PredecessorNotFound { post_id } => {
                Self::new(
                    StatusCode::NOT_FOUND,
                    "post_not_found",
                    "Post was not found",
                    json!({"post_id": post_id}),
                )
            }
            StoreError::InvalidPageLimit { limit, maximum } => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_page_limit",
                "Page limit is outside the supported range",
                json!({"limit": limit, "maximum": maximum}),
            ),
            StoreError::InvalidPageCursor => Self::new(
                StatusCode::BAD_REQUEST,
                "invalid_page_cursor",
                "Page cursor is invalid",
                json!({}),
            ),
            StoreError::UploadLimitExceeded { limit, attempted } => Self::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload_limit_exceeded",
                "Upload exceeds the configured limit",
                json!({"limit": limit, "attempted": attempted}),
            ),
            StoreError::GlobalBlobBudgetExceeded {
                limit,
                current,
                additional,
            } => Self::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "storage_limit_exceeded",
                "Storage budget would be exceeded",
                json!({"limit": limit, "current": current, "additional": additional}),
            ),
            StoreError::BlankPublicationTitle
            | StoreError::BlankPublicationCommentary
            | StoreError::PublicationRequiresFile
            | StoreError::DuplicateSupportPath { .. } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "validation_failed",
                "Publication validation failed",
                json!({}),
            ),
            StoreError::CrossSessionPredecessor { post_id } => Self::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Predecessor belongs to another session",
                json!({"post_id": post_id}),
            ),
            StoreError::Integrity(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_integrity_error",
                "Stored data failed integrity validation",
                json!({}),
            ),
            StoreError::Io(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_io_error",
                "A storage I/O operation failed",
                json!({}),
            ),
            StoreError::Sqlite(rusqlite::Error::SqliteFailure(failure, _))
                if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                Self::new(
                    StatusCode::CONFLICT,
                    "storage_constraint_conflict",
                    "A storage constraint rejected the operation",
                    json!({}),
                )
            }
            StoreError::Sqlite(_) => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                "A database operation failed",
                json!({}),
            ),
            _ => Self::internal(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}
