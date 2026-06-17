//! # Tool Handlers
//!
//! MCP tool implementations for searching and retrieving documentation from the corpus.
//!
//! ## Rationale
//! Provides the primary interface for agents to discover and cite documentation. It includes
//! logic for selecting the best search mode (Lexical, Semantic, or Hybrid) based on
//! availability and agent intent.
//!
//! ## Security Boundaries
//! * **Read-Mostly Enforcement**: Retrieval tools have no filesystem side effects; `spark.reindex`
//!   is the explicit audited maintenance tool and requires a reason.
//! * **Input Validation**: Ensures that document IDs are properly sanitized before retrieval.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use crate::auto_reindex::{ReindexError, ReindexErrorKind, ReindexRequest};
use crate::llm::{LlmAnswerRequest, LlmProvider, LlmRuntime};
use crate::search::{
    LexicalQueryKind, LineContext, SearchError, SearchHit, SearchMode, SourceSummary, SymbolMatch,
    SymbolMatchKind, SymbolOccurrence,
};
use crate::server::SparkMcp;

/// Arguments for `spark.search`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchArgs {
    /// The search query string.
    pub query: String,
    /// How to interpret the query for lexical search.
    ///
    /// - `tantivy` (default): interprets `query` using Tantivy query syntax.
    /// - `literal`: treats `query` as plain text (safe for signatures/code fragments).
    #[serde(default)]
    pub query_kind: SearchQueryKindArg,
    /// Maximum number of results to return.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Optional source filter (e.g. "manual", "spec").
    #[serde(default)]
    pub source: Option<String>,
    /// Optional list of source filters (overrides `source`).
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// The search mode to use (auto, lexical, semantic, hybrid).
    #[serde(default)]
    pub mode: SearchModeArg,
    /// Include line context for each result.
    #[serde(default)]
    pub include_context: bool,
    /// Override the number of context lines (default 3, max 20).
    #[serde(default)]
    pub context_lines: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SearchModeArg {
    Auto,
    Lexical,
    Semantic,
    Hybrid,
}

impl Default for SearchModeArg {
    fn default() -> Self {
        SearchModeArg::Auto
    }
}

#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SearchQueryKindArg {
    Literal,
    Tantivy,
}

impl Default for SearchQueryKindArg {
    fn default() -> Self {
        SearchQueryKindArg::Tantivy
    }
}

/// Arguments for `spark.llm_answer`.
#[derive(Debug, Deserialize, JsonSchema, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
pub enum LlmProviderArg {
    #[default]
    Gemini,
}

impl LlmProviderArg {
    fn as_provider(self) -> LlmProvider {
        match self {
            Self::Gemini => LlmProvider::Gemini,
        }
    }
}

/// Arguments for `spark.llm_answer`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LlmAnswerArgs {
    /// LLM provider selector (currently only `gemini`).
    #[serde(default)]
    pub provider: LlmProviderArg,
    /// The user's question to be answered.
    pub question: String,
}

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct ListSourcesArgs {}

/// Arguments for `spark.index_status`.
#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct IndexStatusArgs {}

/// Arguments for `spark.reindex`.
#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct ReindexArgs {
    /// Optional source labels. Defaults to local-only when `full_reindex` is false.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// Optional workspace-relative paths used for audit/scoped validation.
    #[serde(default)]
    pub workspace_paths: Option<Vec<String>>,
    /// Allow broad source selection (requires `SPARK_MCP_REINDEX_ALLOW_FULL=1`).
    #[serde(default)]
    pub full_reindex: bool,
    /// Required audit reason for running reindex.
    pub reason: String,
}

/// Arguments for `spark.get_doc`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDocArgs {
    /// The document identifier (relative path or mount-prefixed path).
    pub doc_id: String,
    /// Maximum number of characters to return (to avoid context window bloat).
    #[serde(default)]
    pub max_chars: Option<u32>,
}

/// Arguments for `spark.get_chunk`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetChunkArgs {
    /// The document identifier (relative path or mount-prefixed path).
    pub doc_id: String,
    /// The zero-based chunk index.
    pub chunk_index: u64,
}

/// Arguments for `spark.hover`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HoverArgs {
    /// File path or doc_id to inspect.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number.
    pub column: u32,
    /// Optional symbol override.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Include line context around the resolved position.
    #[serde(default)]
    pub include_context: Option<bool>,
    /// Override context lines (default 3, max 20).
    #[serde(default)]
    pub context_lines: Option<u32>,
}

/// Arguments for `spark_locate`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct LocateArgs {
    /// The symbol to locate.
    pub symbol: String,
    /// Maximum number of results to return.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Optional source filter (e.g. "manual", "spec", "local", "local-spark").
    #[serde(default)]
    pub source: Option<String>,
    /// Optional kind filter (definition|reference|both).
    #[serde(default)]
    pub kind: LocateKindArg,
    /// Include line context around matches.
    #[serde(default)]
    pub include_context: bool,
    /// Override the number of context lines (default 3, max 20).
    #[serde(default)]
    pub context_lines: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum LocateKindArg {
    Definition,
    Reference,
    Both,
}

impl Default for LocateKindArg {
    fn default() -> Self {
        LocateKindArg::Definition
    }
}

impl LocateKindArg {
    fn as_str(self) -> &'static str {
        match self {
            LocateKindArg::Definition => "definition",
            LocateKindArg::Reference => "reference",
            LocateKindArg::Both => "both",
        }
    }
}

/// Arguments for `spark_refs`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RefsArgs {
    /// The symbol to locate references for.
    pub symbol: String,
    /// Maximum number of results to return.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Optional source filter (e.g. "manual", "spec", "local", "local-spark").
    #[serde(default)]
    pub source: Option<String>,
    /// Optional kind filter (definition|reference|both).
    #[serde(default)]
    pub kind: RefsKindArg,
    /// Include line context around matches.
    #[serde(default)]
    pub include_context: bool,
    /// Override the number of context lines (default 3, max 20).
    #[serde(default)]
    pub context_lines: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RefsKindArg {
    Definition,
    Reference,
    Both,
}

impl Default for RefsKindArg {
    fn default() -> Self {
        RefsKindArg::Reference
    }
}

impl RefsKindArg {
    fn as_str(self) -> &'static str {
        match self {
            RefsKindArg::Definition => "definition",
            RefsKindArg::Reference => "reference",
            RefsKindArg::Both => "both",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum QueryShortcut {
    Def,
    Ref,
}

impl QueryShortcut {
    fn as_str(self) -> &'static str {
        match self {
            QueryShortcut::Def => "def",
            QueryShortcut::Ref => "ref",
        }
    }
}

fn parse_query_shortcut(query: &str) -> (Option<QueryShortcut>, &str) {
    let trimmed = query.trim();
    for (prefix, shortcut) in [("def:", QueryShortcut::Def), ("ref:", QueryShortcut::Ref)] {
        if trimmed.len() >= prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix) {
            return (Some(shortcut), trimmed[prefix.len()..].trim());
        }
    }
    (None, trimmed)
}

fn split_source_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect()
}

fn full_reindex_allowed() -> bool {
    std::env::var("SPARK_MCP_REINDEX_ALLOW_FULL")
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn is_local_scope(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower == "local" || lower.starts_with("local-")
}

fn normalize_scope_values(values: Option<Vec<String>>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in values.unwrap_or_default() {
        for part in split_source_values(&raw) {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            let canonical = trimmed.to_ascii_lowercase();
            if seen.insert(canonical.clone()) {
                out.push(canonical);
            }
        }
    }
    out
}

fn normalize_workspace_paths(values: Option<Vec<String>>) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in values.unwrap_or_default() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let path = PathBuf::from(trimmed);
        if path.is_absolute() {
            return Err(format!("workspace_paths must be relative (got {trimmed})"));
        }
        for component in path.components() {
            match component {
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(format!(
                        "workspace_paths must not contain '..' or absolute prefixes (got {trimmed})"
                    ));
                }
                Component::CurDir | Component::Normal(_) => {}
            }
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

fn map_reindex_error(err: ReindexError) -> crate::McpError {
    match err.kind {
        ReindexErrorKind::Scope | ReindexErrorKind::Busy => {
            crate::McpError::invalid_params(err.message, None)
        }
        ReindexErrorKind::Internal => crate::McpError::internal_error(err.message, None),
    }
}

fn resolve_source_filter(
    sources: Option<Vec<String>>,
    source: Option<String>,
    available: &[SourceSummary],
) -> Result<Option<String>, String> {
    let mut requested = Vec::new();
    if let Some(values) = sources {
        for value in values {
            requested.extend(split_source_values(&value));
        }
    }
    if requested.is_empty() {
        if let Some(value) = source {
            requested.extend(split_source_values(&value));
        }
    }
    if requested.is_empty() {
        return Ok(None);
    }

    let mut known = HashMap::new();
    let mut has_local = false;
    for summary in available {
        let lower = summary.source.to_ascii_lowercase();
        if lower == "local" || lower.starts_with("local-") {
            has_local = true;
        }
        known.entry(lower).or_insert_with(|| summary.source.clone());
    }

    let mut unknown = Vec::new();
    let mut seen = HashSet::new();
    let mut canonical = Vec::new();
    for value in requested {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower == "local" {
            if !has_local {
                unknown.push(trimmed.to_string());
                continue;
            }
            if seen.insert("local".to_string()) {
                canonical.push("local".to_string());
            }
            continue;
        }
        if let Some(actual) = known.get(&lower) {
            if seen.insert(actual.clone()) {
                canonical.push(actual.clone());
            }
        } else {
            unknown.push(trimmed.to_string());
        }
    }

    if !unknown.is_empty() {
        let mut known_sources: Vec<String> = known.values().cloned().collect();
        known_sources.sort();
        if has_local
            && !known_sources
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case("local"))
        {
            known_sources.insert(0, "local".to_string());
        }
        return Err(format!(
            "unknown sources: {}; known sources: {}",
            unknown.join(", "),
            known_sources.join(", ")
        ));
    }

    Ok(Some(canonical.join(",")))
}

fn split_file_label(input: &str) -> Option<(String, String)> {
    let normalized = input.trim().replace('\\', "/");
    let mut parts = normalized.splitn(2, '/');
    let first = parts.next()?.trim();
    let rest = parts.next()?.trim();
    if first.is_empty() || rest.is_empty() {
        return None;
    }
    Some((first.to_string(), rest.to_string()))
}

fn source_label_exists(label: &str, available: &[SourceSummary]) -> bool {
    available
        .iter()
        .any(|summary| summary.source.eq_ignore_ascii_case(label))
}

fn classify_hover_input_kind(file: &str, available: &[SourceSummary]) -> &'static str {
    if Path::new(file).is_absolute() {
        return "absolute";
    }
    if let Some((label, _rest)) = split_file_label(file) {
        if source_label_exists(&label, available)
            || label.eq_ignore_ascii_case("local")
            || label.contains('-')
        {
            return "doc_id";
        }
    }
    "relative"
}

fn classify_hover_none_reason(
    file: &str,
    available: &[SourceSummary],
) -> (&'static str, &'static str) {
    if let Some((label, _rest)) = split_file_label(file) {
        if !label.eq_ignore_ascii_case("local")
            && !source_label_exists(&label, available)
            && label.contains('-')
        {
            return (
                "unmapped_source",
                "The source label is not configured; run spark.list_sources and use one of the returned labels.",
            );
        }
    }
    (
        "not_found",
        "The file was not found under indexed corpus mounts; verify the path and refresh local indexing if needed.",
    )
}

fn classify_hover_invalid_doc_reason(message: &str) -> (&'static str, &'static str) {
    if message.contains("outside indexed corpus mounts") {
        return (
            "outside_root",
            "The path resolves outside configured corpus roots. Use a doc_id or a filesystem path under an indexed mount.",
        );
    }
    (
        "invalid_input",
        "The file input is invalid for hover lookup; confirm path format and corpus mount boundaries.",
    )
}

fn suggest_hover_doc_id(file: &str, available: &[SourceSummary]) -> Option<String> {
    if let Some((label, rest)) = split_file_label(file) {
        if source_label_exists(&label, available) {
            return Some(format!("{label}/{rest}"));
        }
        for summary in available {
            if let Some(alias) = summary.source.strip_prefix("local-") {
                if label.eq_ignore_ascii_case(alias) {
                    return Some(format!("{}/{}", summary.source, rest));
                }
            }
        }
    }

    let path = Path::new(file);
    if path.is_absolute() {
        let parts: Vec<String> = path
            .components()
            .filter_map(|component| {
                let value = component.as_os_str().to_string_lossy();
                if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                }
            })
            .collect();
        for summary in available {
            if let Some(alias) = summary.source.strip_prefix("local-") {
                if let Some(pos) = parts
                    .iter()
                    .position(|part| part.eq_ignore_ascii_case(alias))
                {
                    if pos + 1 >= parts.len() {
                        continue;
                    }
                    let rest = parts[(pos + 1)..].join("/");
                    if !rest.is_empty() {
                        return Some(format!("{}/{}", summary.source, rest));
                    }
                }
            }
        }
    }

    None
}

fn is_identifier_like_query(query: &str) -> bool {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return false;
    }
    let has_alpha = trimmed.chars().any(|ch| ch.is_ascii_alphabetic());
    let allowed = trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.');
    has_alpha && allowed
}

fn title_case_identifier(value: &str) -> String {
    value
        .split('.')
        .map(|segment| {
            segment
                .split('_')
                .map(|part| {
                    let mut chars = part.chars();
                    let Some(first) = chars.next() else {
                        return String::new();
                    };
                    let mut out = String::new();
                    out.push(first.to_ascii_uppercase());
                    for ch in chars {
                        out.push(ch.to_ascii_lowercase());
                    }
                    out
                })
                .collect::<Vec<String>>()
                .join("_")
        })
        .collect::<Vec<String>>()
        .join(".")
}

fn identifier_case_variants(query: &str) -> Vec<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut variants = vec![trimmed.to_string()];
    let title_case = title_case_identifier(trimmed);
    if !title_case.is_empty() && !variants.iter().any(|item| item == &title_case) {
        variants.push(title_case);
    }
    variants
}

fn query_tokens_for_phrase_workflow(query: &str) -> Vec<String> {
    query
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn literal_multi_token_variants(query: &str) -> Vec<String> {
    let tokens = query_tokens_for_phrase_workflow(query);
    if tokens.len() < 2 {
        return Vec::new();
    }

    let mut variants = Vec::new();
    let mut seen = HashSet::new();
    for candidate in [
        tokens.join("_"),
        tokens.join("-"),
        tokens.join("."),
        tokens.join("::"),
    ] {
        if candidate.is_empty() {
            continue;
        }
        let key = candidate.to_ascii_lowercase();
        if seen.insert(key) {
            variants.push(candidate);
        }
    }
    variants
}

fn symbol_occurrences_to_search_hits(
    occurrences: &[SymbolOccurrence],
    include_context: bool,
) -> Vec<SearchHit> {
    occurrences
        .iter()
        .enumerate()
        .map(|(index, item)| SearchHit {
            doc_id: item.doc_id.clone(),
            path: item.path.clone(),
            title: Path::new(&item.path)
                .file_stem()
                .map(|stem| stem.to_string_lossy().to_string()),
            source: item.source.clone(),
            score: 1.0_f32 - (index as f32 * 0.001),
            snippet: item.excerpt.clone(),
            chunk_index: 0,
            context: if include_context {
                Some(LineContext {
                    line_start: item.line,
                    line_end: item.line_end,
                    context_start: item.context_start,
                    context_end: item.context_end,
                    lines: item.context.clone(),
                })
            } else {
                None
            },
            provenance: item.provenance.clone(),
            symbol: Some(SymbolMatch {
                symbol: item.symbol.clone(),
                kind: item.kind.clone(),
            }),
        })
        .collect()
}

#[tool_router(router = tool_router_spark, vis = "pub")]
impl SparkMcp {
    /// Ask the configured LLM provider (adapter surface; retrieval-only by default).
    #[tool(
        name = "spark.llm_answer",
        description = "LLM adapter endpoint for grounded answers (provider-agnostic; returns not configured until a provider runtime is enabled)."
    )]
    async fn spark_llm_answer(
        &self,
        Parameters(args): Parameters<LlmAnswerArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let question = args.question.trim();
        if question.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "ok": false,
                "error": "question must not be empty"
            })));
        }

        let provider = args.provider.as_provider();
        let runtime = LlmRuntime::new();
        let request = LlmAnswerRequest {
            question: question.to_string(),
            provider,
        };
        match runtime.answer(request).await {
            Ok(result) => {
                let value = serde_json::to_value(&result)
                    .map_err(|err| crate::McpError::internal_error(err.to_string(), None))?;
                Ok(CallToolResult::structured(value))
            }
            Err(err) => Ok(CallToolResult::structured(json!({
                "ok": false,
                "provider": provider.as_str(),
                "error": err.to_string(),
            }))),
        }
    }

    /// Search the local SPARK corpus and return relevant snippets with citations.
    ///
    /// # Security
    /// * **Information Retrieval**: Only returns data from documents within the verified corpus.
    #[tool(
        name = "spark.search",
        description = "Search local SPARK corpus with citations (mode: auto|lexical|semantic|hybrid)."
    )]
    async fn spark_search(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let query = args.query.trim();
        if query.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "error": "query must not be empty"
            })));
        }
        let limit = args.limit.map(|v| v as usize);
        let available_sources = self.search.list_sources();
        let source_filter = resolve_source_filter(
            args.sources.clone(),
            args.source.clone(),
            &available_sources,
        )
        .map_err(|message| crate::McpError::invalid_params(message, None))?;
        let semantic_available = self.search.semantic_available();
        let requested_mode = args.mode;
        let effective_mode = match requested_mode {
            SearchModeArg::Auto => {
                if semantic_available {
                    SearchMode::Hybrid
                } else {
                    SearchMode::Lexical
                }
            }
            SearchModeArg::Lexical => SearchMode::Lexical,
            SearchModeArg::Semantic => {
                if semantic_available {
                    SearchMode::Semantic
                } else {
                    SearchMode::Lexical
                }
            }
            SearchModeArg::Hybrid => {
                if semantic_available {
                    SearchMode::Hybrid
                } else {
                    SearchMode::Lexical
                }
            }
        };
        let query_kind = match args.query_kind {
            SearchQueryKindArg::Literal => LexicalQueryKind::Literal,
            SearchQueryKindArg::Tantivy => LexicalQueryKind::Tantivy,
        };

        let (shortcut, shortcut_query) = parse_query_shortcut(query);
        if let Some(shortcut) = shortcut {
            let symbol = shortcut_query.trim();
            if symbol.is_empty() {
                return Ok(CallToolResult::structured(json!({
                    "error": "query must include a symbol after the shortcut prefix"
                })));
            }
            let symbol_limit = limit.unwrap_or(match shortcut {
                QueryShortcut::Def => 25,
                QueryShortcut::Ref => 50,
            });
            let results = match shortcut {
                QueryShortcut::Def => self.search.locate_symbol(
                    symbol,
                    symbol_limit,
                    source_filter.as_deref(),
                    SymbolMatchKind::Definition,
                    args.include_context,
                    args.context_lines.map(|value| value as usize),
                ),
                QueryShortcut::Ref => self.search.refs_symbol(
                    symbol,
                    symbol_limit,
                    source_filter.as_deref(),
                    args.include_context,
                    args.context_lines.map(|value| value as usize),
                ),
            }
            .map_err(|err| match err {
                SearchError::InvalidDocId(message) => {
                    crate::McpError::invalid_params(message, None)
                }
                _ => crate::McpError::internal_error(err.to_string(), None),
            })?;

            return Ok(CallToolResult::structured(json!({
                "query": query,
                "query_kind": format!("{:?}", args.query_kind).to_lowercase(),
                "shortcut": shortcut.as_str(),
                "mode": format!("{:?}", effective_mode).to_lowercase(),
                "semantic_available": semantic_available,
                "source_filter": source_filter,
                "results": results,
                "result_kind": "symbol",
            })));
        }

        let mut outcome = self
            .search
            .search(
                query,
                limit,
                source_filter.as_deref(),
                effective_mode,
                query_kind,
                args.include_context,
                args.context_lines.map(|v| v as usize),
            )
            .map_err(|err| match err {
                SearchError::Query(parse_err) => crate::McpError::invalid_params(
                    format!(
                        "lexical query parse error: {parse_err}\n\nTip: set query_kind=\"literal\" for code fragments/signatures."
                    ),
                    None,
                ),
                _ => crate::McpError::internal_error(err.to_string(), None),
            })?;

        let query_tokens = query_tokens_for_phrase_workflow(query);
        let multi_token_query = query_tokens.len() >= 2;
        let identifier_query = is_identifier_like_query(query);
        let mut literal_multi_token_fallback_applied = false;
        let mut literal_multi_token_fallback_mode: Option<String> = None;
        let mut literal_multi_token_variant_used: Option<String> = None;
        let mut literal_multi_token_variants_considered = Vec::new();

        if matches!(query_kind, LexicalQueryKind::Literal)
            && multi_token_query
            && !identifier_query
            && outcome.hits.is_empty()
        {
            for candidate in literal_multi_token_variants(query) {
                if candidate.eq_ignore_ascii_case(query) {
                    continue;
                }
                literal_multi_token_variants_considered.push(candidate.clone());
                let candidate_outcome = self
                    .search
                    .search(
                        &candidate,
                        limit,
                        source_filter.as_deref(),
                        effective_mode,
                        query_kind,
                        args.include_context,
                        args.context_lines.map(|value| value as usize),
                    )
                    .map_err(|err| match err {
                        SearchError::Query(parse_err) => crate::McpError::invalid_params(
                            format!(
                                "lexical query parse error: {parse_err}\n\nTip: set query_kind=\"literal\" for code fragments/signatures."
                            ),
                            None,
                        ),
                        _ => crate::McpError::internal_error(err.to_string(), None),
                    })?;
                if !candidate_outcome.hits.is_empty() {
                    literal_multi_token_fallback_applied = true;
                    literal_multi_token_fallback_mode = Some("lexical_variant".to_string());
                    literal_multi_token_variant_used = Some(candidate);
                    outcome = candidate_outcome;
                    break;
                }
            }

            if outcome.hits.is_empty() {
                let locate_limit = limit.unwrap_or(25);
                let mut seen_locate_variants = HashSet::new();
                for candidate in literal_multi_token_variants(query) {
                    for locate_variant in identifier_case_variants(&candidate) {
                        if !seen_locate_variants.insert(locate_variant.clone()) {
                            continue;
                        }
                        if !literal_multi_token_variants_considered
                            .iter()
                            .any(|entry| entry == &locate_variant)
                        {
                            literal_multi_token_variants_considered.push(locate_variant.clone());
                        }
                        let locate_results = self
                            .search
                            .locate_symbol(
                                &locate_variant,
                                locate_limit,
                                source_filter.as_deref(),
                                SymbolMatchKind::Any,
                                args.include_context,
                                args.context_lines.map(|value| value as usize),
                            )
                            .map_err(|err| match err {
                                SearchError::InvalidDocId(message) => {
                                    crate::McpError::invalid_params(message, None)
                                }
                                _ => crate::McpError::internal_error(err.to_string(), None),
                            })?;
                        if !locate_results.is_empty() {
                            literal_multi_token_fallback_applied = true;
                            literal_multi_token_fallback_mode = Some("locate_variant".to_string());
                            literal_multi_token_variant_used = Some(locate_variant);
                            outcome.hits = symbol_occurrences_to_search_hits(
                                &locate_results,
                                args.include_context,
                            );
                            break;
                        }
                    }
                    if !outcome.hits.is_empty() {
                        break;
                    }
                }
            }

            if outcome.hits.is_empty() {
                let locate_limit = limit.unwrap_or(25);
                let mut seen_identifier_tokens = HashSet::new();
                for raw_token in query.split_whitespace() {
                    let token = raw_token.trim_matches(|ch: char| {
                        !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.')
                    });
                    if token.is_empty() || !is_identifier_like_query(token) {
                        continue;
                    }
                    if !seen_identifier_tokens.insert(token.to_ascii_lowercase()) {
                        continue;
                    }
                    for candidate in identifier_case_variants(token) {
                        if !literal_multi_token_variants_considered
                            .iter()
                            .any(|entry| entry == &candidate)
                        {
                            literal_multi_token_variants_considered.push(candidate.clone());
                        }
                        let locate_results = self
                            .search
                            .locate_symbol(
                                &candidate,
                                locate_limit,
                                source_filter.as_deref(),
                                SymbolMatchKind::Any,
                                args.include_context,
                                args.context_lines.map(|value| value as usize),
                            )
                            .map_err(|err| match err {
                                SearchError::InvalidDocId(message) => {
                                    crate::McpError::invalid_params(message, None)
                                }
                                _ => crate::McpError::internal_error(err.to_string(), None),
                            })?;
                        if !locate_results.is_empty() {
                            literal_multi_token_fallback_applied = true;
                            literal_multi_token_fallback_mode = Some("locate_token".to_string());
                            literal_multi_token_variant_used = Some(candidate);
                            outcome.hits = symbol_occurrences_to_search_hits(
                                &locate_results,
                                args.include_context,
                            );
                            break;
                        }
                    }
                    if !outcome.hits.is_empty() {
                        break;
                    }
                }
            }
        }

        let mut search_locate_parity = json!({
            "identifier_query": false,
        });
        if matches!(query_kind, LexicalQueryKind::Literal) && identifier_query {
            let locate_limit = limit.unwrap_or(25);
            let mut locate_query_used = query.to_string();
            let mut locate_results = Vec::new();
            for candidate in identifier_case_variants(query) {
                let results = self
                    .search
                    .locate_symbol(
                        &candidate,
                        locate_limit,
                        source_filter.as_deref(),
                        SymbolMatchKind::Any,
                        args.include_context,
                        args.context_lines.map(|value| value as usize),
                    )
                    .map_err(|err| match err {
                        SearchError::InvalidDocId(message) => {
                            crate::McpError::invalid_params(message, None)
                        }
                        _ => crate::McpError::internal_error(err.to_string(), None),
                    })?;
                if !results.is_empty() {
                    locate_query_used = candidate;
                    locate_results = results;
                    break;
                }
            }

            let mut fallback_applied = false;
            if outcome.hits.is_empty() && !locate_results.is_empty() {
                outcome.hits =
                    symbol_occurrences_to_search_hits(&locate_results, args.include_context);
                fallback_applied = true;
            }

            let search_doc_ids: HashSet<String> =
                outcome.hits.iter().map(|hit| hit.doc_id.clone()).collect();
            let locate_doc_ids: HashSet<String> = locate_results
                .iter()
                .map(|entry| entry.doc_id.clone())
                .collect();
            let shared_doc_ids = search_doc_ids.intersection(&locate_doc_ids).count();
            search_locate_parity = json!({
                "identifier_query": true,
                "locate_query_used": locate_query_used,
                "locate_matches": locate_results.len(),
                "search_matches": outcome.hits.len(),
                "shared_doc_ids": shared_doc_ids,
                "fallback_applied": fallback_applied,
                "guidance": if fallback_applied {
                    Some("literal identifier search returned no lexical hits; response used locate-derived fallback to preserve local parity")
                } else {
                    None
                },
            });
        }

        let warning = if matches!(
            requested_mode,
            SearchModeArg::Semantic | SearchModeArg::Hybrid
        ) && !semantic_available
        {
            Some("semantic search not enabled; falling back to lexical search")
        } else {
            None
        };

        let no_results_guidance = if outcome.hits.is_empty() {
            Some(match query_kind {
                LexicalQueryKind::Literal if multi_token_query => {
                    "No hits for this literal multi-token query. Try query_kind=\"tantivy\" for parser syntax, or code-form variants like policy_kernel / policy::kernel."
                }
                LexicalQueryKind::Literal => {
                    "No hits for this literal query. Verify source_filter or try query_kind=\"tantivy\" for explicit query parser syntax."
                }
                LexicalQueryKind::Tantivy => {
                    "No hits for this Tantivy query. Verify parser syntax or retry with query_kind=\"literal\" for safe tokenized matching."
                }
            })
        } else {
            None
        };

        let query_behavior = json!({
            "identifier_query": identifier_query,
            "multi_token_query": multi_token_query,
            "token_count": query_tokens.len(),
            "phrase_strategy": match query_kind {
                LexicalQueryKind::Literal => "tokenized_disjunction_with_code_join_fallback",
                LexicalQueryKind::Tantivy => "tantivy_query_parser_syntax",
            },
            "literal_variant_fallback": if matches!(query_kind, LexicalQueryKind::Literal) {
                Some(json!({
                    "applied": literal_multi_token_fallback_applied,
                    "mode": literal_multi_token_fallback_mode,
                    "variant_used": literal_multi_token_variant_used,
                    "variants_considered": literal_multi_token_variants_considered,
                }))
            } else {
                None
            },
        });

        Ok(CallToolResult::structured(json!({
            "query": query,
            "query_kind": format!("{:?}", args.query_kind).to_lowercase(),
            "mode": format!("{:?}", effective_mode).to_lowercase(),
            "semantic_available": semantic_available,
            "source_filter": source_filter,
            "warning": warning,
            "query_behavior": query_behavior,
            "no_results_guidance": no_results_guidance,
            "results": outcome.hits,
            "related_defs": outcome.related_defs,
            "search_locate_parity": search_locate_parity,
        })))
    }

    /// List available corpus sources and basic counts.
    #[tool(
        name = "spark.list_sources",
        description = "List corpus sources and file counts."
    )]
    async fn spark_list_sources(
        &self,
        Parameters(_): Parameters<ListSourcesArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let sources = self.search.list_sources();
        Ok(CallToolResult::structured(json!({
            "sources": sources,
        })))
    }

    /// Return index freshness metadata and corpus counts.
    #[tool(
        name = "spark.index_status",
        description = "Report index metadata and corpus counts."
    )]
    async fn spark_index_status(
        &self,
        Parameters(_): Parameters<IndexStatusArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let meta = self.search.index_metadata();
        let sources = self.search.list_sources();
        let local_freshness = self
            .search
            .local_freshness_report()
            .map_err(|err| crate::McpError::internal_error(err.to_string(), None))?;
        let local_any_stale = local_freshness.any_stale;
        let hover_telemetry = self.hover_telemetry_snapshot();
        Ok(CallToolResult::structured(json!({
            "index": meta,
            "sources": sources,
            "local_freshness": local_freshness,
            "hover_telemetry": {
                "input_kinds": hover_telemetry.input_kind_counts,
                "failure_reasons": hover_telemetry.failure_reason_counts,
            },
            "refresh": {
                "mode": "in_process_reindex",
                "supports_scoped_in_process_refresh": true,
                "reason_required": true,
                "default_scope": "local",
                "status": if local_any_stale { "stale" } else { "fresh" },
                "next_action": if local_any_stale { "run_in_process_reindex" } else { "none" },
                "tool": "spark.reindex",
                "reason_contract": {
                    "required": true,
                    "transport": "tool_reason",
                    "example": "local-spark stale after edits",
                    "description": "pass a short reason to spark.reindex.reason"
                },
                "commands": [
                    "spark.reindex {\"reason\":\"<reason>\",\"sources\":[\"local\"]}"
                ],
                "post_refresh_verify": [
                    "spark.index_status -> local_freshness.any_stale == false",
                    "spark.search source=local-spark include_context=true for edited symbol/file",
                    "spark.hover on edited file position resolves expected symbol"
                ],
                "guidance": "Prefer spark.reindex for in-process refresh; use restart-driven SPARK_MCP_REINDEX=1 only when operating outside MCP tool context."
            },
        })))
    }

    /// Trigger an audited lexical reindex without restarting the service.
    #[tool(
        name = "spark.reindex",
        description = "Trigger an audited lexical reindex (local-only by default, single-flight)."
    )]
    async fn spark_reindex(
        &self,
        Parameters(args): Parameters<ReindexArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let reason = args.reason.trim();
        if reason.is_empty() {
            return Err(crate::McpError::invalid_params(
                "reason must not be empty",
                None,
            ));
        }

        let mut requested_sources = normalize_scope_values(args.sources);
        if args.full_reindex {
            if !full_reindex_allowed() {
                return Err(crate::McpError::invalid_params(
                    "full_reindex requires SPARK_MCP_REINDEX_ALLOW_FULL=1",
                    None,
                ));
            }
            if requested_sources.is_empty() {
                requested_sources = self
                    .search
                    .list_sources()
                    .into_iter()
                    .map(|source| source.source)
                    .filter(|source| !source.trim().is_empty())
                    .collect();
            }
        } else {
            if requested_sources.is_empty() {
                requested_sources.push("local".to_string());
            }
            if let Some(non_local) = requested_sources
                .iter()
                .find(|value| !is_local_scope(value))
            {
                return Err(crate::McpError::invalid_params(
                    format!(
                        "source '{non_local}' is not allowed in scoped mode; set full_reindex=true with SPARK_MCP_REINDEX_ALLOW_FULL=1 for broad scope"
                    ),
                    None,
                ));
            }
        }

        let workspace_paths = normalize_workspace_paths(args.workspace_paths)
            .map_err(|message| crate::McpError::invalid_params(message, None))?;
        let request = ReindexRequest {
            sources: requested_sources,
            workspace_paths,
            reason: reason.to_string(),
        };
        let report = self
            .reindexer
            .force_and_wait(request)
            .await
            .map_err(map_reindex_error)?;

        Ok(CallToolResult::structured(json!({
            "status": "ok",
            "reindex": report,
            "index": self.search.index_metadata(),
            "sources": self.search.list_sources(),
        })))
    }

    /// Fetch a document from the corpus by doc_id.
    ///
    /// # Security
    /// * **Path Isolation**: Validates that `doc_id` is within configured corpus roots.
    #[tool(
        name = "spark.get_doc",
        description = "Fetch a corpus document by doc_id."
    )]
    async fn spark_get_doc(
        &self,
        Parameters(args): Parameters<GetDocArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let doc_id = args.doc_id.trim();
        if doc_id.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "error": "doc_id must not be empty"
            })));
        }
        let max_chars = args.max_chars.map(|v| v as usize);
        let doc = self
            .search
            .get_doc(doc_id, max_chars)
            .map_err(|err| match err {
                SearchError::InvalidDocId(message) => {
                    crate::McpError::invalid_params(message, None)
                }
                _ => crate::McpError::internal_error(err.to_string(), None),
            })?;

        match doc {
            Some(doc) => Ok(CallToolResult::structured(json!({ "doc": doc }))),
            None => Ok(CallToolResult::structured(json!({
                "error": "document not found",
                "doc_id": doc_id,
            }))),
        }
    }

    /// Fetch a chunk from the corpus by doc_id + chunk_index.
    #[tool(
        name = "spark.get_chunk",
        description = "Fetch a corpus chunk by doc_id + chunk_index."
    )]
    async fn spark_get_chunk(
        &self,
        Parameters(args): Parameters<GetChunkArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let doc_id = args.doc_id.trim();
        if doc_id.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "error": "doc_id must not be empty"
            })));
        }

        let chunk = self
            .search
            .get_chunk(doc_id, args.chunk_index)
            .map_err(|err| match err {
                SearchError::InvalidDocId(message) => {
                    crate::McpError::invalid_params(message, None)
                }
                _ => crate::McpError::internal_error(err.to_string(), None),
            })?;

        match chunk {
            Some(chunk) => Ok(CallToolResult::structured(json!({ "chunk": chunk }))),
            None => Ok(CallToolResult::structured(json!({
                "error": "chunk not found",
                "doc_id": doc_id,
                "chunk_index": args.chunk_index,
            }))),
        }
    }

    /// Resolve lexical hover information at a file position.
    #[tool(
        name = "spark.hover",
        description = "Resolve lexical hover at file:line:column (symbol + snippet + context)."
    )]
    async fn spark_hover(
        &self,
        Parameters(args): Parameters<HoverArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let file = args.file.trim();
        if file.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "error": "file must not be empty"
            })));
        }

        let line = args.line.max(1);
        let column = args.column.max(1);
        let symbol = args
            .symbol
            .as_ref()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let include_context = args.include_context.unwrap_or(true);
        let available_sources = self.search.list_sources();
        let input_kind = classify_hover_input_kind(file, &available_sources);
        self.record_hover_input_kind(input_kind);

        let hover = match self.search.hover(
            file,
            line,
            column,
            symbol.as_deref(),
            include_context,
            args.context_lines.map(|value| value as usize),
        ) {
            Ok(hover) => hover,
            Err(SearchError::InvalidDocId(message)) => {
                let (reason, hint) = classify_hover_invalid_doc_reason(&message);
                self.record_hover_failure_reason(reason);
                return Ok(CallToolResult::structured(json!({
                    "error": "hover lookup failed",
                    "reason": reason,
                    "hint": hint,
                    "input_kind": input_kind,
                    "suggested_doc_id": suggest_hover_doc_id(file, &available_sources),
                    "file": file,
                    "line": line,
                    "column": column,
                    "symbol": symbol,
                })));
            }
            Err(err) => {
                return Err(crate::McpError::internal_error(err.to_string(), None));
            }
        };

        match hover {
            Some(hover) => Ok(CallToolResult::structured(json!({
                "file": file,
                "line": line,
                "column": column,
                "symbol": symbol,
                "include_context": include_context,
                "context_lines": args.context_lines,
                "hover": hover,
            }))),
            None => {
                let (reason, hint) = classify_hover_none_reason(file, &available_sources);
                self.record_hover_failure_reason(reason);
                Ok(CallToolResult::structured(json!({
                    "error": "hover lookup failed",
                    "reason": reason,
                    "hint": hint,
                    "input_kind": input_kind,
                    "suggested_doc_id": suggest_hover_doc_id(file, &available_sources),
                    "file": file,
                    "line": line,
                    "column": column,
                    "symbol": symbol,
                })))
            }
        }
    }

    /// Locate symbol definitions (and optionally references) in the corpus.
    #[tool(
        name = "spark_locate",
        description = "Locate symbol definitions (definition or reference)."
    )]
    async fn spark_locate(
        &self,
        Parameters(args): Parameters<LocateArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let symbol = args.symbol.trim();
        if symbol.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "error": "symbol must not be empty"
            })));
        }

        let limit = args.limit.map(|value| value as usize).unwrap_or(25);
        let kind = match args.kind {
            LocateKindArg::Definition => SymbolMatchKind::Definition,
            LocateKindArg::Reference => SymbolMatchKind::Reference,
            LocateKindArg::Both => SymbolMatchKind::Any,
        };

        let matches = self
            .search
            .locate_symbol(
                symbol,
                limit,
                args.source.as_deref(),
                kind,
                args.include_context,
                args.context_lines.map(|value| value as usize),
            )
            .map_err(|err| match err {
                SearchError::InvalidDocId(message) => {
                    crate::McpError::invalid_params(message, None)
                }
                _ => crate::McpError::internal_error(err.to_string(), None),
            })?;

        Ok(CallToolResult::structured(json!({
            "symbol": symbol,
            "kind": args.kind.as_str(),
            "source_filter": args.source,
            "results": matches,
        })))
    }

    /// Find reference sites for a symbol in the corpus.
    #[tool(
        name = "spark_refs",
        description = "Find symbol references (usage sites)."
    )]
    async fn spark_refs(
        &self,
        Parameters(args): Parameters<RefsArgs>,
    ) -> Result<CallToolResult, crate::McpError> {
        let symbol = args.symbol.trim();
        if symbol.is_empty() {
            return Ok(CallToolResult::structured(json!({
                "error": "symbol must not be empty"
            })));
        }

        let limit = args.limit.map(|value| value as usize).unwrap_or(50);
        let matches = match args.kind {
            RefsKindArg::Reference => self.search.refs_symbol(
                symbol,
                limit,
                args.source.as_deref(),
                args.include_context,
                args.context_lines.map(|value| value as usize),
            ),
            RefsKindArg::Definition => self.search.locate_symbol(
                symbol,
                limit,
                args.source.as_deref(),
                SymbolMatchKind::Definition,
                args.include_context,
                args.context_lines.map(|value| value as usize),
            ),
            RefsKindArg::Both => self.search.locate_symbol(
                symbol,
                limit,
                args.source.as_deref(),
                SymbolMatchKind::Any,
                args.include_context,
                args.context_lines.map(|value| value as usize),
            ),
        }
        .map_err(|err| match err {
            SearchError::InvalidDocId(message) => crate::McpError::invalid_params(message, None),
            _ => crate::McpError::internal_error(err.to_string(), None),
        })?;

        Ok(CallToolResult::structured(json!({
            "symbol": symbol,
            "kind": args.kind.as_str(),
            "source_filter": args.source,
            "results": matches,
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HoverArgs, IndexStatusArgs, QueryShortcut, ReindexArgs, SearchArgs, SearchModeArg,
        SearchQueryKindArg, parse_query_shortcut, resolve_source_filter,
    };
    use crate::auto_reindex::AutoReindexer;
    use crate::config::ResumeMode;
    use crate::search::{
        CorpusMountConfig, HnswConfig, SearchConfig, SearchIndex, SemanticBackend, SemanticConfig,
        SourceSummary,
    };
    use crate::server::SparkMcp;
    use mcp_toolkit_docs::ChunkConfig;
    use mcp_toolkit_http::session::BoundedSessionManager;
    use mcp_toolkit_testing::assert_tool_schema_snapshot;
    use rmcp::handler::server::wrapper::Parameters;
    use rmcp::model::CallToolResult;
    use rmcp::transport::streamable_http_server::session::local::{
        LocalSessionManager, SessionConfig,
    };
    use serde_json::Value;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut path = std::env::temp_dir();
        path.push(format!("{prefix}-{}-{}", std::process::id(), nanos));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn test_config(semantic_dir: &Path) -> SearchConfig {
        SearchConfig {
            chunk_config: ChunkConfig {
                max_chars: 256,
                overlap: 32,
            },
            max_file_bytes: 1_000_000,
            default_limit: 5,
            max_limit: 20,
            snippet_max_chars: 120,
            semantic: SemanticConfig {
                enabled: false,
                backend: SemanticBackend::Flat,
                model: "all-minilm-l6-v2".to_string(),
                index_dir: semantic_dir.to_path_buf(),
                cache_dir: None,
                build_on_start: false,
                batch_size: 32,
                top_k: 10,
                min_score: 0.0,
                weight: 0.0,
                hnsw: HnswConfig {
                    m: 16,
                    ef_construction: 64,
                    ef_search: 32,
                },
            },
        }
    }

    struct HoverToolHarness {
        workspace_root: PathBuf,
        corpus_dir: PathBuf,
        local_mount: PathBuf,
        index_dir: PathBuf,
        semantic_dir: PathBuf,
        server: SparkMcp,
    }

    impl HoverToolHarness {
        fn new() -> Self {
            let workspace_root = temp_dir("spark-mcp-tools-workspace");
            let corpus_dir = workspace_root.join("corpus");
            let local_mount = workspace_root.join("spark");
            let index_dir = temp_dir("spark-mcp-tools-index");
            let semantic_dir = temp_dir("spark-mcp-tools-semantic");

            fs::create_dir_all(corpus_dir.join("seed")).expect("create corpus");
            fs::create_dir_all(local_mount.join("src")).expect("create local mount");
            fs::write(corpus_dir.join("seed/readme.md"), "seed").expect("write seed doc");
            fs::write(
                local_mount.join("src/policy_gateway.ads"),
                "procedure Gateway_Decision is\nprocedure Policy_Kernel is\n-- policy_kernel hardening path\n-- policy::kernel fallback probe\n-- refresh workflow notes",
            )
            .expect("write local file");
            fs::write(
                local_mount.join("src/policy,gateway.ads"),
                "procedure Gateway_Comma_Path is",
            )
            .expect("write comma path local file");

            let search = SearchIndex::open_or_create(
                &corpus_dir,
                &index_dir,
                test_config(&semantic_dir),
                false,
                vec![CorpusMountConfig::new("local-spark", local_mount.clone())],
            )
            .expect("index");
            let session_manager = Arc::new(BoundedSessionManager::new(
                LocalSessionManager::default(),
                8,
                false,
                {
                    let mut config = SessionConfig::default();
                    config.channel_capacity = 16;
                    config.keep_alive = Some(Duration::from_secs(30));
                    config
                },
            ));
            let search = Arc::new(search);
            let reindexer = AutoReindexer::new(search.clone(), Duration::from_millis(1));
            let server = SparkMcp::new(search, reindexer, session_manager, ResumeMode::Off);
            Self {
                workspace_root,
                corpus_dir,
                local_mount,
                index_dir,
                semantic_dir,
                server,
            }
        }

        fn local_file(&self) -> PathBuf {
            self.local_mount.join("src/policy_gateway.ads")
        }

        fn mutate_local_file(&self) {
            thread::sleep(Duration::from_millis(15));
            fs::write(
                self.local_file(),
                "procedure Gateway_Decision is\nprocedure Gateway_Decision_Updated is",
            )
            .expect("mutate local file");
        }

        fn workspace_relative_path(&self, path_under_local_mount: &str) -> String {
            format!("spark/{path_under_local_mount}")
        }
    }

    impl Drop for HoverToolHarness {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.workspace_root);
            let _ = fs::remove_dir_all(&self.corpus_dir);
            let _ = fs::remove_dir_all(&self.local_mount);
            let _ = fs::remove_dir_all(&self.index_dir);
            let _ = fs::remove_dir_all(&self.semantic_dir);
        }
    }

    fn extract_hover_payload(result: CallToolResult) -> Value {
        let structured = result.structured_content.expect("structured payload");
        structured
            .get("hover")
            .cloned()
            .expect("hover payload present")
    }

    fn extract_structured(result: CallToolResult) -> Value {
        result.structured_content.expect("structured payload")
    }

    fn search_args(query: &str) -> SearchArgs {
        SearchArgs {
            query: query.to_string(),
            query_kind: SearchQueryKindArg::Literal,
            limit: Some(20),
            source: Some("local-spark".to_string()),
            sources: None,
            mode: SearchModeArg::Lexical,
            include_context: true,
            context_lines: Some(2),
        }
    }

    fn search_args_with_kind(query: &str, query_kind: SearchQueryKindArg) -> SearchArgs {
        SearchArgs {
            query: query.to_string(),
            query_kind,
            limit: Some(20),
            source: Some("local-spark".to_string()),
            sources: None,
            mode: SearchModeArg::Lexical,
            include_context: true,
            context_lines: Some(2),
        }
    }

    #[test]
    fn tool_schema_snapshot_contract_is_stable() {
        let tools = SparkMcp::tool_router_spark().list_all();
        let snapshot_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("spec/tool_schema_snapshot.v1.json");
        assert_tool_schema_snapshot(snapshot_path, &tools);
    }

    #[test]
    fn parse_query_shortcut_detects_def_and_ref_prefixes() {
        let (shortcut, query) = parse_query_shortcut("def:Parse_Input");
        assert!(matches!(shortcut, Some(QueryShortcut::Def)));
        assert_eq!(query, "Parse_Input");

        let (shortcut, query) = parse_query_shortcut("ref: Parse_Input ");
        assert!(matches!(shortcut, Some(QueryShortcut::Ref)));
        assert_eq!(query, "Parse_Input");

        let (shortcut, query) = parse_query_shortcut("plain query");
        assert!(shortcut.is_none());
        assert_eq!(query, "plain query");
    }

    #[test]
    fn resolve_source_filter_supports_alias_and_sources_override() {
        let available = vec![
            SourceSummary {
                source: "manual".to_string(),
                file_count: 1,
                total_bytes: 1,
            },
            SourceSummary {
                source: "local-spark".to_string(),
                file_count: 1,
                total_bytes: 1,
            },
        ];

        let filter = resolve_source_filter(
            Some(vec!["manual".to_string(), "local".to_string()]),
            Some("spec".to_string()),
            &available,
        )
        .expect("source filter");

        assert_eq!(filter.as_deref(), Some("manual,local"));
    }

    #[test]
    fn resolve_source_filter_rejects_unknown_source_labels() {
        let available = vec![SourceSummary {
            source: "manual".to_string(),
            file_count: 1,
            total_bytes: 1,
        }];
        let error = resolve_source_filter(None, Some("unknown".to_string()), &available)
            .expect_err("unknown source should fail");
        assert!(error.contains("unknown sources"));
    }

    #[tokio::test]
    async fn spark_hover_absolute_and_doc_id_inputs_have_parity() {
        let harness = HoverToolHarness::new();
        let absolute_file = harness.local_file().to_string_lossy().to_string();
        let absolute = harness
            .server
            .spark_hover(Parameters(HoverArgs {
                file: absolute_file,
                line: 1,
                column: 12,
                symbol: None,
                include_context: Some(true),
                context_lines: Some(3),
            }))
            .await
            .expect("absolute hover");
        let doc_id = harness
            .server
            .spark_hover(Parameters(HoverArgs {
                file: "local-spark/src/policy_gateway.ads".to_string(),
                line: 1,
                column: 12,
                symbol: None,
                include_context: Some(true),
                context_lines: Some(3),
            }))
            .await
            .expect("doc_id hover");

        assert_eq!(
            extract_hover_payload(absolute),
            extract_hover_payload(doc_id)
        );
    }

    #[tokio::test]
    async fn spark_hover_accepts_repo_relative_input() {
        let harness = HoverToolHarness::new();
        let result = harness
            .server
            .spark_hover(Parameters(HoverArgs {
                file: "spark/src/policy_gateway.ads".to_string(),
                line: 1,
                column: 12,
                symbol: None,
                include_context: Some(false),
                context_lines: None,
            }))
            .await
            .expect("repo-relative hover");

        let structured = result.structured_content.expect("structured payload");
        assert!(structured.get("hover").is_some());
        assert!(structured.get("error").is_none());
    }

    #[tokio::test]
    async fn spark_hover_returns_unmapped_source_reason_for_unknown_doc_id_label() {
        let harness = HoverToolHarness::new();
        let structured = extract_structured(
            harness
                .server
                .spark_hover(Parameters(HoverArgs {
                    file: "unknown-label/src/policy_gateway.ads".to_string(),
                    line: 1,
                    column: 1,
                    symbol: None,
                    include_context: Some(false),
                    context_lines: None,
                }))
                .await
                .expect("hover response"),
        );

        assert_eq!(
            structured.get("reason").and_then(|value| value.as_str()),
            Some("unmapped_source")
        );
        assert_eq!(
            structured
                .get("input_kind")
                .and_then(|value| value.as_str()),
            Some("doc_id")
        );
    }

    #[tokio::test]
    async fn spark_hover_returns_not_found_reason_and_doc_id_hint_for_missing_repo_path() {
        let harness = HoverToolHarness::new();
        let structured = extract_structured(
            harness
                .server
                .spark_hover(Parameters(HoverArgs {
                    file: "spark/src/missing.ads".to_string(),
                    line: 1,
                    column: 1,
                    symbol: None,
                    include_context: Some(false),
                    context_lines: None,
                }))
                .await
                .expect("hover response"),
        );

        assert_eq!(
            structured.get("reason").and_then(|value| value.as_str()),
            Some("not_found")
        );
        assert_eq!(
            structured
                .get("suggested_doc_id")
                .and_then(|value| value.as_str()),
            Some("local-spark/src/missing.ads")
        );
    }

    #[tokio::test]
    async fn spark_hover_returns_outside_root_for_absolute_out_of_mount_path() {
        let harness = HoverToolHarness::new();
        let outside_dir = temp_dir("spark-mcp-tools-outside");
        let outside_file = outside_dir.join("outside.ads");
        fs::write(&outside_file, "procedure Outside;").expect("write outside file");

        let structured = extract_structured(
            harness
                .server
                .spark_hover(Parameters(HoverArgs {
                    file: outside_file.to_string_lossy().to_string(),
                    line: 1,
                    column: 1,
                    symbol: None,
                    include_context: Some(false),
                    context_lines: None,
                }))
                .await
                .expect("hover response"),
        );
        assert_eq!(
            structured.get("reason").and_then(|value| value.as_str()),
            Some("outside_root")
        );
        assert_eq!(
            structured
                .get("input_kind")
                .and_then(|value| value.as_str()),
            Some("absolute")
        );
        let _ = fs::remove_dir_all(outside_dir);
    }

    #[tokio::test]
    async fn spark_index_status_reports_hover_telemetry_counters() {
        let harness = HoverToolHarness::new();

        let _ = harness
            .server
            .spark_hover(Parameters(HoverArgs {
                file: "local-spark/src/policy_gateway.ads".to_string(),
                line: 1,
                column: 12,
                symbol: None,
                include_context: Some(false),
                context_lines: None,
            }))
            .await
            .expect("hover success");
        let _ = harness
            .server
            .spark_hover(Parameters(HoverArgs {
                file: "spark/src/missing.ads".to_string(),
                line: 1,
                column: 1,
                symbol: None,
                include_context: Some(false),
                context_lines: None,
            }))
            .await
            .expect("hover missing");
        let _ = harness
            .server
            .spark_hover(Parameters(HoverArgs {
                file: "unknown-label/src/policy_gateway.ads".to_string(),
                line: 1,
                column: 1,
                symbol: None,
                include_context: Some(false),
                context_lines: None,
            }))
            .await
            .expect("hover unmapped");

        let status = extract_structured(
            harness
                .server
                .spark_index_status(Parameters(IndexStatusArgs {}))
                .await
                .expect("index status"),
        );
        let telemetry = status
            .get("hover_telemetry")
            .expect("hover telemetry field present");
        let input_kinds = telemetry
            .get("input_kinds")
            .and_then(|value| value.as_object())
            .expect("input kinds object");
        let failure_reasons = telemetry
            .get("failure_reasons")
            .and_then(|value| value.as_object())
            .expect("failure reasons object");

        assert!(
            input_kinds
                .get("doc_id")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                >= 2
        );
        assert!(
            input_kinds
                .get("relative")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                >= 1
        );
        assert!(
            failure_reasons
                .get("not_found")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                >= 1
        );
        assert!(
            failure_reasons
                .get("unmapped_source")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                >= 1
        );
    }

    #[tokio::test]
    async fn spark_index_status_exposes_refresh_guidance_and_local_freshness() {
        let harness = HoverToolHarness::new();
        let status = extract_structured(
            harness
                .server
                .spark_index_status(Parameters(IndexStatusArgs {}))
                .await
                .expect("index status"),
        );
        let refresh = status.get("refresh").expect("refresh block");
        assert_eq!(
            refresh.get("mode").and_then(|value| value.as_str()),
            Some("in_process_reindex")
        );
        assert_eq!(
            refresh
                .get("supports_scoped_in_process_refresh")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            refresh
                .get("reason_required")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            refresh.get("tool").and_then(|value| value.as_str()),
            Some("spark.reindex")
        );

        let local_freshness = status.get("local_freshness").expect("local freshness");
        let any_stale = local_freshness
            .get("any_stale")
            .and_then(|value| value.as_bool())
            .expect("any_stale bool");
        assert_eq!(
            refresh.get("status").and_then(|value| value.as_str()),
            Some(if any_stale { "stale" } else { "fresh" })
        );
        assert_eq!(
            refresh.get("next_action").and_then(|value| value.as_str()),
            Some(if any_stale {
                "run_in_process_reindex"
            } else {
                "none"
            })
        );
        let sources = local_freshness
            .get("sources")
            .and_then(|value| value.as_array())
            .expect("local freshness sources");
        assert!(sources.iter().any(|entry| {
            entry
                .get("source")
                .and_then(|value| value.as_str())
                .map(|value| value == "local-spark")
                .unwrap_or(false)
        }));
    }

    #[tokio::test]
    async fn spark_index_status_marks_local_source_stale_after_edit() {
        let harness = HoverToolHarness::new();
        harness.mutate_local_file();

        let status = extract_structured(
            harness
                .server
                .spark_index_status(Parameters(IndexStatusArgs {}))
                .await
                .expect("index status"),
        );
        let local_freshness = status.get("local_freshness").expect("local freshness");
        assert_eq!(
            local_freshness
                .get("any_stale")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        let refresh = status.get("refresh").expect("refresh block");
        assert_eq!(
            refresh.get("status").and_then(|value| value.as_str()),
            Some("stale")
        );
        assert_eq!(
            refresh.get("next_action").and_then(|value| value.as_str()),
            Some("run_in_process_reindex")
        );
        let sources = local_freshness
            .get("sources")
            .and_then(|value| value.as_array())
            .expect("sources");
        let local = sources
            .iter()
            .find(|entry| {
                entry
                    .get("source")
                    .and_then(|value| value.as_str())
                    .map(|value| value == "local-spark")
                    .unwrap_or(false)
            })
            .expect("local-spark source freshness");
        assert_eq!(
            local.get("stale").and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn spark_reindex_rejects_empty_reason() {
        let harness = HoverToolHarness::new();

        let err = harness
            .server
            .spark_reindex(Parameters(ReindexArgs {
                sources: None,
                workspace_paths: None,
                full_reindex: false,
                reason: "   ".to_string(),
            }))
            .await
            .expect_err("empty reason rejected");

        assert!(err.to_string().contains("reason must not be empty"));
    }

    #[tokio::test]
    async fn spark_reindex_rejects_non_local_scope_without_full_opt_in() {
        let harness = HoverToolHarness::new();

        let err = harness
            .server
            .spark_reindex(Parameters(ReindexArgs {
                sources: Some(vec!["seed".to_string()]),
                workspace_paths: None,
                full_reindex: false,
                reason: "refresh local edit".to_string(),
            }))
            .await
            .expect_err("non-local scope rejected");

        assert!(err.to_string().contains("not allowed in scoped mode"));
    }

    #[tokio::test]
    async fn spark_reindex_rejects_missing_workspace_path_fail_closed() {
        let harness = HoverToolHarness::new();

        let err = harness
            .server
            .spark_reindex(Parameters(ReindexArgs {
                sources: Some(vec!["local".to_string()]),
                workspace_paths: Some(vec![harness.workspace_relative_path("src/missing.ads")]),
                full_reindex: false,
                reason: "refresh missing local path".to_string(),
            }))
            .await
            .expect_err("missing workspace path rejected");

        assert!(
            err.to_string()
                .contains("failed to canonicalize workspace path")
        );
    }

    #[tokio::test]
    async fn spark_reindex_preserves_comma_in_workspace_path() {
        let harness = HoverToolHarness::new();
        let comma_path = harness.workspace_relative_path("src/policy,gateway.ads");

        let reindex = extract_structured(
            harness
                .server
                .spark_reindex(Parameters(ReindexArgs {
                    sources: Some(vec!["local".to_string()]),
                    workspace_paths: Some(vec![comma_path.clone()]),
                    full_reindex: false,
                    reason: "refresh comma path".to_string(),
                }))
                .await
                .expect("comma path reindex succeeds"),
        );

        let paths = reindex
            .get("reindex")
            .and_then(|value| value.get("workspace_paths"))
            .and_then(|value| value.as_array())
            .expect("workspace paths array");
        let rendered_paths = paths.iter().map(|value| value.as_str()).collect::<Vec<_>>();
        assert_eq!(rendered_paths, vec![Some(comma_path.as_str())]);
    }

    #[tokio::test]
    async fn spark_reindex_refreshes_stale_local_source_in_process() {
        let harness = HoverToolHarness::new();
        harness.mutate_local_file();

        let stale_status = extract_structured(
            harness
                .server
                .spark_index_status(Parameters(IndexStatusArgs {}))
                .await
                .expect("stale index status"),
        );
        assert_eq!(
            stale_status
                .get("local_freshness")
                .and_then(|value| value.get("any_stale"))
                .and_then(|value| value.as_bool()),
            Some(true)
        );

        let reindex = extract_structured(
            harness
                .server
                .spark_reindex(Parameters(ReindexArgs {
                    sources: Some(vec!["local".to_string()]),
                    workspace_paths: None,
                    full_reindex: false,
                    reason: "refresh edited local source".to_string(),
                }))
                .await
                .expect("reindex succeeds"),
        );
        assert_eq!(
            reindex.get("status").and_then(|value| value.as_str()),
            Some("ok")
        );
        assert_eq!(
            reindex
                .get("reindex")
                .and_then(|value| value.get("effective_sources"))
                .and_then(|value| value.as_array())
                .map(|values| values.iter().any(|value| value == "local-spark")),
            Some(true)
        );

        let fresh_status = extract_structured(
            harness
                .server
                .spark_index_status(Parameters(IndexStatusArgs {}))
                .await
                .expect("fresh index status"),
        );
        assert_eq!(
            fresh_status
                .get("local_freshness")
                .and_then(|value| value.get("any_stale"))
                .and_then(|value| value.as_bool()),
            Some(false)
        );

        let search = extract_structured(
            harness
                .server
                .spark_search(Parameters(search_args("Gateway_Decision_Updated")))
                .await
                .expect("search updated symbol"),
        );
        let results = search
            .get("results")
            .and_then(|value| value.as_array())
            .expect("results array");
        assert!(
            !results.is_empty(),
            "updated local symbol should be searchable"
        );
    }

    #[tokio::test]
    async fn spark_search_literal_identifier_hits_local_for_underscore_and_case_variants() {
        let harness = HoverToolHarness::new();

        for query in ["Gateway_Decision", "gateway_decision"] {
            let structured = extract_structured(
                harness
                    .server
                    .spark_search(Parameters(search_args(query)))
                    .await
                    .expect("spark_search response"),
            );
            let results = structured
                .get("results")
                .and_then(|value| value.as_array())
                .expect("results array");
            assert!(
                !results.is_empty(),
                "expected non-empty results for query {query}"
            );
        }
    }

    #[tokio::test]
    async fn spark_search_reports_search_locate_parity_for_identifier_queries() {
        let harness = HoverToolHarness::new();
        let structured = extract_structured(
            harness
                .server
                .spark_search(Parameters(search_args("Gateway_Decision")))
                .await
                .expect("spark_search response"),
        );
        let parity = structured
            .get("search_locate_parity")
            .and_then(|value| value.as_object())
            .expect("search_locate_parity object");
        assert_eq!(
            parity
                .get("identifier_query")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert!(
            parity
                .get("shared_doc_ids")
                .and_then(|value| value.as_u64())
                .unwrap_or(0)
                >= 1
        );
    }

    #[tokio::test]
    async fn spark_search_literal_multi_token_phrase_hits_local_source() {
        let harness = HoverToolHarness::new();
        let structured = extract_structured(
            harness
                .server
                .spark_search(Parameters(search_args("policy kernel")))
                .await
                .expect("spark_search response"),
        );
        let results = structured
            .get("results")
            .and_then(|value| value.as_array())
            .expect("results array");
        assert!(!results.is_empty(), "expected phrase query local hits");

        let query_behavior = structured
            .get("query_behavior")
            .and_then(|value| value.as_object())
            .expect("query behavior object");
        assert_eq!(
            query_behavior
                .get("multi_token_query")
                .and_then(|value| value.as_bool()),
            Some(true)
        );
        assert_eq!(
            query_behavior
                .get("phrase_strategy")
                .and_then(|value| value.as_str()),
            Some("tokenized_disjunction_with_code_join_fallback")
        );
    }

    #[tokio::test]
    async fn spark_search_mixed_punctuation_and_identifier_phrase_combinations_are_supported() {
        let harness = HoverToolHarness::new();
        for query in ["policy::kernel", "Gateway_Decision policy kernel"] {
            let structured = extract_structured(
                harness
                    .server
                    .spark_search(Parameters(search_args(query)))
                    .await
                    .expect("spark_search response"),
            );
            let results = structured
                .get("results")
                .and_then(|value| value.as_array())
                .expect("results array");
            assert!(
                !results.is_empty(),
                "expected non-empty results for mixed query {query}"
            );
        }
    }

    #[tokio::test]
    async fn spark_search_query_kind_semantics_are_explicit() {
        let harness = HoverToolHarness::new();
        let literal = extract_structured(
            harness
                .server
                .spark_search(Parameters(search_args_with_kind(
                    "policy kernel",
                    SearchQueryKindArg::Literal,
                )))
                .await
                .expect("literal search response"),
        );
        let tantivy = extract_structured(
            harness
                .server
                .spark_search(Parameters(search_args_with_kind(
                    "policy kernel",
                    SearchQueryKindArg::Tantivy,
                )))
                .await
                .expect("tantivy search response"),
        );

        let literal_strategy = literal
            .get("query_behavior")
            .and_then(|value| value.get("phrase_strategy"))
            .and_then(|value| value.as_str());
        let tantivy_strategy = tantivy
            .get("query_behavior")
            .and_then(|value| value.get("phrase_strategy"))
            .and_then(|value| value.as_str());
        assert_eq!(
            literal_strategy,
            Some("tokenized_disjunction_with_code_join_fallback")
        );
        assert_eq!(tantivy_strategy, Some("tantivy_query_parser_syntax"));
    }

    #[tokio::test]
    async fn spark_search_no_results_returns_actionable_guidance() {
        let harness = HoverToolHarness::new();
        let structured = extract_structured(
            harness
                .server
                .spark_search(Parameters(search_args(
                    "missing phrase tokenzzzz anothermissingtoken",
                )))
                .await
                .expect("spark_search response"),
        );
        let results = structured
            .get("results")
            .and_then(|value| value.as_array())
            .expect("results array");
        assert!(results.is_empty(), "expected no hits for missing phrase");

        let guidance = structured
            .get("no_results_guidance")
            .and_then(|value| value.as_str())
            .expect("guidance string");
        assert!(guidance.contains("query_kind=\"tantivy\""));
    }
}
