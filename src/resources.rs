//! # MCP Resources
//!
//! Exposes lightweight operational resources (help, index status, local guidance)
//! without requiring them to live in the search corpus.

use std::fs;
use std::path::{Path, PathBuf};

use mcp_toolkit_core::rmcp_models;
use mcp_toolkit_http::session::SessionStats;
use rmcp::model::{
    Annotated, RawResource, RawResourceTemplate, ReadResourceResult, Resource, ResourceContents,
    ResourceTemplate,
};
use serde_json::json;

use crate::search::SearchIndex;

const HELP_URI: &str = "spark-mcp://help";
const INDEX_URI: &str = "spark-mcp://index-status";
const DOC_PREFIX: &str = "spark-mcp://doc/";
const CHUNK_PREFIX: &str = "spark-mcp://chunk/";
const WORKSPACE_PREFIX: &str = "spark-mcp://workspace/";
const SPEC_PREFIX: &str = "spark-mcp://spec/";

const MIME_MARKDOWN: &str = "text/markdown";
const MIME_JSON: &str = "application/json";
const MIME_TEXT: &str = "text/plain";

pub fn list_resources(
    search: &SearchIndex,
    session: Option<&SessionStats>,
    resume_mode: Option<&str>,
) -> Vec<Resource> {
    let mut resources = Vec::new();
    let help_text = build_help_text();
    resources.push(resource_for_text(
        HELP_URI,
        "help",
        "SPARK MCP help",
        "Tool summary, local source setup, and common workflows.",
        MIME_MARKDOWN,
        help_text.as_bytes().len(),
    ));
    resources.push(resource_for_text(
        INDEX_URI,
        "index-status",
        "Index status",
        "Index metadata, local freshness, and refresh runbook.",
        MIME_JSON,
        build_index_status(search, session, resume_mode)
            .as_bytes()
            .len(),
    ));

    if let Some(workspace_root) = search.workspace_root() {
        for guidance in local_guidance_files(&workspace_root) {
            if let Some(resource) = resource_for_file(&workspace_root, &guidance) {
                resources.push(resource);
            }
        }
    }

    resources
}

pub fn list_resource_templates(search: &SearchIndex) -> Vec<ResourceTemplate> {
    let mut templates = vec![
        Annotated::new(
            RawResourceTemplate {
                uri_template: format!("{DOC_PREFIX}{{doc_id}}"),
                name: "corpus-doc".to_string(),
                title: Some("Corpus document".to_string()),
                description: Some(
                    "Read a corpus document by doc_id (same doc_id used by spark.get_doc)."
                        .to_string(),
                ),
                mime_type: Some(MIME_TEXT.to_string()),
                icons: None,
            },
            None,
        ),
        Annotated::new(
            RawResourceTemplate {
                uri_template: format!("{CHUNK_PREFIX}{{doc_id}}/{{chunk_index}}"),
                name: "corpus-chunk".to_string(),
                title: Some("Corpus chunk".to_string()),
                description: Some(
                    "Read a corpus chunk by doc_id + chunk_index (same keys used by spark.get_chunk)."
                        .to_string(),
                ),
                mime_type: Some(MIME_TEXT.to_string()),
                icons: None,
            },
            None,
        ),
    ];

    if search.workspace_root().is_none() {
        return templates;
    }

    templates.extend([
        Annotated::new(
            RawResourceTemplate {
                uri_template: format!("{WORKSPACE_PREFIX}{{path}}"),
                name: "workspace-file".to_string(),
                title: Some("Workspace file".to_string()),
                description: Some(
                    "Read a markdown file under the workspace root (relative path).".to_string(),
                ),
                mime_type: Some(MIME_MARKDOWN.to_string()),
                icons: None,
            },
            None,
        ),
        Annotated::new(
            RawResourceTemplate {
                uri_template: format!("{SPEC_PREFIX}{{doc}}"),
                name: "spec-doc".to_string(),
                title: Some("Spec document".to_string()),
                description: Some(
                    "Read a spec doc by name (maps to spec/<doc>.md in the workspace).".to_string(),
                ),
                mime_type: Some(MIME_MARKDOWN.to_string()),
                icons: None,
            },
            None,
        ),
    ]);

    templates
}

pub fn read_resource(
    search: &SearchIndex,
    uri: &str,
    session: Option<&SessionStats>,
    resume_mode: Option<&str>,
) -> Result<ReadResourceResult, rmcp::ErrorData> {
    if uri == HELP_URI {
        return Ok(rmcp_models::read_resource_result(vec![
            ResourceContents::TextResourceContents {
                uri: HELP_URI.to_string(),
                mime_type: Some(MIME_MARKDOWN.to_string()),
                text: build_help_text(),
                meta: None,
            },
        ]));
    }
    if uri == INDEX_URI {
        return Ok(rmcp_models::read_resource_result(vec![
            ResourceContents::TextResourceContents {
                uri: INDEX_URI.to_string(),
                mime_type: Some(MIME_JSON.to_string()),
                text: build_index_status(search, session, resume_mode),
                meta: None,
            },
        ]));
    }

    if let Some(doc_id) = uri.strip_prefix(DOC_PREFIX) {
        let doc_id = doc_id.trim_matches('/').trim();
        if doc_id.is_empty() {
            return Err(rmcp::ErrorData::resource_not_found(
                "doc_id is required",
                None,
            ));
        }
        let doc = search
            .get_doc(doc_id, Some(200_000))
            .map_err(|err| rmcp::ErrorData::resource_not_found(err.to_string(), None))?;
        let Some(doc) = doc else {
            return Err(rmcp::ErrorData::resource_not_found(
                "document not found",
                None,
            ));
        };
        let mime = mime_type_for_path(Path::new(&doc.path));
        let mut rendered = String::new();
        rendered.push_str("SPARK_MCP_DOC_V1\n");
        rendered.push_str(&format!("doc_id: {}\n", doc.doc_id));
        rendered.push_str(&format!("path: {}\n", doc.path));
        if let Some(title) = doc
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            rendered.push_str(&format!("title: {title}\n"));
        }
        if let Some(source) = doc
            .source
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
        {
            rendered.push_str(&format!("source: {source}\n"));
        }
        rendered.push_str(&format!(
            "truncated: {}\nchar_len: {}\n\n",
            doc.truncated, doc.char_len
        ));
        rendered.push_str(doc.content.trim());
        rendered.push('\n');
        return Ok(rmcp_models::read_resource_result(vec![
            ResourceContents::TextResourceContents {
                uri: uri.to_string(),
                mime_type: Some(mime.to_string()),
                text: rendered,
                meta: None,
            },
        ]));
    }

    if let Some(rest) = uri.strip_prefix(CHUNK_PREFIX) {
        let rest = rest.trim_matches('/').trim();
        if rest.is_empty() {
            return Err(rmcp::ErrorData::resource_not_found(
                "doc_id/chunk_index is required",
                None,
            ));
        }
        let mut parts = rest.split('/').collect::<Vec<_>>();
        if parts.len() < 2 {
            return Err(rmcp::ErrorData::resource_not_found(
                "expected chunk URI spark-mcp://chunk/<doc_id>/<chunk_index>",
                None,
            ));
        }
        let chunk_raw = parts.pop().unwrap_or_default().trim();
        let chunk_index = chunk_raw
            .parse::<u64>()
            .map_err(|_| rmcp::ErrorData::resource_not_found("invalid chunk_index", None))?;
        let doc_id = parts.join("/").trim().to_string();
        if doc_id.is_empty() {
            return Err(rmcp::ErrorData::resource_not_found(
                "doc_id is required",
                None,
            ));
        }

        let chunk = search
            .get_chunk(&doc_id, chunk_index)
            .map_err(|err| rmcp::ErrorData::resource_not_found(err.to_string(), None))?;
        let Some(chunk) = chunk else {
            return Err(rmcp::ErrorData::resource_not_found("chunk not found", None));
        };
        let mime = mime_type_for_path(Path::new(&chunk.path));
        let mut rendered = String::new();
        rendered.push_str("SPARK_MCP_CHUNK_V1\n");
        rendered.push_str(&format!("doc_id: {}\n", chunk.doc_id));
        rendered.push_str(&format!("path: {}\n", chunk.path));
        rendered.push_str(&format!("chunk_index: {}\n", chunk.chunk_index));
        if let Some(title) = chunk
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
        {
            rendered.push_str(&format!("title: {title}\n"));
        }
        if let Some(source) = chunk
            .source
            .as_deref()
            .map(str::trim)
            .filter(|source| !source.is_empty())
        {
            rendered.push_str(&format!("source: {source}\n"));
        }
        rendered.push('\n');
        rendered.push_str(chunk.content.trim());
        rendered.push('\n');
        return Ok(rmcp_models::read_resource_result(vec![
            ResourceContents::TextResourceContents {
                uri: uri.to_string(),
                mime_type: Some(mime.to_string()),
                text: rendered,
                meta: None,
            },
        ]));
    }

    let Some(workspace_root) = search.workspace_root() else {
        return Err(rmcp::ErrorData::resource_not_found(
            "workspace root not configured",
            None,
        ));
    };

    if let Some(path) = resolve_workspace_resource(&workspace_root, uri) {
        let contents = fs::read_to_string(&path)
            .map_err(|err| rmcp::ErrorData::resource_not_found(err.to_string(), None))?;
        let mime = mime_type_for_path(&path);
        return Ok(rmcp_models::read_resource_result(vec![
            ResourceContents::TextResourceContents {
                uri: uri.to_string(),
                mime_type: Some(mime.to_string()),
                text: contents,
                meta: None,
            },
        ]));
    }

    Err(rmcp::ErrorData::resource_not_found(
        "resource not found",
        None,
    ))
}

fn build_help_text() -> String {
    [
        "# SPARK MCP help",
        "",
        "Tools:",
        "- spark.search / spark.list_sources / spark.get_doc / spark.get_chunk / spark.hover",
        "- spark.index_status",
        "- spark_locate / spark_refs",
        "",
        "Session resumption:",
        "- `index-status.session.resume_mode` reports off/historyless/replay.",
        "",
        "Include local sources:",
        "- Set SPARK_MCP_WORKSPACE_ROOT=/path/to/local/workspace",
        "- SPARK_MCP_INCLUDE_WORKSPACE_RUST=1 (default when workspace root is set)",
        "- Filter searches with source=local-spark (or other local labels).",
        "",
        "Common workflows:",
        "1) Search -> chunk: spark.search then spark.get_chunk for citations.",
        "2) Local search: spark.search with source=local-spark and include_context=true.",
        "3) Check index status: spark.index_status.",
        "4) Lexical hover: spark.hover with file + line + column (optional symbol override).",
        "5) Symbol discovery: spark_locate and spark_refs.",
        "",
        "Resource templates:",
        "- spark-mcp://doc/<doc_id>",
        "- spark-mcp://chunk/<doc_id>/<chunk_index>",
        "- spark-mcp://workspace/<path>",
        "- spark-mcp://spec/<doc>",
        "",
    ]
    .join("\n")
}

fn build_index_status(
    search: &SearchIndex,
    session: Option<&SessionStats>,
    resume_mode: Option<&str>,
) -> String {
    let index = search.index_metadata();
    let sources = search.list_sources();
    let local_freshness = search.local_freshness_report().ok();
    let local_any_stale = local_freshness
        .as_ref()
        .map(|report| report.any_stale)
        .unwrap_or(false);
    let local_sources: Vec<_> = sources
        .iter()
        .filter(|source| source.source.starts_with("local-"))
        .collect();
    let workspace_root = search
        .workspace_root()
        .map(|root| root.to_string_lossy().to_string());
    let mut payload = json!({
        "index": index,
        "sources": sources,
        "local_freshness": local_freshness,
        "local_sources": local_sources,
        "workspace_root": workspace_root,
        "refresh": {
            "mode": "restart_with_reindex",
            "supports_scoped_in_process_refresh": false,
            "reason_required": true,
            "default_scope": "all_local_sources",
            "status": if local_any_stale { "stale" } else { "fresh" },
            "next_action": if local_any_stale { "run_refresh_preset" } else { "none" },
            "build_helper_preset": "spark.mcp-refresh-local",
            "reason_contract": {
                "required": true,
                "transport": "build_helper_note",
                "example": "local-spark stale after edits",
                "description": "record a short audit reason in build.start_and_wait note"
            },
            "commands": [
                "build_helper_mcp/build.start_and_wait preset_id=spark.mcp-refresh-local note=\"<reason>\"",
                "SPARK_MCP_SEMANTIC_ENABLED=0 SPARK_MCP_REINDEX=1 cargo run --release --bin spark-mcp",
                "./scripts/ingest_docs.sh && SPARK_MCP_SEMANTIC_ENABLED=0 SPARK_MCP_REINDEX=1 cargo run --release --bin spark-mcp"
            ],
            "post_refresh_verify": [
                "spark.index_status -> local_freshness.any_stale == false",
                "spark.search source=local-spark include_context=true for edited symbol/file",
                "spark.hover on edited file position resolves expected symbol"
            ],
            "guidance": "Use the first command after local edits; use the second when corpus ingestion sources changed."
        }
    });
    if let Some(stats) = session {
        let resume_mode = resume_mode.unwrap_or("unknown");
        payload["session"] = json!({
            "active_streams": stats.active_sessions,
            "max_streams": stats.max_sessions,
            "resume_enabled": resume_mode != "off",
            "resume_mode": resume_mode,
        });
    }
    serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
}

fn local_guidance_files(workspace_root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let candidates = [
        PathBuf::from("README.md"),
        PathBuf::from("spark/AGENTS.md"),
        PathBuf::from("spark/README.md"),
    ];
    for candidate in candidates {
        let path = workspace_root.join(&candidate);
        if path.exists() {
            files.push(candidate);
        }
    }

    let spec_dir = workspace_root.join("spec");
    if let Ok(entries) = fs::read_dir(&spec_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                if let Ok(rel) = path.strip_prefix(workspace_root) {
                    files.push(rel.to_path_buf());
                }
            }
        }
    }

    files.sort();
    files
}

fn resource_for_text(
    uri: &str,
    name: &str,
    title: &str,
    description: &str,
    mime_type: &str,
    size: usize,
) -> Resource {
    Annotated::new(
        RawResource {
            uri: uri.to_string(),
            name: name.to_string(),
            title: Some(title.to_string()),
            description: Some(description.to_string()),
            mime_type: Some(mime_type.to_string()),
            size: Some(size.min(u32::MAX as usize) as u32),
            icons: None,
            meta: None,
        },
        None,
    )
}

fn resource_for_file(workspace_root: &Path, rel_path: &Path) -> Option<Resource> {
    let path = workspace_root.join(rel_path);
    let metadata = fs::metadata(&path).ok()?;
    let uri = format!("{WORKSPACE_PREFIX}{}", rel_path.to_string_lossy());
    Some(Annotated::new(
        RawResource {
            uri,
            name: rel_path.to_string_lossy().to_string(),
            title: Some(rel_path.to_string_lossy().to_string()),
            description: Some("Workspace guidance".to_string()),
            mime_type: Some(mime_type_for_path(&path).to_string()),
            size: Some(metadata.len().min(u32::MAX as u64) as u32),
            icons: None,
            meta: None,
        },
        None,
    ))
}

fn resolve_workspace_resource(workspace_root: &Path, uri: &str) -> Option<PathBuf> {
    if let Some(rel) = uri.strip_prefix(WORKSPACE_PREFIX) {
        return resolve_under_workspace(workspace_root, rel, true);
    }
    if let Some(doc) = uri.strip_prefix(SPEC_PREFIX) {
        let mut rel = doc.trim().to_string();
        if rel.is_empty() {
            return None;
        }
        if !rel.ends_with(".md") {
            rel.push_str(".md");
        }
        let spec_rel = format!("spec/{rel}");
        return resolve_under_workspace(workspace_root, &spec_rel, true);
    }
    None
}

fn resolve_under_workspace(
    workspace_root: &Path,
    rel: &str,
    markdown_only: bool,
) -> Option<PathBuf> {
    if rel.contains("..") {
        return None;
    }
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        return None;
    }
    let path = workspace_root.join(rel);
    let canonical = path.canonicalize().ok()?;
    if !canonical.starts_with(workspace_root) {
        return None;
    }
    if markdown_only {
        let is_markdown = matches!(canonical.extension(), Some(ext) if ext == "md");
        if !is_markdown {
            return None;
        }
    }
    Some(canonical)
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("md") => MIME_MARKDOWN,
        Some("json") => MIME_JSON,
        _ => MIME_TEXT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{
        CorpusMountConfig, HnswConfig, SearchConfig, SearchIndex, SemanticBackend, SemanticConfig,
    };
    use mcp_toolkit_docs::ChunkConfig;
    use rmcp::model::ResourceContents;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

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

    #[tokio::test]
    async fn list_resource_templates_includes_doc_and_chunk_without_workspace_root() {
        let corpus_dir = temp_dir("spark-mcp-corpus");
        let index_dir = temp_dir("spark-mcp-index");
        let semantic_dir = temp_dir("spark-mcp-semantic");
        fs::write(corpus_dir.join("seed.md"), "seed").expect("write corpus");

        let search = SearchIndex::open_or_create(
            &corpus_dir,
            &index_dir,
            test_config(&semantic_dir),
            false,
            Vec::<CorpusMountConfig>::new(),
        )
        .expect("index");

        let templates = list_resource_templates(&search);
        assert!(
            templates
                .iter()
                .any(|template| template.name == "corpus-doc")
        );
        assert!(
            templates
                .iter()
                .any(|template| template.name == "corpus-chunk")
        );
        assert!(
            !templates
                .iter()
                .any(|template| template.name == "workspace-file")
        );

        let _ = fs::remove_dir_all(&corpus_dir);
        let _ = fs::remove_dir_all(&index_dir);
        let _ = fs::remove_dir_all(&semantic_dir);
    }

    #[tokio::test]
    async fn read_resource_supports_doc_and_chunk_templates() {
        let corpus_dir = temp_dir("spark-mcp-corpus");
        let index_dir = temp_dir("spark-mcp-index");
        let semantic_dir = temp_dir("spark-mcp-semantic");
        fs::write(
            corpus_dir.join("seed.md"),
            "# Seed\n\nSPARK resource template content.",
        )
        .expect("write corpus");

        let search = SearchIndex::open_or_create(
            &corpus_dir,
            &index_dir,
            test_config(&semantic_dir),
            false,
            Vec::<CorpusMountConfig>::new(),
        )
        .expect("index");

        let doc = read_resource(&search, "spark-mcp://doc/seed.md", None, None).expect("read doc");
        match &doc.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => {
                assert!(text.contains("SPARK_MCP_DOC_V1"));
                assert!(text.contains("SPARK resource template content."));
            }
            _ => panic!("expected text resource"),
        }

        let chunk =
            read_resource(&search, "spark-mcp://chunk/seed.md/0", None, None).expect("read chunk");
        match &chunk.contents[0] {
            ResourceContents::TextResourceContents { text, .. } => {
                assert!(text.contains("SPARK_MCP_CHUNK_V1"));
                assert!(text.contains("SPARK resource template content."));
            }
            _ => panic!("expected text resource"),
        }

        let _ = fs::remove_dir_all(&corpus_dir);
        let _ = fs::remove_dir_all(&index_dir);
        let _ = fs::remove_dir_all(&semantic_dir);
    }
}
