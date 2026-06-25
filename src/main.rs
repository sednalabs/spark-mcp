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
    header::{CACHE_CONTROL, CONTENT_TYPE},
    uri::{PathAndQuery, Uri},
};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use futures::stream;
use mcp_toolkit_auth::surface::{
    AuthSurfaceConfig, AuthSurfaceLayer, AuthorizationServerMetadataSource, IssuerEntry,
};
use mcp_toolkit_auth::{Authenticator, discover_oidc_metadata};
use mcp_toolkit_core::tool_schema::{tool_names, tool_schema_snapshot_value};
use mcp_toolkit_http::host::validate_host_header;
use mcp_toolkit_http::oauth::AuthorizationServerMetadata;
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
    session_manager: Arc<BoundedSessionManager>,
    stateful_service: StreamableHttpService<SparkMcp, RecordingSessionManager>,
    stateless_service: Option<StreamableHttpService<SparkMcp, RecordingSessionManager>>,
    event_store: Option<EventStore>,
    resume_mode: ResumeMode,
    indexed_at_unix_ms: Option<u64>,
    startup_admission_mode: StartupAdmissionMode,
    provenance: RuntimeProvenance,
    admission: AdmissionEvaluation,
}

const LOG_ERROR_MAX: usize = 512;

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

fn public_base_url_from_bind_addr(bind_addr: &SocketAddr) -> String {
    // Local bind-address fallback; deployments should set SPARK_MCP_AUTH_RESOURCE_URL.
    let scheme = "http";
    format!("{scheme}://{bind_addr}")
}

fn public_base_url_from_resource_url(resource_url: &str) -> Result<Option<String>, String> {
    let mut parsed =
        url::Url::parse(resource_url).map_err(|err| format!("invalid auth resource URL: {err}"))?;
    parsed.set_query(None);
    parsed.set_fragment(None);

    let path = parsed.path().trim_end_matches('/').to_string();
    let Some(prefix) = path.strip_suffix("/mcp") else {
        return Ok(None);
    };
    if prefix.is_empty() {
        parsed.set_path("/");
    } else {
        parsed.set_path(prefix);
    }

    let mut value = parsed.to_string();
    while value.ends_with('/') {
        value.pop();
    }
    Ok(Some(value))
}

fn fallback_oauth_endpoints(issuer: &str) -> (String, String) {
    let trimmed = issuer.trim_end_matches('/');
    if trimmed.contains("/realms/") {
        return (
            format!("{trimmed}/protocol/openid-connect/auth"),
            format!("{trimmed}/protocol/openid-connect/token"),
        );
    }
    (
        format!("{trimmed}/oauth/authorize"),
        format!("{trimmed}/oauth/token"),
    )
}

fn url_uses_insecure_http(value: &str) -> bool {
    url::Url::parse(value)
        .map(|url| url.scheme() == "http")
        .unwrap_or(false)
}

fn auth_surface_allow_insecure_http(config: &AuthSurfaceConfig) -> bool {
    if url_uses_insecure_http(&config.public_base_url) {
        return true;
    }
    config.entries.iter().any(|entry| {
        url_uses_insecure_http(&entry.issuer)
            || url_uses_insecure_http(&entry.authorization_endpoint)
            || url_uses_insecure_http(&entry.token_endpoint)
            || entry
                .jwks_uri
                .as_deref()
                .map(url_uses_insecure_http)
                .unwrap_or(false)
            || entry
                .introspection_endpoint
                .as_deref()
                .map(url_uses_insecure_http)
                .unwrap_or(false)
            || entry
                .device_authorization_endpoint
                .as_deref()
                .map(url_uses_insecure_http)
                .unwrap_or(false)
            || entry
                .resource_url_override
                .as_deref()
                .map(url_uses_insecure_http)
                .unwrap_or(false)
    })
}

async fn build_auth_surface_layer(
    config: &spark_mcp::config::Config,
    bind_addr: &SocketAddr,
    auth: Arc<Authenticator>,
) -> Result<AuthSurfaceLayer, String> {
    let mut public_base_url = public_base_url_from_bind_addr(bind_addr);
    if let Some(resource_url) = config.auth_resource_url.as_deref() {
        match public_base_url_from_resource_url(resource_url)? {
            Some(derived) => public_base_url = derived,
            None => {
                tracing::warn!(
                    resource_url,
                    "SPARK_MCP_AUTH_RESOURCE_URL does not end with /mcp; using bind address for auth surface base URL"
                );
            }
        }
    }

    let issuer = config
        .auth_issuer
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| public_base_url.clone());

    let (default_authz, default_token) = fallback_oauth_endpoints(&issuer);
    let metadata_source = if config.auth_issuer.is_some() {
        match discover_oidc_metadata(&issuer, None).await {
            Ok(metadata) => AuthorizationServerMetadataSource::OidcDiscovery(metadata),
            Err(err) => {
                tracing::warn!(
                    issuer,
                    err = %err,
                    "failed OIDC discovery for auth surface; using fallback OAuth endpoint URLs"
                );
                AuthorizationServerMetadataSource::Explicit(AuthorizationServerMetadata {
                    issuer: issuer.clone(),
                    authorization_endpoint: default_authz,
                    token_endpoint: default_token,
                    registration_endpoint: None,
                    jwks_uri: config.auth_config.jwks_url.clone(),
                    introspection_endpoint: config.auth_config.introspection_url.clone(),
                    device_authorization_endpoint: None,
                    grant_types_supported: None,
                    client_id_metadata_document_supported: None,
                    token_endpoint_auth_methods_supported: None,
                    code_challenge_methods_supported: None,
                })
            }
        }
    } else {
        AuthorizationServerMetadataSource::Explicit(AuthorizationServerMetadata {
            issuer: issuer.clone(),
            authorization_endpoint: default_authz,
            token_endpoint: default_token,
            registration_endpoint: None,
            jwks_uri: config.auth_config.jwks_url.clone(),
            introspection_endpoint: config.auth_config.introspection_url.clone(),
            device_authorization_endpoint: None,
            grant_types_supported: None,
            client_id_metadata_document_supported: None,
            token_endpoint_auth_methods_supported: None,
            code_challenge_methods_supported: None,
        })
    };

    let mut mcp_entry = IssuerEntry::from_metadata_source(
        "/mcp",
        metadata_source,
        config.auth_realm.clone(),
        config.auth_scopes_supported.clone(),
        config.auth_allowed_client_ids.iter().cloned().collect(),
        auth,
        config.auth_resource_url.clone(),
    )
    .map_err(|err| format!("invalid auth surface metadata: {err}"))?;
    if let Some(jwks_url) = &config.auth_config.jwks_url {
        mcp_entry.jwks_uri = Some(jwks_url.clone());
    }
    if let Some(introspection_url) = &config.auth_config.introspection_url {
        mcp_entry.introspection_endpoint = Some(introspection_url.clone());
    }

    let mut surface = AuthSurfaceConfig::single_issuer(public_base_url, mcp_entry);
    surface.public_paths.insert("/health".to_string());
    surface.public_paths.insert("/attest".to_string());
    surface.allow_insecure_http = auth_surface_allow_insecure_http(&surface);

    AuthSurfaceLayer::from_config(surface)
        .map_err(|err| format!("invalid auth surface config: {err}"))
}

async fn trim_trailing_slash(req: axum::extract::Request, next: Next) -> Response {
    let mut req = req;
    let path = req.uri().path();
    if path.len() > 1 && path.ends_with('/') {
        let trimmed_path = path.trim_end_matches('/').to_string();
        let query = req.uri().query().map(|q| q.to_string());
        let normalized = match query {
            Some(query) if !query.is_empty() => format!("{trimmed_path}?{query}"),
            _ => trimmed_path,
        };
        if let Ok(path_and_query) = normalized.parse::<PathAndQuery>() {
            let mut parts = req.uri().clone().into_parts();
            parts.path_and_query = Some(path_and_query);
            if let Ok(uri) = Uri::from_parts(parts) {
                *req.uri_mut() = uri;
            }
        }
    }
    next.run(req).await
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

    let state = AppState {
        allowed_hosts,
        session_manager,
        stateful_service,
        stateless_service,
        event_store,
        resume_mode: config.streamable_http.resume_mode,
        indexed_at_unix_ms,
        startup_admission_mode: config.startup_admission.mode,
        provenance: runtime_provenance,
        admission,
    };

    let auth = Authenticator::new(config.auth_config.clone()).map_err(|err| {
        std::io::Error::other(format!(
            "invalid auth config: {}",
            sanitize_error_message(&err.to_string(), LOG_ERROR_MAX)
        ))
    })?;
    let auth = Arc::new(auth);
    let auth_layer = build_auth_surface_layer(&config, &addr, auth)
        .await
        .map_err(|err| {
            std::io::Error::other(format!(
                "invalid auth surface config: {}",
                sanitize_error_message(&err, LOG_ERROR_MAX)
            ))
        })?;

    let router = Router::new()
        .route("/health", get(health))
        .route("/attest", get(attest))
        .route("/mcp", any(handle_mcp))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .layer(middleware::from_fn(trim_trailing_slash))
        .with_state(state)
        .layer(auth_layer);

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
