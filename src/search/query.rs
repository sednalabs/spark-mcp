#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_line_skips_comments_and_strings() {
        let mut depth = 0usize;
        let line = "-- Foo in comment";
        let sanitized = sanitize_line(line, &mut depth);
        assert!(!line_contains_symbol(&sanitized, "Foo"));

        let line = "procedure Foo is";
        let sanitized = sanitize_line(line, &mut depth);
        assert!(line_contains_symbol(&sanitized, "Foo"));

        let line = "\"Foo\"";
        let sanitized = sanitize_line(line, &mut depth);
        assert!(!line_contains_symbol(&sanitized, "Foo"));

        let line = "/* Foo */ procedure Bar is";
        let sanitized = sanitize_line(line, &mut depth);
        assert!(!line_contains_symbol(&sanitized, "Foo"));
        assert!(line_contains_symbol(&sanitized, "Bar"));
    }

    #[test]
    fn sanitize_line_tracks_block_comments() {
        let mut depth = 0usize;
        let line = "/* Foo";
        let sanitized = sanitize_line(line, &mut depth);
        assert!(!line_contains_symbol(&sanitized, "Foo"));
        assert!(depth > 0);

        let line = "Bar */ procedure Baz";
        let sanitized = sanitize_line(line, &mut depth);
        assert!(line_contains_symbol(&sanitized, "Baz"));
        assert_eq!(depth, 0);
    }

    #[test]
    fn infer_symbol_from_line_extracts_identifier_at_column() {
        let line = "procedure Parse_Gateway_Decision_Input is";
        let col = line.find("Gateway").unwrap() as u32 + 2;
        let inferred = infer_symbol_from_line(line, col);
        assert_eq!(inferred, Some("Parse_Gateway_Decision_Input".to_string()));
    }

    #[test]
    fn infer_symbol_from_line_supports_dotted_names() {
        let line = "Pkg.Parser.Parse (Input);";
        let col = line.find("Parser").unwrap() as u32 + 2;
        let inferred = infer_symbol_from_line(line, col);
        assert_eq!(inferred, Some("Pkg.Parser.Parse".to_string()));
    }

    #[test]
    fn infer_symbol_from_line_returns_none_for_whitespace() {
        let line = "   ( )";
        let inferred = infer_symbol_from_line(line, 1);
        assert!(inferred.is_none());
    }

    #[test]
    fn source_filter_supports_local_alias() {
        assert!(matches_source_filter("local", Some("local-spark")));
        assert!(matches_source_filter("local,manual", Some("local-fstar")));
        assert!(!matches_source_filter("local", Some("manual")));
    }

    #[test]
    fn split_source_filter_trims_and_drops_empty_values() {
        let filters = split_source_filter("  local-spark, , manual ,");
        assert_eq!(filters, vec!["local-spark".to_string(), "manual".to_string()]);
    }
}

fn collect_symbol_matches(
    mounts: &[CorpusMount],
    symbol: &str,
    limit: usize,
    source: Option<&str>,
    kind: SymbolMatchKind,
    include_context: bool,
    context_lines: Option<usize>,
    max_file_bytes: u64,
) -> Result<Vec<SymbolOccurrence>, SearchError> {
    let symbol = symbol.trim();
    if symbol.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let context_lines = clamp_context_lines(context_lines);
    let source_filter = source.map(|value| value.trim().to_string());
    let mut matches = Vec::new();

    for_each_corpus_file(mounts, max_file_bytes, |mount, path, _meta| {
        let doc_meta = document_meta(mount, path);
        if let Some(filter) = source_filter.as_ref() {
            if !matches_source_filter(filter, doc_meta.source.as_deref()) {
                return Ok(WalkOutcome::Continue);
            }
        }

        let raw = match read_text(path) {
            Some(text) => text,
            None => return Ok(WalkOutcome::Continue),
        };
        if raw.trim().is_empty() {
            return Ok(WalkOutcome::Continue);
        }

        let mut comment_depth = 0usize;
        let lines: Vec<&str> = raw.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            let sanitized = sanitize_line(line, &mut comment_depth);
            if sanitized.trim().is_empty() {
                continue;
            }
            if !line_contains_symbol(&sanitized, symbol) {
                continue;
            }

            let is_definition = definition_kind_for_line(&sanitized, symbol).is_some();
            let include = match kind {
                SymbolMatchKind::Definition => is_definition,
                SymbolMatchKind::Reference => !is_definition,
                SymbolMatchKind::Any => true,
            };
            if !include {
                continue;
            }

            let line_number = idx + 1;
            let context = if include_context {
                build_line_context_from_lines(&lines, line_number, line_number, context_lines)
            } else {
                LineContext {
                    line_start: line_number,
                    line_end: line_number,
                    context_start: line_number,
                    context_end: line_number,
                    lines: Vec::new(),
                }
            };

            let excerpt = line.trim().to_string();
            let signature = if is_definition {
                Some(excerpt.clone())
            } else {
                None
            };
            let provenance = provenance_from_source(doc_meta.source.as_deref());

            matches.push(SymbolOccurrence {
                symbol: symbol.to_string(),
                doc_id: doc_meta.id.clone(),
                path: doc_meta.path.clone(),
                source: doc_meta.source.clone(),
                line: line_number,
                line_end: line_number,
                context_start: context.context_start,
                context_end: context.context_end,
                context: context.lines,
                excerpt,
                kind: if is_definition {
                    "definition".to_string()
                } else {
                    "reference".to_string()
                },
                signature,
                provenance,
            });

            if matches.len() >= limit {
                return Ok(WalkOutcome::Break);
            }
        }

        Ok(WalkOutcome::Continue)
    })?;

    Ok(matches)
}

fn matches_source_filter(filter: &str, source: Option<&str>) -> bool {
    let Some(source) = source else {
        return false;
    };
    let source = source.trim();
    if source.is_empty() {
        return false;
    }
    split_source_filter(filter).into_iter().any(|candidate| {
        if candidate.eq_ignore_ascii_case("local") {
            return source.starts_with("local-");
        }
        candidate == source || source.starts_with(&format!("{candidate}/"))
    })
}

fn split_source_filter(filter: &str) -> Vec<String> {
    filter
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect()
}

fn parse_embedding_model(name: &str) -> Result<EmbeddingModel, SearchError> {
    let normalized = name.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "" | "default" | "all-minilm-l6-v2" | "allminilml6v2" | "all_minilm_l6_v2" => {
            Ok(EmbeddingModel::AllMiniLML6V2)
        }
        "all-minilm-l6-v2-q" | "all-minilm-l6-v2-quantized" | "allminilml6v2q" => {
            Ok(EmbeddingModel::AllMiniLML6V2Q)
        }
        "bge-small-en-v1.5" | "bge-small-en" | "bge-small" => {
            Ok(EmbeddingModel::BGESmallENV15)
        }
        "bge-small-en-v1.5-q" | "bge-small-en-quantized" | "bge-small-q" => {
            Ok(EmbeddingModel::BGESmallENV15Q)
        }
        "bge-base-en-v1.5" | "bge-base-en" | "bge-base" => Ok(EmbeddingModel::BGEBaseENV15),
        "bge-base-en-v1.5-q" | "bge-base-en-quantized" | "bge-base-q" => {
            Ok(EmbeddingModel::BGEBaseENV15Q)
        }
        _ => Err(SearchError::SemanticConfig(format!(
            "unknown embedding model '{name}' (supported: all-minilm-l6-v2, all-minilm-l6-v2-q, bge-small-en-v1.5, bge-small-en-v1.5-q, bge-base-en-v1.5, bge-base-en-v1.5-q)"
        ))),
    }
}

fn meta_matches(meta: &SemanticMeta, config: &SearchConfig, semantic: &SemanticConfig) -> bool {
    meta.model == semantic.model
        && meta.chunk_max_chars == config.chunk_config.max_chars
        && meta.chunk_overlap == config.chunk_config.overlap
        && meta.max_file_bytes == config.max_file_bytes
        && (meta.embedding_scale - EMBEDDING_SCALE).abs() <= 1.0e-6
}

fn hnsw_meta_matches(stored: &HnswMeta, expected: &HnswMeta) -> bool {
    stored.m == expected.m
        && stored.ef_construction == expected.ef_construction
}

fn hnsw_files_exist(index_dir: &Path) -> bool {
    let graph = hnsw_graph_path(index_dir);
    let data = hnsw_data_path(index_dir);
    graph.exists() && data.exists()
}

fn hnsw_graph_path(index_dir: &Path) -> PathBuf {
    index_dir.join(format!("{HNSW_BASENAME}.hnsw.graph"))
}

fn hnsw_data_path(index_dir: &Path) -> PathBuf {
    index_dir.join(format!("{HNSW_BASENAME}.hnsw.data"))
}

fn build_hnsw_index(
    records: &[EmbeddingRecord],
    config: &HnswConfig,
    index_dir: &Path,
) -> Result<HnswIndex, SearchError> {
    if records.is_empty() {
        return Err(SearchError::SemanticConfig(
            "cannot build HNSW index with no records".to_string(),
        ));
    }
    let max_layer = ((records.len() as f32).ln().ceil() as usize).clamp(1, 16);
    tracing::info!(
        records = records.len(),
        max_layer,
        m = config.m,
        ef_construction = config.ef_construction,
        "building HNSW index"
    );

    let mut hnsw: Hnsw<'static, f32, DistDot> =
        Hnsw::new(config.m, records.len(), max_layer, config.ef_construction, DistDot::default());

    let mut data = Vec::with_capacity(records.len());
    for (idx, record) in records.iter().enumerate() {
        data.push((&record.embedding, idx));
    }
    hnsw.parallel_insert(&data);
    hnsw.set_searching_mode(true);
    hnsw.file_dump(index_dir, HNSW_BASENAME)
        .map_err(|err| SearchError::SemanticConfig(format!("failed to dump HNSW index: {err}")))?;

    Ok(HnswIndex {
        index: hnsw,
        ef_search: config.ef_search,
    })
}

fn load_hnsw_index(index_dir: &Path, ef_search: usize) -> Result<HnswIndex, SearchError> {
    // HnswIo ties the loaded graph lifetime to the loader, so we leak it to
    // keep the mapped data alive for the server process lifetime.
    let io = Box::new(HnswIo::new(index_dir, HNSW_BASENAME));
    let io = Box::leak(io);
    let mut hnsw: Hnsw<'static, f32, DistDot> = io
        .load_hnsw()
        .map_err(|err| SearchError::SemanticConfig(format!("failed to load HNSW index: {err}")))?;
    hnsw.set_searching_mode(true);
    Ok(HnswIndex {
        index: hnsw,
        ef_search,
    })
}

fn build_embeddings(
    mounts: &[CorpusMount],
    config: &SearchConfig,
    embedder: &mut TextEmbedding,
) -> Result<(Vec<EmbeddingRecord>, usize), SearchError> {
    tracing::info!(
        batch_size = config.semantic.batch_size,
        "building semantic embeddings"
    );
    let mut records: Vec<EmbeddingRecord> = Vec::new();
    let mut batch_texts: Vec<String> = Vec::new();
    let mut batch_meta: Vec<(String, u64)> = Vec::new();
    let batch_size = config.semantic.batch_size.max(1);

    for_each_corpus_file(mounts, config.max_file_bytes, |mount, path, _meta| {
        let text = match read_text(path) {
            Some(text) => text,
            None => return Ok(WalkOutcome::Continue),
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(WalkOutcome::Continue);
        }

        let doc_meta = document_meta(mount, path);
        let chunks = chunk_text(text, config.chunk_config);
        if chunks.is_empty() {
            return Ok(WalkOutcome::Continue);
        }
        for chunk in chunks {
            batch_texts.push(format!("passage: {}", chunk.text));
            batch_meta.push((doc_meta.id.clone(), chunk.index as u64));
            if batch_texts.len() >= batch_size {
                flush_embedding_batch(
                    &mut records,
                    &mut batch_texts,
                    &mut batch_meta,
                    embedder,
                    batch_size,
                )?;
            }
        }

        Ok(WalkOutcome::Continue)
    })?;

    flush_embedding_batch(
        &mut records,
        &mut batch_texts,
        &mut batch_meta,
        embedder,
        batch_size,
    )?;

    let dim = records.first().map(|r| r.embedding.len()).unwrap_or(0);
    tracing::info!(records = records.len(), dim, "spark semantic embeddings built");
    Ok((records, dim))
}

fn flush_embedding_batch(
    records: &mut Vec<EmbeddingRecord>,
    batch_texts: &mut Vec<String>,
    batch_meta: &mut Vec<(String, u64)>,
    embedder: &mut TextEmbedding,
    batch_size: usize,
) -> Result<(), SearchError> {
    if batch_texts.is_empty() {
        return Ok(());
    }

    let texts = std::mem::take(batch_texts);
    let meta = std::mem::take(batch_meta);
    let embeddings = embedder.embed(texts, Some(batch_size))?;

    if embeddings.len() != meta.len() {
        return Err(SearchError::SemanticConfig(
            "embedding batch size mismatch".to_string(),
        ));
    }

    for ((doc_id, chunk_index), mut embedding) in meta.into_iter().zip(embeddings.into_iter()) {
        normalize_embedding(&mut embedding).map_err(|err| {
            SearchError::SemanticConfig(format!(
                "embedding invalid for {doc_id} chunk {chunk_index}: {err}"
            ))
        })?;
        records.push(EmbeddingRecord {
            doc_id,
            chunk_index,
            embedding,
        });
    }

    Ok(())
}

fn load_embeddings(path: &Path) -> Result<Vec<EmbeddingRecord>, SearchError> {
    let data = fs::read(path)?;
    let records: Vec<EmbeddingRecord> = bincode::deserialize(&data)?;
    Ok(records)
}

fn save_embeddings(path: &Path, records: &[EmbeddingRecord]) -> Result<(), SearchError> {
    let data = bincode::serialize(records)?;
    fs::write(path, data)?;
    Ok(())
}

fn normalize_embedding(embedding: &mut [f32]) -> Result<(), &'static str> {
    let mut norm_sq = 0.0_f32;
    for value in embedding.iter() {
        if !value.is_finite() {
            return Err("embedding contains non-finite values");
        }
        norm_sq += value * value;
    }
    if !norm_sq.is_finite() {
        return Err("embedding norm is non-finite");
    }
    if norm_sq > 0.0 {
        let inv = norm_sq.sqrt().recip();
        if !inv.is_finite() {
            return Err("embedding norm is non-finite");
        }
        for value in embedding.iter_mut() {
            *value = *value * inv * EMBEDDING_SCALE;
        }
    }
    Ok(())
}

fn dot_product(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right.iter()).map(|(a, b)| a * b).sum()
}

fn doc_id_matches_source(doc_id: &str, source: &str) -> bool {
    doc_id
        .split('/')
        .next()
        .map(|component| component == source)
        .unwrap_or(false)
}

fn clamp_doc_chars(requested: Option<usize>) -> usize {
    let default_chars = 8000usize;
    let max_chars = 100_000usize;
    let value = requested.unwrap_or(default_chars);
    value.clamp(256, max_chars)
}
