use std::path::Path;

use mcp_toolkit_observability::sanitize_error_message;
use spark_mcp::config::load_config;
use spark_mcp::search::{
    CorpusMountConfig, HnswConfig, SearchConfig, SearchIndex, SemanticBackend, SemanticConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = load_config().map_err(|err| {
        tracing::error!(
            error = %sanitize_error_message(&err.to_string(), 512),
            "invalid configuration"
        );
        err
    })?;
    let semantic_backend = SemanticBackend::parse(&config.semantic_backend)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidInput, err.to_string()))?;

    if !config.semantic_enabled {
        return Err("SPARK_MCP_SEMANTIC_ENABLED=1 is required to build embeddings".into());
    }

    let search_config = SearchConfig {
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
    };

    SearchIndex::build_semantic(
        Path::new(&config.corpus_dir),
        &search_config,
        config.reindex,
        config
            .workspace_mounts()
            .into_iter()
            .map(|mount| CorpusMountConfig::new(mount.label, mount.path))
            .collect(),
    )?;
    tracing::info!("semantic embeddings written");
    Ok(())
}
