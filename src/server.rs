//! # SPARK MCP Server Logic
//!
//! Implements the MCP protocol handlers and tool routing for the SPARK corpus.
//!
//! ## Rationale
//! Acts as the entrypoint for MCP clients (like Claude Desktop or Gemini CLI) to interact
//! with the SPARK documentation. It exposes tools for search and retrieval, and prompts
//! for grounded question answering.
//!
//! ## Security Boundaries
//! * **Corpus Gating**: Only documents within the verified corpus directory are accessible.
//! * **Tool Gating**: Restricts ordinary tools to retrieval and exposes reindexing only through
//!   an explicit audited maintenance tool.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::auto_reindex::AutoReindexer;
use mcp_toolkit_core::rmcp_models;
use mcp_toolkit_http::session::BoundedSessionManager;
use rmcp::handler::server::prompt::PromptContext;
use rmcp::handler::server::router::prompt::PromptRouter;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, GetPromptRequestParams, GetPromptResult, Implementation,
    ListPromptsResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler};
use std::future::Future;

use crate::config::ResumeMode;
use crate::resources;
use crate::search::SearchIndex;

#[derive(Clone)]
pub struct SparkMcp {
    pub search: Arc<SearchIndex>,
    pub reindexer: Arc<AutoReindexer>,
    tool_router: ToolRouter<SparkMcp>,
    prompt_router: PromptRouter<SparkMcp>,
    session_manager: Arc<BoundedSessionManager>,
    resume_mode: ResumeMode,
    hover_telemetry: Arc<Mutex<HoverTelemetry>>,
}

#[derive(Debug, Default)]
struct HoverTelemetry {
    input_kind_counts: HashMap<String, u64>,
    failure_reason_counts: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct HoverTelemetrySnapshot {
    pub input_kind_counts: HashMap<String, u64>,
    pub failure_reason_counts: HashMap<String, u64>,
}

impl SparkMcp {
    /// Construct a new MCP server handler with its dependencies.
    pub fn new(
        search: Arc<SearchIndex>,
        reindexer: Arc<AutoReindexer>,
        session_manager: Arc<BoundedSessionManager>,
        resume_mode: ResumeMode,
    ) -> Self {
        let tool_router = Self::tool_router_spark();
        let prompt_router = Self::prompt_router_spark();
        Self {
            search,
            reindexer,
            tool_router,
            prompt_router,
            session_manager,
            resume_mode,
            hover_telemetry: Arc::new(Mutex::new(HoverTelemetry::default())),
        }
    }

    fn resume_mode_label(&self) -> &'static str {
        match self.resume_mode {
            ResumeMode::Off => "off",
            ResumeMode::Historyless => "historyless",
            ResumeMode::Replay => "replay",
        }
    }

    pub(crate) fn record_hover_input_kind(&self, kind: &str) {
        let mut guard = self
            .hover_telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard.input_kind_counts.entry(kind.to_string()).or_insert(0) += 1;
    }

    pub(crate) fn record_hover_failure_reason(&self, reason: &str) {
        let mut guard = self
            .hover_telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
            .failure_reason_counts
            .entry(reason.to_string())
            .or_insert(0) += 1;
    }

    pub(crate) fn hover_telemetry_snapshot(&self) -> HoverTelemetrySnapshot {
        let guard = self
            .hover_telemetry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        HoverTelemetrySnapshot {
            input_kind_counts: guard.input_kind_counts.clone(),
            failure_reason_counts: guard.failure_reason_counts.clone(),
        }
    }
}

impl ServerHandler for SparkMcp {
    /// Return server metadata and capabilities (tools, prompts).
    fn get_info(&self) -> ServerInfo {
        rmcp_models::server_info(
            ProtocolVersion::V_2024_11_05,
            ServerCapabilities::builder()
                .enable_tools()
                .enable_prompts()
                .enable_resources()
                .build(),
            Implementation::from_build_env(),
            Some(
                "SPARK corpus MCP server. Use spark.search for citations; no sampling in v1."
                    .to_string(),
            ),
        )
    }

    /// List all registered tools for the SPARK corpus.
    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, rmcp::ErrorData>> + Send + '_ {
        let tools = self.tool_router.list_all();
        std::future::ready(Ok(ListToolsResult {
            meta: None,
            tools,
            next_cursor: None,
        }))
    }

    /// Execute a search or retrieval tool call.
    ///
    /// # Security
    /// * **Isolation**: Every tool call is routed through the `tool_router` to ensure
    ///   that only allow-listed search operations are executed.
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, rmcp::ErrorData>> + Send + '_ {
        let tool_context = ToolCallContext::new(self, request, context);
        async move { self.tool_router.call(tool_context).await }
    }

    /// List available prompts for grounded answering.
    fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListPromptsResult, rmcp::ErrorData>> + Send + '_ {
        let prompts = self.prompt_router.list_all();
        std::future::ready(Ok(ListPromptsResult {
            meta: None,
            prompts,
            next_cursor: None,
        }))
    }

    /// Return a formatted prompt for the model.
    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<GetPromptResult, rmcp::ErrorData>> + Send + '_ {
        let prompt_context = PromptContext::new(self, request.name, request.arguments, context);
        async move { self.prompt_router.get_prompt(prompt_context).await }
    }

    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_ {
        let search = self.search.clone();
        let session_manager = self.session_manager.clone();
        async move {
            let stats = session_manager.stats().await;
            let resources =
                resources::list_resources(&search, Some(&stats), Some(self.resume_mode_label()));
            Ok(ListResourcesResult {
                resources,
                next_cursor: None,
                meta: None,
            })
        }
    }

    fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, rmcp::ErrorData>> + Send + '_
    {
        let resource_templates = resources::list_resource_templates(&self.search);
        std::future::ready(Ok(ListResourceTemplatesResult {
            resource_templates,
            next_cursor: None,
            meta: None,
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResult, rmcp::ErrorData>> + Send + '_ {
        let search = self.search.clone();
        let session_manager = self.session_manager.clone();
        async move {
            let stats = session_manager.stats().await;
            resources::read_resource(
                &search,
                &request.uri,
                Some(&stats),
                Some(self.resume_mode_label()),
            )
        }
    }
}
