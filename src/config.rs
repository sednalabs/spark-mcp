//! # Configuration Engine
//!
//! Handles environment variable parsing and configuration for the SPARK MCP server.
//!
//! ## Rationale
//! Centralizes all server settings, ensuring that indexing parameters, search limits,
//! and semantic backend options are loaded consistently. It provides a single source
//! of truth for the server's operational environment.
//!
//! ## Security Boundaries
//! * **I/O Restriction**: Defines the `corpus_dir` used by the indexing engine.
//! * **Resource Limits**: Sets the `max_file_bytes` limit to prevent DoS attacks.

use std::env;
use std::path::PathBuf;
use std::time::Duration;

use mcp_toolkit_auth::{AuthConfig, AuthMode, AuthSecurityProfile, ClientAuthMethod};
use url::Url;
/// Consolidated configuration for the SPARK MCP server.
#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    pub allow_non_loopback: bool,
    pub allowed_hosts: Vec<String>,
    pub corpus_dir: PathBuf,
    pub workspace_root: Option<PathBuf>,
    pub include_workspace: bool,
    pub include_workspace_rust: bool,
    pub include_workspace_fstar: bool,
    pub index_dir: PathBuf,
    pub reindex: bool,
    pub streamable_http: StreamableHttpConfig,
    pub max_file_bytes: u64,
    pub chunk_max_chars: usize,
    pub chunk_overlap: usize,
    pub default_limit: usize,
    pub max_limit: usize,
    pub snippet_max_chars: usize,
    pub semantic_enabled: bool,
    pub semantic_backend: String,
    pub semantic_model: String,
    pub semantic_index_dir: PathBuf,
    pub semantic_cache_dir: Option<PathBuf>,
    pub semantic_build_on_start: bool,
    pub semantic_batch_size: usize,
    pub semantic_top_k: usize,
    pub semantic_min_score: f32,
    pub semantic_weight: f32,
    pub semantic_hnsw_m: usize,
    pub semantic_hnsw_ef_construction: usize,
    pub semantic_hnsw_ef_search: usize,
    pub auth_realm: String,
    pub auth_resource_url: Option<String>,
    pub auth_issuer: Option<String>,
    pub auth_required_scopes: Vec<String>,
    pub auth_scopes_supported: Vec<String>,
    pub auth_allowed_client_ids: Vec<String>,
    pub auth_strict_oauth: bool,
    pub auth_config: AuthConfig,
    pub startup_admission: StartupAdmissionConfig,
}

#[derive(Debug, Clone)]
pub struct WorkspaceMount {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum EventStoreMode {
    Off,
    Memory,
    Sqlite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeMode {
    Off,
    Historyless,
    Replay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupAdmissionMode {
    Off,
    Warn,
    Strict,
}

impl StartupAdmissionMode {
    pub fn enforcement_phase(self) -> &'static str {
        match self {
            StartupAdmissionMode::Off => "off",
            StartupAdmissionMode::Warn => "warn",
            StartupAdmissionMode::Strict => "strict",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestGateProfile {
    Fast,
    Standard,
}

impl TestGateProfile {
    pub fn label(self) -> &'static str {
        match self {
            TestGateProfile::Fast => "fast",
            TestGateProfile::Standard => "standard",
        }
    }
}

#[derive(Debug, Clone)]
pub struct StartupAdmissionConfig {
    pub mode: StartupAdmissionMode,
    pub required_profile: TestGateProfile,
    pub fast_gate_artifact_path: PathBuf,
    pub standard_gate_artifact_path: PathBuf,
    pub bypass: bool,
    pub bypass_reason: Option<String>,
    pub bypass_ttl_s: Option<u64>,
    pub production_mode: bool,
    pub allow_production_bypass: bool,
}

#[derive(Debug, Clone)]
pub struct StreamableHttpConfig {
    pub event_store_mode: EventStoreMode,
    pub resume_mode: ResumeMode,
    pub event_store_path: Option<String>,
    pub event_store_key: Option<mcp_toolkit_http::session::EventStoreEncryption>,
    pub max_streams: usize,
    pub max_events: usize,
    pub ttl: Option<Duration>,
    pub retry_interval: Option<Duration>,
    pub stateless_fallback: bool,
}

impl StreamableHttpConfig {
    pub fn resume_enabled(&self) -> bool {
        !matches!(self.resume_mode, ResumeMode::Off)
    }

    pub fn replay_enabled(&self) -> bool {
        matches!(self.resume_mode, ResumeMode::Replay)
    }
}

/// Load configuration from environment variables with sensible defaults.
pub fn load_config() -> Result<Config, String> {
    let auth_mode = env_setting("SPARK_MCP_AUTH_MODE", "jwks");
    let auth_mode = parse_auth_mode(&auth_mode)?;
    let auth_resource_url = env_optional_string("SPARK_MCP_AUTH_RESOURCE_URL");
    let auth_issuer = env_optional_string("SPARK_MCP_AUTH_ISSUER");

    let mut auth_config = AuthConfig::with_profile(AuthSecurityProfile::L2Strong);
    auth_config.mode = auth_mode;
    auth_config.strict_oauth = env_flag("SPARK_MCP_AUTH_STRICT_OAUTH", auth_config.strict_oauth)?;
    auth_config.jwks_url = env_optional_string("SPARK_MCP_AUTH_JWKS_URL");
    auth_config.issuer = auth_issuer.clone();
    auth_config.audience = env_optional_string("SPARK_MCP_AUTH_AUDIENCE");
    auth_config.required_scopes = env_csv("SPARK_MCP_AUTH_REQUIRED_SCOPES", "spark:read");
    auth_config.actor_claim = env_setting("SPARK_MCP_AUTH_ACTOR_CLAIM", "sub");
    auth_config.introspection_url = env_optional_string("SPARK_MCP_AUTH_INTROSPECTION_URL");
    auth_config.introspection_client_id =
        env_optional_string("SPARK_MCP_AUTH_INTROSPECTION_CLIENT_ID");
    auth_config.introspection_client_secret =
        env_optional_string("SPARK_MCP_AUTH_INTROSPECTION_CLIENT_SECRET");
    auth_config.introspection_auth_method = parse_auth_method(&env_setting(
        "SPARK_MCP_AUTH_INTROSPECTION_AUTH_METHOD",
        "client_secret_basic",
    ))?;
    auth_config.introspection_cache_ttl_s = env_f64(
        "SPARK_MCP_AUTH_INTROSPECTION_CACHE_TTL_S",
        auth_config.introspection_cache_ttl_s,
    )?;
    auth_config.introspection_force = env_flag(
        "SPARK_MCP_AUTH_INTROSPECTION_FORCE",
        auth_config.introspection_force,
    )?;
    auth_config.delegation_secret = env_optional_string("SPARK_MCP_AUTH_DELEGATION_SECRET");
    auth_config.delegation_issuer = env_setting("SPARK_MCP_AUTH_DELEGATION_ISSUER", "spark-mcp");
    auth_config.delegation_audience =
        env_setting("SPARK_MCP_AUTH_DELEGATION_AUDIENCE", "spark-mcp");
    auth_config.jti_ttl_s = env_f64("SPARK_MCP_AUTH_JTI_TTL_S", auth_config.jti_ttl_s)?;
    auth_config.jti_cache_size =
        env_i64("SPARK_MCP_AUTH_JTI_CACHE_SIZE", auth_config.jti_cache_size)?;
    auth_config.jti_enforce_bearer = env_flag(
        "SPARK_MCP_AUTH_JTI_ENFORCE_BEARER",
        auth_config.jti_enforce_bearer,
    )?;
    auth_config.clock_skew_s = env_f64("SPARK_MCP_AUTH_CLOCK_SKEW_S", auth_config.clock_skew_s)?;

    let auth_strict_oauth = auth_config.strict_oauth;
    let auth_scopes_supported = env_csv("SPARK_MCP_AUTH_SCOPES_SUPPORTED", "");

    if auth_scopes_supported.is_empty() {
        auth_config.required_scopes = auth_config
            .required_scopes
            .iter()
            .map(|scope| scope.trim().to_string())
            .filter(|scope| !scope.is_empty())
            .collect();
    }

    let auth_required_scopes = auth_config.required_scopes.clone();
    let scopes_supported = if auth_scopes_supported.is_empty() {
        auth_required_scopes.clone()
    } else {
        auth_scopes_supported.clone()
    };

    validate_url("SPARK_MCP_AUTH_RESOURCE_URL", auth_resource_url.as_deref())?;
    validate_url("SPARK_MCP_AUTH_ISSUER", auth_issuer.as_deref())?;

    let workspace_root = env_optional_path("SPARK_MCP_WORKSPACE_ROOT");
    let workspace_default = workspace_root.is_some();
    let streamable_http = load_streamable_http_config()?;
    let startup_admission = load_startup_admission_config()?;
    Ok(Config {
        bind_addr: env_setting("SPARK_MCP_BIND_ADDR", "127.0.0.1:9410"),
        allow_non_loopback: env_flag("SPARK_MCP_ALLOW_NON_LOOPBACK", false)?,
        allowed_hosts: env_csv("SPARK_MCP_ALLOWED_HOSTS", "localhost,127.0.0.1,::1"),
        corpus_dir: PathBuf::from(env_setting("SPARK_MCP_CORPUS_DIR", "corpus")),
        workspace_root,
        include_workspace: env_flag("SPARK_MCP_INCLUDE_WORKSPACE", workspace_default)?,
        include_workspace_rust: env_flag("SPARK_MCP_INCLUDE_WORKSPACE_RUST", workspace_default)?,
        include_workspace_fstar: env_flag("SPARK_MCP_INCLUDE_WORKSPACE_FSTAR", workspace_default)?,
        index_dir: PathBuf::from(env_setting("SPARK_MCP_INDEX_DIR", "data/index")),
        reindex: env_flag("SPARK_MCP_REINDEX", false)?,
        streamable_http,
        max_file_bytes: env_u64("SPARK_MCP_MAX_FILE_BYTES", 5_000_000)?,
        chunk_max_chars: env_usize("SPARK_MCP_CHUNK_MAX_CHARS", 2000)?,
        chunk_overlap: env_usize("SPARK_MCP_CHUNK_OVERLAP", 200)?,
        default_limit: env_usize("SPARK_MCP_QUERY_LIMIT_DEFAULT", 10)?,
        max_limit: env_usize("SPARK_MCP_QUERY_LIMIT_MAX", 50)?,
        snippet_max_chars: env_usize("SPARK_MCP_SNIPPET_MAX_CHARS", 360)?,
        semantic_enabled: env_flag("SPARK_MCP_SEMANTIC_ENABLED", false)?,
        semantic_backend: env_setting("SPARK_MCP_SEMANTIC_BACKEND", "hnsw"),
        semantic_model: env_setting("SPARK_MCP_SEMANTIC_MODEL", "all-minilm-l6-v2"),
        semantic_index_dir: PathBuf::from(env_setting(
            "SPARK_MCP_SEMANTIC_INDEX_DIR",
            "data/semantic",
        )),
        semantic_cache_dir: env_optional_path("SPARK_MCP_SEMANTIC_CACHE_DIR"),
        semantic_build_on_start: env_flag("SPARK_MCP_SEMANTIC_BUILD_ON_START", true)?,
        semantic_batch_size: env_usize("SPARK_MCP_SEMANTIC_BATCH_SIZE", 128)?,
        semantic_top_k: env_usize("SPARK_MCP_SEMANTIC_TOP_K", 25)?,
        semantic_min_score: env_f32("SPARK_MCP_SEMANTIC_MIN_SCORE", 0.2)?,
        semantic_weight: env_f32("SPARK_MCP_SEMANTIC_WEIGHT", 0.5)?,
        semantic_hnsw_m: env_usize("SPARK_MCP_SEMANTIC_HNSW_M", 32)?,
        semantic_hnsw_ef_construction: env_usize("SPARK_MCP_SEMANTIC_HNSW_EF_CONSTRUCTION", 200)?,
        semantic_hnsw_ef_search: env_usize("SPARK_MCP_SEMANTIC_HNSW_EF_SEARCH", 64)?,
        auth_realm: env_setting("SPARK_MCP_AUTH_REALM", "spark-mcp"),
        auth_resource_url,
        auth_issuer,
        auth_required_scopes,
        auth_scopes_supported: scopes_supported,
        auth_allowed_client_ids: env_csv("SPARK_MCP_AUTH_ALLOWED_CLIENT_IDS", ""),
        auth_strict_oauth,
        auth_config,
        startup_admission,
    })
}

impl Config {
    pub fn workspace_mounts(&self) -> Vec<WorkspaceMount> {
        let Some(root) = self.workspace_root.as_ref() else {
            if self.include_workspace || self.include_workspace_rust || self.include_workspace_fstar
            {
                tracing::warn!("SPARK_MCP_WORKSPACE_ROOT not set; workspace mounts are disabled");
            }
            return Vec::new();
        };
        let mut mounts = Vec::new();
        if self.include_workspace {
            mounts.push(WorkspaceMount {
                label: "local-spark".to_string(),
                path: root.join("spark"),
            });
        }
        if self.include_workspace_rust {
            mounts.push(WorkspaceMount {
                label: "local-rust".to_string(),
                path: root.join("rust"),
            });
        }
        if self.include_workspace_fstar {
            mounts.push(WorkspaceMount {
                label: "local-fstar".to_string(),
                path: root.join("fstar"),
            });
        }
        mounts
    }
}

pub fn load_streamable_http_config() -> Result<StreamableHttpConfig, String> {
    let mode_raw = env_setting("SPARK_MCP_HTTP_EVENT_STORE", "off")
        .trim()
        .to_lowercase();
    let event_store_mode = match mode_raw.as_str() {
        "" | "0" | "false" | "off" | "none" => EventStoreMode::Off,
        "1" | "true" | "on" | "memory" | "inmemory" => EventStoreMode::Memory,
        "sqlite" | "file" | "disk" => EventStoreMode::Sqlite,
        _ => {
            return Err(format!(
                "Unsupported SPARK_MCP_HTTP_EVENT_STORE={mode_raw:?}; use 'memory', 'sqlite', or 'off'."
            ));
        }
    };

    let resume_raw = env_setting("SPARK_MCP_HTTP_RESUME_MODE", "historyless")
        .trim()
        .to_lowercase();
    let resume_mode = match resume_raw.as_str() {
        "" | "0" | "false" | "off" | "none" => ResumeMode::Off,
        "historyless" | "history-less" | "no-history" | "nohistory" => ResumeMode::Historyless,
        "replay" | "history" | "historyful" => ResumeMode::Replay,
        _ => {
            return Err(format!(
                "Unsupported SPARK_MCP_HTTP_RESUME_MODE={resume_raw:?}; use 'off', 'historyless', or 'replay'."
            ));
        }
    };

    if matches!(resume_mode, ResumeMode::Replay) && matches!(event_store_mode, EventStoreMode::Off)
    {
        return Err(
            "SPARK_MCP_HTTP_RESUME_MODE=replay requires SPARK_MCP_HTTP_EVENT_STORE=memory|sqlite."
                .to_string(),
        );
    }
    if !matches!(resume_mode, ResumeMode::Replay)
        && !matches!(event_store_mode, EventStoreMode::Off)
    {
        return Err(
            "SPARK_MCP_HTTP_EVENT_STORE is only supported when SPARK_MCP_HTTP_RESUME_MODE=replay."
                .to_string(),
        );
    }

    let event_store_path = if matches!(event_store_mode, EventStoreMode::Sqlite) {
        let path = env_setting("SPARK_MCP_HTTP_EVENT_STORE_PATH", "data/event-store.sqlite")
            .trim()
            .to_string();
        if path.is_empty() {
            return Err(
                "SPARK_MCP_HTTP_EVENT_STORE_PATH must be set when SPARK_MCP_HTTP_EVENT_STORE is sqlite."
                    .to_string(),
            );
        }
        Some(path)
    } else {
        None
    };

    let event_store_key = env_optional_string("SPARK_MCP_HTTP_EVENT_STORE_KEY_B64")
        .map(|value| {
            mcp_toolkit_http::session::EventStoreEncryption::from_base64(&value)
                .map_err(|err| err.to_string())
        })
        .transpose()?;

    let max_streams_raw = env_i64("SPARK_MCP_HTTP_EVENT_STORE_MAX_STREAMS", 200)?;
    let max_events_raw = env_i64("SPARK_MCP_HTTP_EVENT_STORE_MAX_EVENTS", 200)?;
    let max_streams = usize::try_from(max_streams_raw.max(1))
        .map_err(|_| "SPARK_MCP_HTTP_EVENT_STORE_MAX_STREAMS must be positive.".to_string())?;
    let max_events = usize::try_from(max_events_raw.max(1))
        .map_err(|_| "SPARK_MCP_HTTP_EVENT_STORE_MAX_EVENTS must be positive.".to_string())?;

    let ttl_s = env_f64("SPARK_MCP_HTTP_EVENT_STORE_TTL_S", 120.0)?;
    let ttl = if ttl_s <= 0.0 {
        None
    } else {
        Some(Duration::from_secs_f64(ttl_s.max(1.0)))
    };

    let retry_ms = env_i64("SPARK_MCP_HTTP_RETRY_INTERVAL_MS", 0)?;
    let retry_interval = if !matches!(resume_mode, ResumeMode::Off) && retry_ms > 0 {
        Some(Duration::from_millis(retry_ms.max(1) as u64))
    } else {
        None
    };

    let stateless_fallback = env_flag("SPARK_MCP_HTTP_STATELESS_FALLBACK", true)?;

    Ok(StreamableHttpConfig {
        event_store_mode,
        resume_mode,
        event_store_path,
        event_store_key,
        max_streams,
        max_events,
        ttl,
        retry_interval,
        stateless_fallback,
    })
}

pub fn load_startup_admission_config() -> Result<StartupAdmissionConfig, String> {
    let production_mode = env_flag(
        "SPARK_MCP_BUILD_PRODUCTION",
        env_flag("MCP_BUILD_PRODUCTION", false)?,
    )?;
    let mode_default = if production_mode { "strict" } else { "warn" };
    let mode = parse_startup_admission_mode(&env_setting(
        "SPARK_MCP_STARTUP_ADMISSION_MODE",
        mode_default,
    ))?;

    let profile_default = if production_mode { "standard" } else { "fast" };
    let required_profile = parse_test_gate_profile(&env_setting(
        "SPARK_MCP_TEST_GATE_REQUIRED_PROFILE",
        profile_default,
    ))?;

    let bypass = env_flag("SPARK_MCP_STARTUP_ADMISSION_BYPASS", false)?;
    let bypass_reason = env_optional_string("SPARK_MCP_STARTUP_ADMISSION_BYPASS_REASON");
    let bypass_ttl_s = env_optional_u64("SPARK_MCP_STARTUP_ADMISSION_BYPASS_TTL_S")?;
    let allow_production_bypass = env_flag("SPARK_MCP_STARTUP_ADMISSION_ALLOW_PROD_BYPASS", false)?;

    if production_mode && matches!(mode, StartupAdmissionMode::Off) {
        return Err(
            "SPARK_MCP_STARTUP_ADMISSION_MODE=off is not allowed when SPARK_MCP_BUILD_PRODUCTION=1."
                .to_string(),
        );
    }
    if bypass {
        if bypass_reason
            .as_deref()
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            return Err(
                "SPARK_MCP_STARTUP_ADMISSION_BYPASS requires SPARK_MCP_STARTUP_ADMISSION_BYPASS_REASON."
                    .to_string(),
            );
        }
        if bypass_ttl_s.unwrap_or(0) == 0 {
            return Err(
                "SPARK_MCP_STARTUP_ADMISSION_BYPASS requires SPARK_MCP_STARTUP_ADMISSION_BYPASS_TTL_S>0."
                    .to_string(),
            );
        }
        if production_mode && !allow_production_bypass {
            return Err(
                "Production bypass requires SPARK_MCP_STARTUP_ADMISSION_ALLOW_PROD_BYPASS=1."
                    .to_string(),
            );
        }
    }

    Ok(StartupAdmissionConfig {
        mode,
        required_profile,
        fast_gate_artifact_path: PathBuf::from(env_setting(
            "SPARK_MCP_TEST_GATE_FAST_ARTIFACT_PATH",
            "data/test-gates/spark-mcp/fast.json",
        )),
        standard_gate_artifact_path: PathBuf::from(env_setting(
            "SPARK_MCP_TEST_GATE_STANDARD_ARTIFACT_PATH",
            "data/test-gates/spark-mcp/standard.json",
        )),
        bypass,
        bypass_reason,
        bypass_ttl_s,
        production_mode,
        allow_production_bypass,
    })
}

fn parse_startup_admission_mode(value: &str) -> Result<StartupAdmissionMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "off" | "0" | "false" => Ok(StartupAdmissionMode::Off),
        "warn" => Ok(StartupAdmissionMode::Warn),
        "strict" => Ok(StartupAdmissionMode::Strict),
        other => Err(format!(
            "Unsupported SPARK_MCP_STARTUP_ADMISSION_MODE={other:?}; use off, warn, or strict."
        )),
    }
}

fn parse_test_gate_profile(value: &str) -> Result<TestGateProfile, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "fast" => Ok(TestGateProfile::Fast),
        "standard" => Ok(TestGateProfile::Standard),
        other => Err(format!(
            "Unsupported SPARK_MCP_TEST_GATE_REQUIRED_PROFILE={other:?}; use fast or standard."
        )),
    }
}

fn env_setting(name: &str, fallback: &str) -> String {
    env::var(name).unwrap_or_else(|_| fallback.to_string())
}

fn env_optional_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_csv(name: &str, fallback: &str) -> Vec<String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    raw.split(',')
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn env_flag(name: &str, fallback: bool) -> Result<bool, String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    let normalized = raw.trim().to_lowercase();
    if normalized.is_empty() {
        return Ok(fallback);
    }
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(format!("Invalid {name}={raw} (expected bool).")),
    }
}

fn env_u64(name: &str, fallback: u64) -> Result<u64, String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(fallback);
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| format!("Invalid {name}={raw} (expected integer)."))
}

fn env_optional_u64(name: &str) -> Result<Option<u64>, String> {
    let Some(raw) = env::var(name).ok() else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let parsed = trimmed
        .parse::<u64>()
        .map_err(|_| format!("Invalid {name}={raw} (expected integer)."))?;
    Ok(Some(parsed))
}

fn env_i64(name: &str, fallback: i64) -> Result<i64, String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(fallback);
    }
    trimmed
        .parse::<i64>()
        .map_err(|_| format!("Invalid {name}={raw} (expected integer)."))
}

fn env_usize(name: &str, fallback: usize) -> Result<usize, String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(fallback);
    }
    trimmed
        .parse::<usize>()
        .map_err(|_| format!("Invalid {name}={raw} (expected integer)."))
}

fn env_optional_path(name: &str) -> Option<PathBuf> {
    let raw = env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn env_f32(name: &str, fallback: f32) -> Result<f32, String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(fallback);
    }
    trimmed
        .parse::<f32>()
        .map_err(|_| format!("Invalid {}={} (expected float).", name, raw))
}

fn env_f64(name: &str, fallback: f64) -> Result<f64, String> {
    let raw = env::var(name).unwrap_or_else(|_| fallback.to_string());
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(fallback);
    }
    trimmed
        .parse::<f64>()
        .map_err(|_| format!("Invalid {}={} (expected float).", name, raw))
}

fn validate_url(name: &str, value: Option<&str>) -> Result<(), String> {
    if let Some(value) = value {
        Url::parse(value).map_err(|err| format!("invalid URL for {}: {}", name, err))?;
    }
    Ok(())
}

fn parse_auth_mode(raw: &str) -> Result<AuthMode, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "jwks" => Ok(AuthMode::Jwks),
        "introspection" => Ok(AuthMode::Introspection),
        "delegation" => Ok(AuthMode::Delegation),
        "" => Err("SPARK_MCP_AUTH_MODE must not be empty".to_string()),
        "none" | "off" | "disabled" => Err("insecure auth mode is not allowed".to_string()),
        _ => Err(format!("unsupported SPARK_MCP_AUTH_MODE: {raw}")),
    }
}

fn parse_auth_method(raw: &str) -> Result<ClientAuthMethod, String> {
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "client_secret_basic" => Ok(ClientAuthMethod::ClientSecretBasic),
        "client_secret_post" => Ok(ClientAuthMethod::ClientSecretPost),
        "" => Ok(ClientAuthMethod::ClientSecretBasic),
        _ => Err(format!(
            "unsupported SPARK_MCP_AUTH_INTROSPECTION_AUTH_METHOD: {raw}"
        )),
    }
}
