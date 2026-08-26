use std::{
    collections::{HashMap, HashSet},
    convert::Infallible,
    io::SeekFrom,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        DefaultBodyLimit, Multipart, OriginalUri, Path, Query, State,
        multipart::MultipartRejection,
        rejection::{BytesRejection, JsonRejection, QueryRejection},
    },
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    middleware::Next,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::{broadcast, mpsc},
};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::io::ReaderStream;

use crate::storage::{
    ActivityReport, ArtifactRenderer, GitProvenance, LifecycleReport, PageRequest, PublicationFile,
    PublicationIdentity, PublicationRequest, PublicationStagingWriter, PublicationSupportAsset,
    PublishedPublication, Store, StoreError,
};

const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_DECLARED_PARTS: usize = 256;
/// Live publication fan-out is intentionally lossy; lagging clients receive `reset`.
const LIVE_EVENT_CHANNEL_CAPACITY: usize = 256;
/// A reconnect may replay at most this many durable posts before receiving `reset`.
const LIVE_REPLAY_LIMIT: usize = 100;
const BROWSER_SESSION_LIMIT: usize = 128;
const BROWSER_SESSION_SECONDS: u64 = 12 * 60 * 60;
const BROWSER_SESSION_COOKIE: &str = "glim_session";
const HTML_CAPABILITY_LIMIT: usize = 256;
const HTML_CAPABILITY_SECONDS: u64 = 5 * 60;

#[derive(Clone)]
pub(crate) struct ApiState {
    store: Option<Arc<Mutex<Store>>>,
    events: broadcast::Sender<LiveEvent>,
    authentication: Option<Arc<TokenAuthentication>>,
}

struct TokenAuthentication {
    access_token: crate::daemon::AccessToken,
    expected_origin: String,
    secure_cookie: bool,
    sessions: Mutex<HashMap<String, u64>>,
    capabilities: Mutex<HashMap<String, HtmlCapability>>,
}

struct HtmlCapability {
    post_id: i64,
    position: u32,
    expires_at: u64,
}

impl Default for ApiState {
    fn default() -> Self {
        let (events, _) = broadcast::channel(LIVE_EVENT_CHANNEL_CAPACITY);
        Self {
            store: None,
            events,
            authentication: None,
        }
    }
}

impl ApiState {
    pub(crate) fn with_store(store: Store) -> Self {
        Self {
            store: Some(Arc::new(Mutex::new(store))),
            ..Self::default()
        }
    }

    pub(crate) fn with_store_and_token_auth(
        store: Store,
        access_token: crate::daemon::AccessToken,
        expected_origin: String,
        secure_cookie: bool,
    ) -> Self {
        Self {
            store: Some(Arc::new(Mutex::new(store))),
            authentication: Some(Arc::new(TokenAuthentication {
                access_token,
                expected_origin,
                secure_cookie,
                sessions: Mutex::new(HashMap::new()),
                capabilities: Mutex::new(HashMap::new()),
            })),
            ..Self::default()
        }
    }
}

#[derive(Clone)]
enum LiveEvent {
    Post {
        project_id: i64,
        post: crate::storage::PostRead,
    },
    SessionClosed {
        project_id: i64,
        session_public_id: String,
    },
}

#[derive(Clone)]
enum FeedScope {
    Global,
    Project(i64),
    Session(String),
}

pub(crate) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/auth/session",
            post(create_browser_session)
                .delete(delete_browser_session)
                .route_layer(DefaultBodyLimit::max(1024)),
        )
        .route("/sessions", post(resolve_session))
        .route(
            "/sessions/{public_id}",
            get(get_session).delete(close_session),
        )
        .route("/sessions/{public_id}/posts", get(session_posts))
        .route("/sessions/{public_id}/posts/events", get(session_events))
        .route("/sessions/{public_id}/heartbeat", post(heartbeat))
        .route("/projects/{project_id}/posts", get(project_posts))
        .route("/projects/{project_id}/posts/events", get(project_events))
        .route("/posts/events", get(global_events))
        .route("/posts", get(global_posts))
        .route(
            "/posts",
            post(publish_post).route_layer(DefaultBodyLimit::disable()),
        )
        .route("/posts/{post_id}", get(get_post))
        .route(
            "/posts/{post_id}/files/{position}/html-capability",
            post(create_html_capability),
        )
        .route(
            "/posts/{post_id}/files/{position}/content",
            get(visible_artifact).head(visible_artifact),
        )
        .route(
            "/posts/{post_id}/files/{position}/support/{*asset_path}",
            get(support_artifact).head(support_artifact),
        )
}

pub(crate) fn capability_routes() -> Router<ApiState> {
    Router::new().route(
        "/cap/{capability}/api/v1/posts/{post_id}/files/{position}/support/{*asset_path}",
        get(capability_support_artifact).head(capability_support_artifact),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrowserSessionRequest {
    token: String,
}

enum AuthenticatedBy {
    Bearer,
    Cookie,
}

pub(crate) async fn authenticate_request(
    State(state): State<ApiState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let Some(authentication) = state.authentication.as_ref() else {
        return next.run(request).await;
    };
    let path = request.uri().path();
    let is_public = path == "/api/v1/health"
        || (path == "/api/v1/auth/session" && request.method() == Method::POST)
        || path == "/login"
        || path == "/assets/app.js"
        || path == "/assets/pdf.worker.mjs"
        || path.starts_with("/cap/");
    if is_public {
        return next.run(request).await;
    }
    let authenticated_by = authenticate_headers(authentication, request.headers());
    let Some(authenticated_by) = authenticated_by else {
        return authentication_failure(path);
    };
    if matches!(authenticated_by, AuthenticatedBy::Cookie)
        && !matches!(
            *request.method(),
            Method::GET | Method::HEAD | Method::OPTIONS
        )
        && request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            != Some(authentication.expected_origin.as_str())
    {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "origin_rejected",
            "Cookie-authenticated mutation requires the configured origin",
            json!({}),
        )
        .into_response();
    }
    next.run(request).await
}

fn authenticate_headers(
    authentication: &TokenAuthentication,
    headers: &HeaderMap,
) -> Option<AuthenticatedBy> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        let supplied = value.to_str().ok()?.strip_prefix("Bearer ")?;
        return constant_time_equal(authentication.access_token.expose(), supplied)
            .then_some(AuthenticatedBy::Bearer);
    }
    let session = cookie_value(headers, BROWSER_SESSION_COOKIE)?;
    let now = unix_seconds().ok()?;
    let mut sessions = authentication.sessions.lock().ok()?;
    sessions.retain(|_, expires_at| *expires_at > now);
    sessions
        .get(session)
        .is_some_and(|expires_at| *expires_at > now)
        .then_some(AuthenticatedBy::Cookie)
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .map(str::trim)
        .find_map(|value| value.strip_prefix(name)?.strip_prefix('='))
}

fn authentication_failure(path: &str) -> Response {
    if !is_v1_path(path) {
        return (
            StatusCode::SEE_OTHER,
            [(header::LOCATION, HeaderValue::from_static("/login"))],
        )
            .into_response();
    }
    let mut response = ApiError::new(
        StatusCode::UNAUTHORIZED,
        "authentication_required",
        "Authentication is required",
        json!({}),
    )
    .into_response();
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"glim\""),
    );
    response
}

async fn create_browser_session(
    State(state): State<ApiState>,
    payload: Result<Json<BrowserSessionRequest>, JsonRejection>,
) -> Response {
    let Some(authentication) = state.authentication.as_ref() else {
        return authentication_not_configured().into_response();
    };
    let Ok(Json(payload)) = payload else {
        return ApiError::malformed_json().into_response();
    };
    if !constant_time_equal(authentication.access_token.expose(), &payload.token) {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "Credentials were rejected",
            json!({}),
        )
        .into_response();
    }
    let session = match random_hex(32) {
        Ok(session) => session,
        Err(error) => return error.into_response(),
    };
    let now = match unix_seconds() {
        Ok(now) => now,
        Err(error) => return error.into_response(),
    };
    let mut sessions = match authentication.sessions.lock() {
        Ok(sessions) => sessions,
        Err(_) => return ApiError::internal().into_response(),
    };
    sessions.retain(|_, expires_at| *expires_at > now);
    if sessions.len() >= BROWSER_SESSION_LIMIT
        && let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, expires_at)| **expires_at)
            .map(|(session, _)| session.clone())
    {
        sessions.remove(&oldest);
    }
    sessions.insert(session.clone(), now.saturating_add(BROWSER_SESSION_SECONDS));
    drop(sessions);

    let secure = if authentication.secure_cookie {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{BROWSER_SESSION_COOKIE}={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age={BROWSER_SESSION_SECONDS}{secure}"
    );
    session_cookie_response(cookie)
}

async fn delete_browser_session(State(state): State<ApiState>, headers: HeaderMap) -> Response {
    let Some(authentication) = state.authentication.as_ref() else {
        return authentication_not_configured().into_response();
    };
    if let Some(session) = cookie_value(&headers, BROWSER_SESSION_COOKIE)
        && let Ok(mut sessions) = authentication.sessions.lock()
    {
        sessions.remove(session);
    }
    let secure = if authentication.secure_cookie {
        "; Secure"
    } else {
        ""
    };
    let cookie =
        format!("{BROWSER_SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{secure}");
    session_cookie_response(cookie)
}

fn session_cookie_response(cookie: String) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    match HeaderValue::from_str(&cookie) {
        Ok(cookie) => {
            response.headers_mut().insert(header::SET_COOKIE, cookie);
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(_) => ApiError::internal().into_response(),
    }
}

fn authentication_not_configured() -> ApiError {
    ApiError::new(
        StatusCode::CONFLICT,
        "authentication_not_configured",
        "Token authentication is not configured",
        json!({}),
    )
}

fn unix_seconds() -> Result<u64, ApiError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ApiError::internal())
}

fn random_hex(byte_count: usize) -> Result<String, ApiError> {
    let mut bytes = vec![0_u8; byte_count];
    getrandom::fill(&mut bytes).map_err(|_| ApiError::internal())?;
    let mut encoded = String::with_capacity(byte_count * 2);
    use std::fmt::Write as _;
    for byte in bytes {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok(encoded)
}

fn constant_time_equal(expected: &str, supplied: &str) -> bool {
    if expected.len() != supplied.len() {
        return false;
    }
    expected
        .bytes()
        .zip(supplied.bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

#[derive(Serialize)]
struct HtmlCapabilityResponse {
    path_prefix: String,
    expires_in_seconds: u64,
}

async fn create_html_capability(
    State(state): State<ApiState>,
    Path((post_id, position)): Path<(String, String)>,
) -> Result<Json<HtmlCapabilityResponse>, ApiError> {
    let post_id = positive_id(&post_id)?;
    let position = position.parse::<u32>().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "File position must be a nonnegative integer",
            json!({}),
        )
    })?;
    let valid_html = with_store(state.clone(), move |store| {
        Ok(store
            .post(post_id)?
            .files
            .into_iter()
            .any(|file| file.position == position && file.renderer == ArtifactRenderer::Html))
    })
    .await?;
    if !valid_html {
        return Err(capability_not_found());
    }
    let Some(authentication) = state.authentication.as_ref() else {
        return Ok(Json(HtmlCapabilityResponse {
            path_prefix: format!("/api/v1/posts/{post_id}/files/{position}/support/"),
            expires_in_seconds: HTML_CAPABILITY_SECONDS,
        }));
    };
    let capability = random_hex(32)?;
    let now = unix_seconds()?;
    let mut capabilities = authentication
        .capabilities
        .lock()
        .map_err(|_| ApiError::internal())?;
    capabilities.retain(|_, value| value.expires_at > now);
    if capabilities.len() >= HTML_CAPABILITY_LIMIT
        && let Some(oldest) = capabilities
            .iter()
            .min_by_key(|(_, value)| value.expires_at)
            .map(|(key, _)| key.clone())
    {
        capabilities.remove(&oldest);
    }
    capabilities.insert(
        capability.clone(),
        HtmlCapability {
            post_id,
            position,
            expires_at: now.saturating_add(HTML_CAPABILITY_SECONDS),
        },
    );
    Ok(Json(HtmlCapabilityResponse {
        path_prefix: format!("/cap/{capability}/api/v1/posts/{post_id}/files/{position}/support/"),
        expires_in_seconds: HTML_CAPABILITY_SECONDS,
    }))
}

async fn capability_support_artifact(
    State(state): State<ApiState>,
    method: Method,
    headers: HeaderMap,
    Path((capability, post_id, position, asset_path)): Path<(String, String, String, String)>,
) -> Result<Response, ApiError> {
    let post_id = positive_id(&post_id)?;
    let position = position.parse::<u32>().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "File position must be a nonnegative integer",
            json!({}),
        )
    })?;
    let authentication = state
        .authentication
        .as_ref()
        .ok_or_else(capability_not_found)?;
    let now = unix_seconds()?;
    let allowed = {
        let mut capabilities = authentication
            .capabilities
            .lock()
            .map_err(|_| ApiError::internal())?;
        capabilities.retain(|_, value| value.expires_at > now);
        capabilities.get_mut(&capability).is_some_and(|value| {
            let allowed =
                value.post_id == post_id && value.position == position && value.expires_at > now;
            if allowed {
                value.expires_at = now.saturating_add(HTML_CAPABILITY_SECONDS);
            }
            allowed
        })
    };
    if !allowed {
        return Err(capability_not_found());
    }
    support_artifact_response(state, method, headers, post_id, position, asset_path).await
}

fn capability_not_found() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "artifact_not_found",
        "Artifact was not found",
        json!({}),
    )
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
    let path = request.uri().path();
    if is_v1_path(path) && artifact_path_is_unsafe(path) {
        return ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "Artifact path is malformed",
            json!({}),
        )
        .into_response();
    }
    if is_v1_path(path) && has_malformed_percent(path) {
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

fn artifact_path_is_unsafe(path: &str) -> bool {
    if !path.contains("/support/") {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    lower.contains("%2f")
        || lower.contains("%5c")
        || path.contains("//")
        || path.split('/').any(|segment| {
            segment == "."
                || segment == ".."
                || segment.eq_ignore_ascii_case("%2e")
                || segment.eq_ignore_ascii_case("%2e%2e")
        })
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationManifest {
    pub integration_namespace: String,
    pub external_key: String,
    pub project_label: String,
    pub working_directory: String,
    pub title: String,
    pub commentary: String,
    pub predecessor_post_id: Option<i64>,
    pub git: Option<GitProvenance>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub part: String,
    pub filename: String,
    pub caption: Option<String>,
    pub media_type: Option<String>,
    #[serde(default)]
    pub support_assets: Vec<ManifestSupportAsset>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSupportAsset {
    pub part: String,
    pub relative_path: String,
}

#[derive(Debug, Serialize)]
struct PublicationResponse {
    session: crate::storage::SessionRead,
    post: crate::storage::PostRead,
}

async fn publish_post(
    State(state): State<ApiState>,
    multipart: Result<Multipart, MultipartRejection>,
) -> Result<(StatusCode, Json<PublicationResponse>), ApiError> {
    let mut multipart = multipart.map_err(|_| {
        ApiError::multipart(
            StatusCode::BAD_REQUEST,
            "malformed_multipart",
            "Multipart request is malformed",
        )
    })?;
    let mut manifest_field = multipart
        .next_field()
        .await
        .map_err(|_| {
            ApiError::multipart(
                StatusCode::BAD_REQUEST,
                "multipart_stream_error",
                "Multipart request stream was interrupted",
            )
        })?
        .ok_or_else(|| {
            ApiError::multipart(
                StatusCode::BAD_REQUEST,
                "manifest_must_be_first",
                "The first multipart part must be manifest",
            )
        })?;
    if manifest_field.name() != Some("manifest") {
        return Err(ApiError::multipart(
            StatusCode::BAD_REQUEST,
            "manifest_must_be_first",
            "The first multipart part must be manifest",
        ));
    }
    let mut manifest_bytes = Vec::new();
    while let Some(chunk) = manifest_field.chunk().await.map_err(|_| {
        ApiError::multipart(
            StatusCode::BAD_REQUEST,
            "multipart_stream_error",
            "Multipart request stream was interrupted",
        )
    })? {
        if manifest_bytes.len().saturating_add(chunk.len()) > MAX_MANIFEST_BYTES {
            return Err(ApiError::multipart(
                StatusCode::PAYLOAD_TOO_LARGE,
                "manifest_too_large",
                "Publication manifest exceeds 64 KiB",
            ));
        }
        manifest_bytes.extend_from_slice(&chunk);
    }
    drop(manifest_field);
    let manifest_text = std::str::from_utf8(&manifest_bytes).map_err(|_| {
        ApiError::multipart(
            StatusCode::BAD_REQUEST,
            "manifest_not_utf8",
            "Publication manifest must be UTF-8",
        )
    })?;
    let manifest: PublicationManifest = serde_json::from_str(manifest_text).map_err(|_| {
        ApiError::multipart(
            StatusCode::BAD_REQUEST,
            "malformed_manifest",
            "Publication manifest JSON is malformed or contains unknown fields",
        )
    })?;
    let declared = validate_manifest(&manifest)?;

    let mut staged = HashMap::with_capacity(declared.len());
    while let Some(mut field) = multipart.next_field().await.map_err(|_| {
        ApiError::multipart(
            StatusCode::BAD_REQUEST,
            "multipart_stream_error",
            "Multipart request stream was interrupted",
        )
    })? {
        let name = field
            .name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| {
                ApiError::multipart(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_part_name",
                    "Every artifact part must have a nonempty name",
                )
            })?
            .to_owned();
        if name == "manifest" {
            return Err(ApiError::multipart(
                StatusCode::UNPROCESSABLE_ENTITY,
                "duplicate_manifest",
                "The manifest part may appear only once",
            ));
        }
        if !declared.contains(&name) {
            return Err(ApiError::multipart(
                StatusCode::UNPROCESSABLE_ENTITY,
                "unexpected_part",
                "Multipart request contains an undeclared part",
            ));
        }
        if staged.contains_key(&name) {
            return Err(ApiError::multipart(
                StatusCode::UNPROCESSABLE_ENTITY,
                "duplicate_part",
                "A declared artifact part appeared more than once",
            ));
        }
        let mut writer =
            with_store(state.clone(), |store| store.publication_staging_writer()).await?;
        while let Some(chunk) = field.chunk().await.map_err(|_| {
            ApiError::multipart(
                StatusCode::BAD_REQUEST,
                "multipart_stream_error",
                "Multipart request stream was interrupted",
            )
        })? {
            writer = write_staging_chunk(writer, chunk).await?;
        }
        staged.insert(name, finish_staging_writer(writer).await?);
    }
    if staged.len() != declared.len() {
        return Err(ApiError::multipart(
            StatusCode::UNPROCESSABLE_ENTITY,
            "missing_part",
            "One or more declared artifact parts are missing",
        ));
    }

    let identity = PublicationIdentity {
        integration_namespace: manifest.integration_namespace,
        external_key: manifest.external_key,
        project_label: manifest.project_label,
        working_directory: manifest.working_directory,
    };
    let declared_media_types = manifest
        .files
        .iter()
        .map(|file| file.media_type.clone())
        .collect();
    let files = manifest
        .files
        .into_iter()
        .map(|file| {
            let support_assets = file
                .support_assets
                .into_iter()
                .map(|asset| PublicationSupportAsset {
                    relative_path: asset.relative_path,
                    blob: staged
                        .remove(&asset.part)
                        .expect("validated part is staged"),
                })
                .collect();
            PublicationFile {
                filename: file.filename,
                caption: file.caption,
                blob: staged.remove(&file.part).expect("validated part is staged"),
                support_assets,
            }
        })
        .collect();
    let request = PublicationRequest {
        session_public_id: String::new(),
        title: manifest.title,
        commentary: manifest.commentary,
        predecessor_post_id: manifest.predecessor_post_id,
        git: manifest.git,
        files,
    };
    let published_at = daemon_unix_seconds()?;
    let events = state.events.clone();
    let PublishedPublication { session, post } = with_store(state, move |store| {
        let publication = store.publish_resolving_classified_at(
            identity,
            request,
            published_at,
            Some(declared_media_types),
        )?;
        let _ = events.send(LiveEvent::Post {
            project_id: publication.session.project.id,
            post: publication.post.clone(),
        });
        Ok(publication)
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(PublicationResponse { session, post }),
    ))
}

fn daemon_unix_seconds() -> Result<i64, ApiError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApiError::internal())?
        .as_secs();
    i64::try_from(seconds).map_err(|_| ApiError::internal())
}

fn validate_manifest(manifest: &PublicationManifest) -> Result<HashSet<String>, ApiError> {
    for (field, value) in [
        (
            "integration_namespace",
            manifest.integration_namespace.as_str(),
        ),
        ("external_key", manifest.external_key.as_str()),
        ("project_label", manifest.project_label.as_str()),
        ("working_directory", manifest.working_directory.as_str()),
        ("title", manifest.title.as_str()),
        ("commentary", manifest.commentary.as_str()),
    ] {
        validate_nonblank(field, value)?;
    }
    if manifest.git.as_ref().is_some_and(|git| !git.is_valid()) {
        return Err(ApiError::multipart(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "Git provenance is malformed",
        ));
    }
    if manifest.files.is_empty() {
        return Err(ApiError::multipart(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "Publication requires at least one visible file",
        ));
    }
    let count = manifest
        .files
        .iter()
        .map(|file| 1 + file.support_assets.len())
        .sum::<usize>();
    if count > MAX_DECLARED_PARTS {
        return Err(ApiError::multipart(
            StatusCode::UNPROCESSABLE_ENTITY,
            "manifest_complexity_exceeded",
            "Publication declares more than 256 byte parts",
        ));
    }
    let mut parts = HashSet::with_capacity(count);
    for file in &manifest.files {
        if file.part.is_empty() {
            return Err(ApiError::multipart(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid_part_name",
                "Declared part names must be nonempty",
            ));
        }
        if !parts.insert(file.part.clone()) {
            return Err(ApiError::multipart(
                StatusCode::UNPROCESSABLE_ENTITY,
                "duplicate_part",
                "Declared part names must be unique",
            ));
        }
        let mut paths = HashSet::new();
        for asset in &file.support_assets {
            if asset.part.is_empty() {
                return Err(ApiError::multipart(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "invalid_part_name",
                    "Declared part names must be nonempty",
                ));
            }
            if !parts.insert(asset.part.clone()) {
                return Err(ApiError::multipart(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "duplicate_part",
                    "Declared part names must be unique",
                ));
            }
            if !paths.insert(asset.relative_path.as_str()) {
                return Err(ApiError::multipart(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "duplicate_support_path",
                    "Support paths must be unique within a visible file",
                ));
            }
        }
    }
    Ok(parts)
}

async fn write_staging_chunk(
    mut writer: PublicationStagingWriter,
    chunk: Bytes,
) -> Result<PublicationStagingWriter, ApiError> {
    tokio::task::spawn_blocking(move || {
        writer.write_chunk(&chunk)?;
        Ok::<_, StoreError>(writer)
    })
    .await
    .map_err(|_| ApiError::internal())?
    .map_err(ApiError::from)
}

async fn finish_staging_writer(
    writer: PublicationStagingWriter,
) -> Result<crate::storage::StagedPublicationBlob, ApiError> {
    tokio::task::spawn_blocking(move || writer.finish())
        .await
        .map_err(|_| ApiError::internal())?
        .map_err(ApiError::from)
}

async fn visible_artifact(
    State(state): State<ApiState>,
    method: Method,
    headers: HeaderMap,
    Path((post_id, position)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let post_id = positive_id(&post_id)?;
    let position = position.parse::<u32>().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "File position must be a nonnegative integer",
            json!({}),
        )
    })?;
    let artifact = with_store(state, move |store| {
        store.open_visible_artifact(post_id, position)
    })
    .await?;
    artifact_response(method, headers, artifact).await
}

async fn support_artifact(
    State(state): State<ApiState>,
    method: Method,
    headers: HeaderMap,
    Path((post_id, position, asset_path)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let post_id = positive_id(&post_id)?;
    let position = position.parse::<u32>().map_err(|_| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "File position must be a nonnegative integer",
            json!({}),
        )
    })?;
    support_artifact_response(state, method, headers, post_id, position, asset_path).await
}

async fn support_artifact_response(
    state: ApiState,
    method: Method,
    headers: HeaderMap,
    post_id: i64,
    position: u32,
    asset_path: String,
) -> Result<Response, ApiError> {
    if asset_path.is_empty()
        || asset_path.contains('\\')
        || asset_path.chars().any(char::is_control)
        || asset_path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "malformed_path",
            "Support asset path is malformed",
            json!({}),
        ));
    }
    let requested = asset_path.clone();
    let artifact = with_store(state, move |store| {
        store.open_support_artifact(post_id, position, &requested)
    })
    .await?;
    artifact_response(method, headers, artifact).await
}

async fn artifact_response(
    method: Method,
    headers: HeaderMap,
    artifact: crate::storage::AssociatedArtifact,
) -> Result<Response, ApiError> {
    let range = parse_range(headers.get(header::RANGE), artifact.byte_size);
    let (status, start, length, content_range) = match range {
        Ok(Some((start, end))) => (
            StatusCode::PARTIAL_CONTENT,
            start,
            end - start + 1,
            Some(format!("bytes {start}-{end}/{}", artifact.byte_size)),
        ),
        Ok(None) => (StatusCode::OK, 0, artifact.byte_size, None),
        Err(()) => {
            let mut response = Response::new(Body::empty());
            *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
            artifact_headers(response.headers_mut(), &artifact, 0)?;
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{}", artifact.byte_size))
                    .map_err(|_| ApiError::internal())?,
            );
            return Ok(response);
        }
    };
    let mut response_headers = HeaderMap::new();
    artifact_headers(&mut response_headers, &artifact, length)?;
    if let Some(value) = content_range {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&value).map_err(|_| ApiError::internal())?,
        );
    }
    let mut response = if method == Method::HEAD || length == 0 {
        Response::new(Body::empty())
    } else {
        let mut file = tokio::fs::File::from_std(artifact.file);
        file.seek(SeekFrom::Start(start))
            .await
            .map_err(StoreError::from)?;
        Response::new(Body::from_stream(ReaderStream::new(file.take(length))))
    };
    *response.status_mut() = status;
    *response.headers_mut() = response_headers;
    Ok(response)
}

fn artifact_headers(
    headers: &mut HeaderMap,
    artifact: &crate::storage::AssociatedArtifact,
    length: u64,
) -> Result<(), ApiError> {
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&artifact.media_type).map_err(|_| ApiError::internal())?,
    );
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).map_err(|_| ApiError::internal())?,
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    let disposition = if artifact.media_type == "application/octet-stream" {
        "attachment"
    } else {
        "inline"
    };
    let mut safe_name: String = artifact
        .filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if safe_name.is_empty() {
        safe_name.push_str("download");
    }
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("{disposition}; filename=\"{safe_name}\""))
            .map_err(|_| ApiError::internal())?,
    );
    Ok(())
}

fn parse_range(header: Option<&HeaderValue>, length: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(value) = header else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| ())?;
    let spec = value.strip_prefix("bytes=").ok_or(())?;
    if spec.contains(',') {
        return Err(());
    }
    let (first, last) = spec.split_once('-').ok_or(())?;
    if first.is_empty() {
        let suffix = last.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 || length == 0 {
            return Err(());
        }
        let start = length.saturating_sub(suffix);
        return Ok(Some((start, length - 1)));
    }
    let start = first.parse::<u64>().map_err(|_| ())?;
    if start >= length {
        return Err(());
    }
    let end = if last.is_empty() {
        length - 1
    } else {
        last.parse::<u64>().map_err(|_| ())?.min(length - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(Some((start, end)))
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

async fn global_events(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    live_events(state, headers, FeedScope::Global).await
}

async fn project_events(
    State(state): State<ApiState>,
    Path(project_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    live_events(
        state,
        headers,
        FeedScope::Project(positive_id(&project_id)?),
    )
    .await
}

async fn session_events(
    State(state): State<ApiState>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    live_events(state, headers, FeedScope::Session(public_id)).await
}

async fn live_events(
    state: ApiState,
    headers: HeaderMap,
    scope: FeedScope,
) -> Result<Response, ApiError> {
    let after_id = match headers.get("last-event-id") {
        None => 0,
        Some(value) => value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "malformed_last_event_id",
                    "Last-Event-ID must be a positive integer",
                    json!({}),
                )
            })?,
    };
    // Subscribe first: a commit racing durable replay can duplicate, but cannot disappear.
    let mut receiver = state.events.subscribe();
    let replay_scope = scope.clone();
    let replay = with_store(state.clone(), move |store| match replay_scope {
        FeedScope::Global => store.global_posts_after(after_id, LIVE_REPLAY_LIMIT + 1),
        FeedScope::Project(project_id) => {
            store.project_posts_after(project_id, after_id, LIVE_REPLAY_LIMIT + 1)
        }
        FeedScope::Session(public_id) => {
            store.session_posts_after(&public_id, after_id, LIVE_REPLAY_LIMIT + 1)
        }
    })
    .await?;
    let (sender, stream) = mpsc::channel::<Result<Event, Infallible>>(32);
    tokio::spawn(async move {
        if replay.len() > LIVE_REPLAY_LIMIT {
            if sender.send(Ok(reset_event("replay_limit"))).await.is_err() {
                return;
            }
        } else {
            for post in replay {
                if sender.send(Ok(post_event(&post))).await.is_err() {
                    return;
                }
            }
        }
        loop {
            match receiver.recv().await {
                Ok(event) if event_matches(&scope, &event) => {
                    let encoded = match event {
                        LiveEvent::Post { post, .. } => post_event(&post),
                        LiveEvent::SessionClosed { project_id, session_public_id } => Event::default()
                            .event("session-closed")
                            .json_data(json!({"project_id": project_id, "session_public_id": session_public_id}))
                            .expect("trusted closure event serializes"),
                    };
                    if sender.send(Ok(encoded)).await.is_err() {
                        return;
                    }
                }
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    receiver = receiver.resubscribe();
                    if sender.send(Ok(reset_event("channel_lag"))).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(stream))
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response())
}

fn event_matches(scope: &FeedScope, event: &LiveEvent) -> bool {
    match (scope, event) {
        (FeedScope::Global, _) => true,
        (
            FeedScope::Project(expected),
            LiveEvent::Post { project_id, .. } | LiveEvent::SessionClosed { project_id, .. },
        ) => expected == project_id,
        (FeedScope::Session(expected), LiveEvent::Post { post, .. }) => {
            expected == &post.session_public_id
        }
        (
            FeedScope::Session(expected),
            LiveEvent::SessionClosed {
                session_public_id, ..
            },
        ) => expected == session_public_id,
    }
}

fn post_event(post: &crate::storage::PostRead) -> Event {
    Event::default()
        .event("post")
        .id(post.id.to_string())
        .json_data(post)
        .expect("validated post serializes")
}

fn reset_event(reason: &'static str) -> Event {
    Event::default()
        .event("reset")
        .json_data(json!({"reason": reason}))
        .expect("trusted reset event serializes")
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
    let events = state.events.clone();
    let close_id = public_id.clone();
    let report = with_store(state, move |store| {
        let context = store.session(&close_id).ok();
        let report = store.close_session(&close_id)?;
        if report.sessions_deleted > 0
            && let Some(session) = context
        {
            let _ = events.send(LiveEvent::SessionClosed {
                project_id: session.project.id,
                session_public_id: close_id,
            });
        }
        Ok(report)
    })
    .await?;
    Ok(Json(report))
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
    fn multipart(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self::new(status, code, message, json!({}))
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
            StoreError::ArtifactNotFound => Self::new(
                StatusCode::NOT_FOUND,
                "artifact_not_found",
                "Associated artifact was not found",
                json!({}),
            ),
            StoreError::ArtifactClassificationFailed => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "artifact_classification_failed",
                "Artifact bytes contradict the filename or declared media type",
                json!({}),
            ),
            StoreError::BlankPublicationTitle
            | StoreError::BlankPublicationCommentary
            | StoreError::PublicationRequiresFile
            | StoreError::InvalidGitProvenance
            | StoreError::DuplicateSupportPath { .. }
            | StoreError::InvalidSupportPath { .. } => Self::new(
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
            StoreError::Integrity(_) | StoreError::InvalidPostMetadata => Self::new(
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
