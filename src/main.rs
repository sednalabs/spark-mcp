//! # SPARK MCP Main
//!
//! Entrypoint for the SPARK documentation MCP server.
//!
//! ## Rationale
//! Orchestrates the server startup, including configuration loading, search index
//! initialization (Lexical + Semantic), and transport setup (SSE).
//!
//! ## Security Boundaries
//! * **Sandbox Gating**: Inherits sandbox network restrictions from the environment.
//! * **Credential Handling**: Loads API keys for embedding backends from the environment.
//!
//! ## References
//! * **RUNBOOK**: `README.md`

use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::extract::State;
use axum::http::{
    HeaderMap, HeaderValue, Method, Request, StatusCode,
    header::{CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE},
};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use futures::stream;
use mcp_toolkit_auth::challenge::{BearerChallenge, build_bearer_challenge_value};
use mcp_toolkit_auth::{AuthContext, AuthError, Authenticator};
use mcp_toolkit_core::tool_schema::{tool_names, tool_schema_snapshot_value};
use mcp_toolkit_http::host::{base_url, validate_host_header};
use mcp_toolkit_http::oauth::{
    ResourceMetadata, authorization_server_metadata_url, oidc_metadata_url,
    resource_metadata_default, resource_metadata_hint, resource_url_from_base,
};
use mcp_toolkit_http::session::{
    BoundedSessionManager, EventStore, EventStoreConfig, RecordingSessionManager,
};
use mcp_toolkit_observability::{sanitize_error_message, sanitize_log_value};
use rmcp::transport::common::http_header::{
    EVENT_STREAM_MIME_TYPE, HEADER_LAST_EVENT_ID, HEADER_SESSION_ID,
};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService,
    session::SessionManager,
    session::local::{LocalSessionManager, SessionConfig},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use spark_mcp::admission::{AdmissionEvaluation, AdmissionOutcome, evaluate_startup_admission};
use spark_mcp::auto_reindex::AutoReindexer;
use spark_mcp::config::{
    EventStoreMode, ResumeMode, StartupAdmissionMode, StreamableHttpConfig, load_config,
};
use spark_mcp::provenance::{
    RuntimeAdmissionExtension, RuntimeProvenance, build_attestation_envelope,
    capture_runtime_provenance,
};
use spark_mcp::search::{
    CorpusMountConfig, HnswConfig, SearchConfig, SearchIndex, SemanticBackend, SemanticConfig,
};
use spark_mcp::server::SparkMcp;

#[derive(Clone)]
struct AppState {
    allowed_hosts: HashSet<String>,
    default_host: String,
    session_manager: Arc<BoundedSessionManager>,
    stateful_service: StreamableHttpService<SparkMcp, RecordingSessionManager>,
    stateless_service: Option<StreamableHttpService<SparkMcp, RecordingSessionManager>>,
    event_store: Option<EventStore>,
    resume_mode: ResumeMode,
    indexed_at_unix_ms: Option<u64>,
    startup_admission_mode: StartupAdmissionMode,
    provenance: RuntimeProvenance,
    admission: AdmissionEvaluation,
    auth: Arc<Authenticator>,
    auth_realm: String,
    auth_resource_url: Option<String>,
    auth_issuer: Option<String>,
    auth_scopes_supported: Vec<String>,
    auth_allowed_client_ids: HashSet<String>,
}

fn base_url_for_state(headers: &HeaderMap, state: &AppState) -> String {
    base_url(headers, &state.allowed_hosts, &state.default_host)
}

fn resource_url(headers: &HeaderMap, state: &AppState) -> String {
    if let Some(url) = &state.auth_resource_url {
        return url.clone();
    }
    let base = base_url_for_state(headers, state);
    resource_url_from_base(&base, "/mcp")
}

fn resource_metadata(headers: &HeaderMap, state: &AppState) -> ResourceMetadata {
    let resource = resource_url(headers, state);
    let authorization_servers = if let Some(issuer) = &state.auth_issuer {
        vec![issuer.clone()]
    } else {
        vec![base_url_for_state(headers, state)]
    };
    resource_metadata_default(
        resource,
        authorization_servers,
        state.auth_scopes_supported.clone(),
    )
}

fn build_challenge(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let resource = resource_url(headers, state);
    let resource_metadata = resource_metadata_hint(&resource);
    let scope = if state.auth_scopes_supported.is_empty() {
        None
    } else {
        Some(state.auth_scopes_supported.join(" "))
    };
    let challenge = BearerChallenge {
        realm: &state.auth_realm,
        resource_metadata: resource_metadata.as_deref(),
        scope: scope.as_deref(),
        error: None,
        error_description: None,
        error_uri: None,
    };
    build_bearer_challenge_value(&challenge)
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    let stats = state.session_manager.stats().await;
    let resume_mode = format!("{:?}", state.resume_mode).to_lowercase();
    let admission_extension = runtime_admission_extension(&state);
    Json(json!({
        "status": "ok",
        "indexed_at_unix_ms": state.indexed_at_unix_ms,
        "session": {
            "active_streams": stats.active_sessions,
            "max_streams": stats.max_sessions,
            "resume_enabled": state.resume_mode != ResumeMode::Off,
            "resume_mode": resume_mode,
        },
        "provenance": {
            "component": state.provenance.build.component,
            "server_version": state.provenance.build.server_version,
            "build_identity": state.provenance.build.build_identity,
            "source_fingerprint": state.provenance.build.source_fingerprint,
            "source": state.provenance.build.source,
        },
        "runtime_admission": admission_extension,
    }))
}

async fn attest(State(state): State<AppState>) -> impl IntoResponse {
    let envelope =
        build_attestation_envelope(&state.provenance, &runtime_admission_extension(&state));
    Json(envelope)
}

fn runtime_admission_extension(state: &AppState) -> RuntimeAdmissionExtension {
    RuntimeAdmissionExtension {
        enforcement_phase: state.startup_admission_mode.enforcement_phase().to_string(),
        required_gate_level: state.admission.profile.label().to_string(),
        outcome: admission_outcome_label(state.admission.outcome).to_string(),
        reason_code: state.admission.reason_code.clone(),
        override_active: state.admission.override_active,
    }
}

fn admission_outcome_label(outcome: AdmissionOutcome) -> &'static str {
    match outcome {
        AdmissionOutcome::Disabled => "disabled",
        AdmissionOutcome::Bypassed => "bypassed",
        AdmissionOutcome::Passed => "passed",
        AdmissionOutcome::Warning => "warn",
        AdmissionOutcome::Rejected => "rejected",
    }
}

async fn prm_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl axum::response::IntoResponse {
    let metadata = resource_metadata(&headers, &state);
    (StatusCode::OK, Json(metadata))
}

async fn oauth_authorization_metadata(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Response {
    if let Some(issuer) = &state.auth_issuer {
        let location = authorization_server_metadata_url(issuer);
        return axum::response::Redirect::temporary(&location).into_response();
    }
    let base = base_url_for_state(&headers, &state);
    Json(json!({ "issuer": base })).into_response()
}

async fn oidc_metadata(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Some(issuer) = &state.auth_issuer {
        let location = oidc_metadata_url(issuer);
        return axum::response::Redirect::temporary(&location).into_response();
    }
    let base = base_url_for_state(&headers, &state);
    Json(json!({ "issuer": base })).into_response()
}

fn auth_error_response(state: &AppState, headers: &HeaderMap, err: AuthError) -> Response<Body> {
    let (status, error_code) = match err {
        AuthError::MissingScopes => (StatusCode::FORBIDDEN, Some("insufficient_scope")),
        AuthError::TokenExpired => (StatusCode::UNAUTHORIZED, Some("invalid_token")),
        AuthError::InvalidToken | AuthError::ReplayDetected => {
            (StatusCode::UNAUTHORIZED, Some("invalid_token"))
        }
        AuthError::MissingToken => (StatusCode::UNAUTHORIZED, None),
        AuthError::ConfigError(_) => (StatusCode::INTERNAL_SERVER_ERROR, None),
        AuthError::Generic { status_code, .. } => (
            StatusCode::from_u16(status_code).unwrap_or(StatusCode::UNAUTHORIZED),
            None,
        ),
    };

    let mut challenge = build_challenge(state, headers);
    if error_code.is_some() && challenge.is_some() {
        if let Some(value) = &mut challenge {
            value.push_str(&format!(", error=\"{}\"", error_code.unwrap()));
        }
    }

    let mut response = Response::builder().status(status);
    if let Some(challenge) = challenge {
        response = response.header(WWW_AUTHENTICATE, challenge);
    }
    response
        .body(Body::from(format!("{err}")))
        .unwrap_or_else(|_| Response::new(Body::from("auth error")))
}

async fn auth_guard(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let is_discovery = path.starts_with("/.well-known/") || path.starts_with("/mcp/.well-known/");
    if is_discovery || path == "/health" {
        return next.run(req).await;
    }
    if !path.starts_with("/mcp") {
        return next.run(req).await;
    }

    match state.auth.authenticate_headers(req.headers()).await {
        Ok(context) => {
            if !state.auth_allowed_client_ids.is_empty() {
                let azp = context.azp.as_deref().unwrap_or_default();
                if azp.is_empty() || !state.auth_allowed_client_ids.contains(azp) {
                    let err = AuthError::Generic {
                        message: "client_id is not allowed for this service".to_string(),
                        status_code: StatusCode::FORBIDDEN.as_u16(),
                        code: Some("AUTH_CLIENT_NOT_ALLOWED"),
                        reason: Some("client_not_allowed"),
                    };
                    return auth_error_response(&state, req.headers(), err);
                }
            }
            req.extensions_mut().insert::<AuthContext>(context);
            next.run(req).await
        }
        Err(err) => auth_error_response(&state, req.headers(), err),
    }
}
async fn host_guard(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    if let Err(err) = validate_host_header(req.headers(), &state.allowed_hosts) {
        let status = err.status_code();
        let message = err.message();
        return Response::builder()
            .status(status)
            .body(Body::from(message))
            .unwrap_or_else(|_| Response::new(Body::from(message)));
    }

    next.run(req).await
}

fn session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HEADER_SESSION_ID)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn last_event_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(HEADER_LAST_EVENT_ID)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_initialize_payload(body: &[u8]) -> bool {
    if body.is_empty() {
        return false;
    }
    let Ok(payload) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    match payload {
        Value::Object(map) => map
            .get("method")
            .and_then(|value| value.as_str())
            .map(|value| value == "initialize")
            .unwrap_or(false),
        Value::Array(items) => items.iter().any(|item| {
            item.get("method")
                .and_then(|value| value.as_str())
                .map(|value| value == "initialize")
                .unwrap_or(false)
        }),
        _ => false,
    }
}

async fn session_exists(state: &AppState, session_id: &str) -> bool {
    let id = session_id.to_string().into();
    state
        .session_manager
        .has_session(&id)
        .await
        .unwrap_or(false)
}

async fn replay_from_event_store(
    state: &AppState,
    session_id: &str,
    last_event_id: &str,
) -> Option<Response<Body>> {
    let store = state.event_store.as_ref()?;
    let events = match store.replay_after(session_id, last_event_id).await {
        Ok(events) => events,
        Err(err) => {
            tracing::warn!(error = %err, "event store replay failed");
            return None;
        }
    };
    if events.is_empty() {
        return None;
    }
    let stream = stream::iter(events.into_iter().map(|message| {
        let mut event = if let Some(ref msg) = message.message {
            let data = serde_json::to_string(msg.as_ref()).unwrap_or_default();
            Event::default().data(data)
        } else {
            Event::default().data("")
        };
        if let Some(id) = message.event_id.clone() {
            event = event.id(id);
        }
        if let Some(retry) = message.retry {
            event = event.retry(retry);
        }
        Ok::<Event, Infallible>(event)
    }));
    let response = if let Some(interval) = state.stateful_service.config.sse_keep_alive {
        Sse::new(stream)
            .keep_alive(KeepAlive::new().interval(interval))
            .into_response()
    } else {
        Sse::new(stream).into_response()
    };
    let mut response = response;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static(EVENT_STREAM_MIME_TYPE),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Some(response.map(Body::new))
}

fn session_error(status: StatusCode, message: &str, hint: &str) -> Response<Body> {
    let body = serde_json::json!({
        "status": "error",
        "error": message,
        "hint": hint,
    });
    Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::from("{\"status\":\"error\"}")))
}

async fn forward_service<M>(
    service: StreamableHttpService<SparkMcp, M>,
    req: Request<Body>,
) -> Response<Body>
where
    M: SessionManager,
{
    let response = service.handle(req).await;
    response.map(Body::new)
}

async fn handle_mcp(State(state): State<AppState>, req: Request<Body>) -> Response<Body> {
    let method = req.method().clone();
    let session_id = session_id_from_headers(req.headers());

    match method {
        Method::POST => {
            if let Some(session_id) = session_id.clone() {
                if session_exists(&state, &session_id).await {
                    return forward_service(state.stateful_service.clone(), req).await;
                }
                if let Some(stateless) = state.stateless_service.clone() {
                    return forward_service(stateless, req).await;
                }
                return session_error(
                    StatusCode::NOT_FOUND,
                    "Invalid or expired session ID.",
                    "Re-initialize with POST /mcp to obtain a new session id.",
                );
            }

            let (parts, body) = req.into_parts();
            let bytes = match axum::body::to_bytes(body, usize::MAX).await {
                Ok(bytes) => bytes,
                Err(_) => {
                    return session_error(
                        StatusCode::BAD_REQUEST,
                        "Failed to read request body.",
                        "Retry the request.",
                    );
                }
            };
            if is_initialize_payload(&bytes) {
                let req = Request::from_parts(parts, Body::from(bytes));
                return forward_service(state.stateful_service.clone(), req).await;
            }
            if let Some(stateless) = state.stateless_service.clone() {
                let req = Request::from_parts(parts, Body::from(bytes));
                return forward_service(stateless, req).await;
            }
            session_error(
                StatusCode::BAD_REQUEST,
                "Missing session ID.",
                "Initialize with POST /mcp to obtain a session id.",
            )
        }
        Method::GET | Method::DELETE => {
            let Some(session_id) = session_id else {
                return session_error(
                    StatusCode::BAD_REQUEST,
                    "Missing session ID.",
                    "Initialize with POST /mcp to obtain a session id.",
                );
            };
            if !session_exists(&state, &session_id).await {
                if matches!(method, Method::GET) {
                    if state.resume_mode == ResumeMode::Replay {
                        if let Some(last_event_id) = last_event_id_from_headers(req.headers()) {
                            if let Some(response) =
                                replay_from_event_store(&state, &session_id, &last_event_id).await
                            {
                                return response;
                            }
                        }
                    }
                }
                return session_error(
                    StatusCode::NOT_FOUND,
                    "Invalid or expired session ID.",
                    "Re-initialize with POST /mcp to obtain a new session id.",
                );
            }
            forward_service(state.stateful_service.clone(), req).await
        }
        _ => session_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "Method not allowed.",
            "Use POST /mcp to initialize, then reuse the session id for later requests.",
        ),
    }
}

fn build_event_store(config: &StreamableHttpConfig) -> Result<Option<EventStore>, String> {
    let store_config = EventStoreConfig {
        max_streams: config.max_streams,
        max_events: config.max_events,
        ttl: config.ttl,
        encryption: config.event_store_key.clone(),
    };
    match config.event_store_mode {
        EventStoreMode::Off => Ok(None),
        EventStoreMode::Memory => Ok(Some(EventStore::memory(store_config))),
        EventStoreMode::Sqlite => {
            let path = config
                .event_store_path
                .clone()
                .ok_or_else(|| "SPARK_MCP_HTTP_EVENT_STORE_PATH must be set.".to_string())?;
            EventStore::sqlite(path, store_config)
                .map(Some)
                .map_err(|err| err.to_string())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let print_tools = wants_arg("--print-tools");
    let print_tool_schema = wants_arg("--print-tool-schema");
    if print_tools || print_tool_schema {
        let tools = SparkMcp::tool_router_spark().list_all();
        if print_tool_schema {
            println!(
                "{}",
                serde_json::to_string_pretty(&tool_schema_snapshot_value(&tools)?)?
            );
        } else {
            println!("{}", serde_json::to_string_pretty(&tool_names(&tools)?)?);
        }
        return Ok(());
    }

    let config = load_config().map_err(|err| {
        tracing::error!(
            error = %sanitize_error_message(&err.to_string(), 512),
            "invalid configuration"
        );
        err
    })?;
    let auth = Authenticator::new(config.auth_config.clone()).map_err(|err| {
        tracing::error!(
            error = %sanitize_error_message(&err.to_string(), 512),
            "invalid auth configuration"
        );
        format!("invalid auth configuration: {err}")
    })?;
    let executable_path = std::env::current_exe().map_err(|err| {
        std::io::Error::other(format!(
            "failed to resolve executable path for startup admission: {err}"
        ))
    })?;
    let runtime_provenance = capture_runtime_provenance(&executable_path);
    let admission = evaluate_startup_admission(
        &config.startup_admission,
        &executable_path,
        &runtime_provenance,
    );
    let admission_outcome = admission_outcome_label(admission.outcome);
    match admission.outcome {
        AdmissionOutcome::Rejected => {
            return Err(format!(
                "startup admission rejected ({:?}): {}",
                admission.reason_code, admission.detail
            )
            .into());
        }
        AdmissionOutcome::Warning | AdmissionOutcome::Bypassed => {
            tracing::warn!(
                outcome = admission_outcome,
                profile = admission.profile.label(),
                reason_code = ?admission.reason_code,
                gate_path = %admission.gate_path.display(),
                detail = %sanitize_log_value(&admission.detail),
                "startup admission degraded"
            );
        }
        AdmissionOutcome::Disabled | AdmissionOutcome::Passed => {
            tracing::info!(
                outcome = admission_outcome,
                profile = admission.profile.label(),
                reason_code = ?admission.reason_code,
                gate_path = %admission.gate_path.display(),
                detail = %sanitize_log_value(&admission.detail),
                "startup admission outcome"
            );
        }
    }
    tracing::info!(
        build_identity = %sanitize_log_value(&runtime_provenance.build.build_identity),
        source_fingerprint = %sanitize_log_value(&runtime_provenance.build.source_fingerprint),
        git_revision = %sanitize_log_value(&runtime_provenance.build.source.revision),
        git_reference = %sanitize_log_value(&runtime_provenance.build.source.reference),
        git_dirty = runtime_provenance.build.source.dirty,
        "startup provenance"
    );
    let semantic_backend = SemanticBackend::parse(&config.semantic_backend)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string()))?;

    let search = SearchIndex::open_or_create(
        &config.corpus_dir,
        &config.index_dir,
        SearchConfig {
            chunk_config: mcp_toolkit_docs::ChunkConfig {
                max_chars: config.chunk_max_chars,
                overlap: config.chunk_overlap,
            },
            max_file_bytes: config.max_file_bytes,
            default_limit: config.default_limit,
            max_limit: config.max_limit,
            snippet_max_chars: config.snippet_max_chars,
            semantic: SemanticConfig {
                enabled: config.semantic_enabled,
                backend: semantic_backend,
                model: config.semantic_model.clone(),
                index_dir: config.semantic_index_dir.clone(),
                cache_dir: config.semantic_cache_dir.clone(),
                build_on_start: config.semantic_build_on_start,
                batch_size: config.semantic_batch_size,
                top_k: config.semantic_top_k,
                min_score: config.semantic_min_score,
                weight: config.semantic_weight,
                hnsw: HnswConfig {
                    m: config.semantic_hnsw_m,
                    ef_construction: config.semantic_hnsw_ef_construction,
                    ef_search: config.semantic_hnsw_ef_search,
                },
            },
        },
        config.reindex,
        config
            .workspace_mounts()
            .into_iter()
            .map(|mount| CorpusMountConfig::new(mount.label, mount.path))
            .collect(),
    )?;
    let search = Arc::new(search);
    let reindexer = AutoReindexer::new(search.clone(), std::time::Duration::from_millis(3000));

    let token = CancellationToken::new();
    let mut session_config = SessionConfig::default();
    session_config.channel_capacity = config.streamable_http.max_events;
    session_config.keep_alive = config.streamable_http.ttl;
    let session_manager = Arc::new(BoundedSessionManager::new(
        LocalSessionManager::default(),
        config.streamable_http.max_streams,
        config.streamable_http.replay_enabled(),
        session_config,
    ));
    let event_store = build_event_store(&config.streamable_http)?;
    let recording_session_manager = Arc::new(RecordingSessionManager::new(
        session_manager.clone(),
        event_store.clone(),
    ));

    let service_search = search.clone();
    let service_reindexer = reindexer.clone();
    let resume_mode = config.streamable_http.resume_mode;
    let service_sessions = session_manager.clone();
    let mut stateful_server_config = StreamableHttpServerConfig::default();
    stateful_server_config.sse_retry = config.streamable_http.retry_interval;
    stateful_server_config.cancellation_token = token.child_token();
    let stateful_service = StreamableHttpService::new(
        move || {
            Ok(SparkMcp::new(
                service_search.clone(),
                service_reindexer.clone(),
                service_sessions.clone(),
                resume_mode,
            ))
        },
        recording_session_manager.clone(),
        stateful_server_config,
    );
    let stateless_service = if config.streamable_http.stateless_fallback {
        let stateless_search = search.clone();
        let stateless_reindexer = reindexer.clone();
        let stateless_sessions = session_manager.clone();
        let mut stateless_server_config = StreamableHttpServerConfig::default();
        stateless_server_config.sse_retry = None;
        stateless_server_config.cancellation_token = token.child_token();
        stateless_server_config.stateful_mode = false;
        Some(StreamableHttpService::new(
            move || {
                Ok(SparkMcp::new(
                    stateless_search.clone(),
                    stateless_reindexer.clone(),
                    stateless_sessions.clone(),
                    resume_mode,
                ))
            },
            recording_session_manager.clone(),
            stateless_server_config,
        ))
    } else {
        None
    };
    let indexed_at_unix_ms = search.index_metadata().indexed_at_unix_ms.or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_millis() as u64)
    });

    let addr: SocketAddr = config.bind_addr.parse().map_err(|err| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid bind address: {err}"),
        )
    })?;
    if !addr.ip().is_loopback() && !config.allow_non_loopback {
        return Err(format!(
            "bind address must be loopback; set SPARK_MCP_ALLOW_NON_LOOPBACK=1 to override (got {})",
            config.bind_addr
        )
        .into());
    }
    if config.allowed_hosts.is_empty() {
        return Err("SPARK_MCP_ALLOWED_HOSTS must not be empty".into());
    }
    let allowed_hosts = config.allowed_hosts.iter().cloned().collect::<HashSet<_>>();
    let default_host = if addr.is_ipv6() {
        format!("[{}]:{}", addr.ip(), addr.port())
    } else {
        format!("{}:{}", addr.ip(), addr.port())
    };

    let state = AppState {
        allowed_hosts,
        default_host,
        session_manager,
        stateful_service,
        stateless_service,
        event_store,
        resume_mode: config.streamable_http.resume_mode,
        indexed_at_unix_ms,
        startup_admission_mode: config.startup_admission.mode,
        provenance: runtime_provenance,
        admission,
        auth: Arc::new(auth),
        auth_realm: config.auth_realm.clone(),
        auth_resource_url: config.auth_resource_url.clone(),
        auth_issuer: config.auth_issuer.clone(),
        auth_scopes_supported: config.auth_scopes_supported.clone(),
        auth_allowed_client_ids: config.auth_allowed_client_ids.iter().cloned().collect(),
    };

    let router = Router::new()
        .route("/health", get(health))
        .route("/attest", get(attest))
        .route("/.well-known/oauth-protected-resource", get(prm_metadata))
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(prm_metadata),
        )
        .route(
            "/mcp/.well-known/oauth-protected-resource",
            get(prm_metadata),
        )
        .route(
            "/mcp/.well-known/oauth-protected-resource/mcp",
            get(prm_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_metadata),
        )
        .route(
            "/mcp/.well-known/oauth-authorization-server",
            get(oauth_authorization_metadata),
        )
        .route("/.well-known/openid-configuration", get(oidc_metadata))
        .route("/mcp/.well-known/openid-configuration", get(oidc_metadata))
        .route("/mcp", any(handle_mcp))
        .layer(middleware::from_fn_with_state(state.clone(), auth_guard))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .with_state(state);

    tracing::info!(
        bind_addr = %sanitize_log_value(&config.bind_addr),
        "spark-mcp listening"
    );

    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    let shutdown_token = token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        shutdown_token.cancel();
        shutdown_handle.graceful_shutdown(None);
    });

    axum_server::bind(addr)
        .handle(handle)
        .serve(router.into_make_service())
        .await?;

    Ok(())
}

fn wants_arg(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
}
