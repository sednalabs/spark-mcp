//! # Search & Indexing Engine
//!
//! Handles document ingestion, lexical indexing (Tantivy), and semantic search (FastEmbed + HNSW).
//!
//! ## Rationale
//! Provides the core RAG capabilities for the corpus server. It automatically chunks and
//! indexes documents found in the configured corpus directory, enabling high-performance
//! hybrid search.
//!
//! ## Security Boundaries
//! * **Path Traversal**: Validates that all requested document IDs resolve to paths within
//!    the root corpus directory.
//! * **I/O Limits**: Enforces file size limits to prevent DoS attacks via oversized documents.
//! * **Memory Safety**: Uses memory-mapped HNSW indexes for efficient, large-scale semantic search.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use hnsw_rs::anndists::dist::distances::DistDot;
use hnsw_rs::api::AnnT;
use hnsw_rs::hnsw::Hnsw;
use hnsw_rs::hnswio::HnswIo;
use html2text::from_read;
use ignore::WalkBuilder;
use mcp_toolkit_docs::{ChunkConfig, DocumentMeta, chunk_text};
use mcp_toolkit_observability::sanitize_log_value;
use serde::{Deserialize, Serialize};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, EmptyQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, FieldType, IndexRecordOption, STORED, STRING, Schema, SchemaBuilder, TEXT, Value,
};
use tantivy::tokenizer::{TextAnalyzer, TokenStream};
use tantivy::{Index, IndexReader, TantivyDocument, Term};
use thiserror::Error;

const EMBEDDING_SCALE: f32 = 0.999_999;
const CONTEXT_LINES: usize = 3;
const CONTEXT_LINES_MAX: usize = 20;
const DOC_CACHE_CAPACITY: usize = 32;

/// Errors returned by the search engine.
#[derive(Debug, Error)]
pub enum SearchError {
    #[error("index error: {0}")]
    Index(#[from] tantivy::TantivyError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("query parse error: {0}")]
    Query(#[from] tantivy::query::QueryParserError),
    #[error("embedding error: {0}")]
    Embedding(#[from] fastembed::Error),
    #[error("serialization error: {0}")]
    Bincode(#[from] bincode::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("semantic config error: {0}")]
    SemanticConfig(String),
    #[error("invalid doc id: {0}")]
    InvalidDocId(String),
    #[error("invalid reindex request: {0}")]
    ReindexScope(String),
    #[error("reindex already in progress")]
    ReindexBusy,
    #[error("internal reindex state error: {0}")]
    ReindexState(String),
}

/// Global search and indexing configuration.
#[derive(Debug, Clone)]
pub struct SearchConfig {
    pub chunk_config: ChunkConfig,
    pub max_file_bytes: u64,
    pub default_limit: usize,
    pub max_limit: usize,
    pub snippet_max_chars: usize,
    pub semantic: SemanticConfig,
}

#[derive(Debug, Clone)]
pub struct CorpusMountConfig {
    pub label: String,
    pub path: PathBuf,
}

impl CorpusMountConfig {
    pub fn new(label: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            label: label.into(),
            path: path.into(),
        }
    }
}

/// Supported search modes.
#[derive(Debug, Clone, Copy)]
pub enum SearchMode {
    /// BM25 lexical search only.
    Lexical,
    /// Vector-based semantic search only.
    Semantic,
    /// Reciprocal Rank Fusion (RRF) of lexical and semantic results.
    Hybrid,
}

/// Lexical query interpretation mode for Tantivy-backed searches.
#[derive(Debug, Clone, Copy)]
pub enum LexicalQueryKind {
    /// Treat the query as plain text and tokenize it safely for code/signature fragments.
    Literal,
    /// Treat the query as Tantivy query syntax (advanced operators/fields).
    Tantivy,
}

impl SemanticBackend {
    /// Parse a semantic backend string (flat|hnsw).
    pub fn parse(raw: &str) -> Result<Self, SearchError> {
        let normalized = raw.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "flat" | "lexical" => Ok(SemanticBackend::Flat),
            "hnsw" | "ann" => Ok(SemanticBackend::Hnsw),
            "" => Err(SearchError::SemanticConfig(
                "semantic backend must not be empty".to_string(),
            )),
            _ => Err(SearchError::SemanticConfig(format!(
                "unsupported semantic backend: {raw}"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            SemanticBackend::Flat => "flat",
            SemanticBackend::Hnsw => "hnsw",
        }
    }
}

/// Backends for semantic vector search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticBackend {
    /// Brute-force dot product (best for small corpora).
    Flat,
    /// Hierarchical Navigable Small World (HNSW) graph (best for speed).
    Hnsw,
}

#[derive(Debug, Clone)]
pub struct HnswConfig {
    pub m: usize,
    pub ef_construction: usize,
    pub ef_search: usize,
}

/// Configuration for the semantic search layer.
#[derive(Debug, Clone)]
pub struct SemanticConfig {
    pub enabled: bool,
    pub backend: SemanticBackend,
    pub model: String,
    pub index_dir: PathBuf,
    pub cache_dir: Option<PathBuf>,
    pub build_on_start: bool,
    pub batch_size: usize,
    pub top_k: usize,
    pub min_score: f32,
    pub weight: f32,
    pub hnsw: HnswConfig,
}

/// A single search result with citation metadata and a text snippet.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub doc_id: String,
    pub path: String,
    pub title: Option<String>,
    pub source: Option<String>,
    pub score: f32,
    pub snippet: String,
    pub chunk_index: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<LineContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolMatch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SearchOutcome {
    pub hits: Vec<SearchHit>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_defs: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSummary {
    pub source: String,
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct IndexMetadata {
    pub indexed_at_unix_ms: Option<u64>,
    pub source_count: usize,
    pub file_count: usize,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFreshness {
    pub source: String,
    pub indexed_at_unix_ms: Option<u64>,
    pub latest_file_mtime_unix_ms: Option<u64>,
    pub stale: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub staleness_ms: Option<u64>,
    pub file_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalFreshnessReport {
    pub any_stale: bool,
    pub indexed_at_unix_ms: Option<u64>,
    pub sources: Vec<SourceFreshness>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReindexReport {
    pub requested_sources: Vec<String>,
    pub effective_sources: Vec<String>,
    pub workspace_paths: Vec<String>,
    pub reason: String,
    pub indexed_at_unix_ms: Option<u64>,
    pub scan_at_unix_ms: Option<u64>,
    pub scan_file_count: usize,
    pub scan_total_bytes: u64,
}

/// Full content of a document from the corpus.
#[derive(Debug, Clone, Serialize)]
pub struct DocumentContent {
    pub doc_id: String,
    pub path: String,
    pub title: Option<String>,
    pub source: Option<String>,
    pub byte_len: u64,
    pub char_len: usize,
    pub truncated: bool,
    pub content: String,
}

/// content of a specific document chunk.
#[derive(Debug, Clone, Serialize)]
pub struct ChunkContent {
    pub doc_id: String,
    pub path: String,
    pub title: Option<String>,
    pub source: Option<String>,
    pub chunk_index: u64,
    pub start: usize,
    pub end: usize,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HoverResult {
    pub doc_id: String,
    pub path: String,
    pub line: u32,
    pub column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_match: Option<SymbolMatch>,
    pub chunk_index: u64,
    pub snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<LineContext>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub related_defs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LineContext {
    pub line_start: usize,
    pub line_end: usize,
    pub context_start: usize,
    pub context_end: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolMatch {
    pub symbol: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolOccurrence {
    pub symbol: String,
    pub doc_id: String,
    pub path: String,
    pub source: Option<String>,
    pub line: usize,
    pub line_end: usize,
    pub context_start: usize,
    pub context_end: usize,
    pub context: Vec<String>,
    pub excerpt: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Provenance {
    pub kind: String,
    pub source: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EmbeddingRecord {
    doc_id: String,
    chunk_index: u64,
    embedding: Vec<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HnswMeta {
    m: usize,
    ef_construction: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SemanticMeta {
    model: String,
    chunk_max_chars: usize,
    chunk_overlap: usize,
    max_file_bytes: u64,
    records: usize,
    dim: usize,
    #[serde(default)]
    embedding_scale: f32,
    #[serde(default)]
    backend: String,
    #[serde(default)]
    hnsw: Option<HnswMeta>,
}

#[derive(Debug, Clone)]
struct SemanticMatch {
    doc_id: String,
    chunk_index: u64,
    score: f32,
}

/// Thread-safe index providing hybrid search over the document corpus.
pub struct SearchIndex {
    corpus_mounts: Vec<CorpusMount>,
    sources: RwLock<Vec<SourceSummary>>,
    index_metadata: RwLock<IndexMetadata>,
    index_dir: PathBuf,
    _index: Index,
    reader: IndexReader,
    fields: Fields,
    query_parser: QueryParser,
    semantic: Option<SemanticIndex>,
    config: SearchConfig,
    reindex_lock: Mutex<()>,
}

#[derive(Debug, Clone)]
struct CorpusMount {
    label: String,
    root: PathBuf,
}

struct ResolvedDoc<'a> {
    mount: &'a CorpusMount,
    canonical: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum SymbolMatchKind {
    Definition,
    Reference,
    Any,
}

#[derive(Debug, Clone)]
struct Fields {
    doc_id: Field,
    path: Field,
    title: Field,
    source: Field,
    chunk_index: Field,
    body: Field,
}

struct SemanticIndex {
    records: Vec<EmbeddingRecord>,
    dim: usize,
    model: Mutex<TextEmbedding>,
    backend: SemanticBackend,
    hnsw: Option<HnswIndex>,
}

struct HnswIndex {
    index: Hnsw<'static, f32, DistDot>,
    ef_search: usize,
}

const HNSW_BASENAME: &str = "hnsw";

include!("search/runtime.rs");

include!("search/indexing.rs");

include!("search/query.rs");
