impl SearchIndex {
    /// Open an existing index or crawl the corpus to build a new one.
    ///
    /// # Security
    /// * **Root Scoping**: Canonicalizes corpus roots to ensure all file access is relative.
    pub fn open_or_create(
        corpus_dir: &Path,
        index_dir: &Path,
        config: SearchConfig,
        reindex: bool,
        extra_mounts: Vec<CorpusMountConfig>,
    ) -> Result<Self, SearchError> {
        fs::create_dir_all(corpus_dir)?;
        if reindex && index_dir.exists() {
            fs::remove_dir_all(index_dir)?;
        }
        if let Some(parent) = index_dir.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(index_dir)?;

        let corpus_mounts = build_mounts(corpus_dir, extra_mounts);
        let sources = scan_sources(&corpus_mounts, &config)?;
        let index_metadata = build_index_metadata(index_dir, &sources);

        let schema = build_schema();
        let fields = Fields::from_schema(&schema);

        let index = if index_exists(index_dir) {
            Index::open_in_dir(index_dir)?
        } else {
            Index::create_in_dir(index_dir, schema.clone())?
        };

        if reindex || !index_exists(index_dir) {
            build_index(&index, &corpus_mounts, &fields, &config)?;
        }

        let reader = index.reader()?;
        let query_parser = QueryParser::for_index(&index, vec![fields.title, fields.body]);
        let semantic = if config.semantic.enabled {
            SemanticIndex::open_or_build(&corpus_mounts, &config, reindex)?
        } else {
            None
        };

        Ok(Self {
            corpus_mounts,
            sources: RwLock::new(sources),
            index_metadata: RwLock::new(index_metadata),
            index_dir: index_dir.to_path_buf(),
            _index: index,
            reader,
            fields,
            query_parser,
            semantic,
            config,
            reindex_lock: Mutex::new(()),
        })
    }

    /// Execute a search across the index using the specified mode.
    pub fn search(
        &self,
        query_text: &str,
        limit: Option<usize>,
        source: Option<&str>,
        mode: SearchMode,
        query_kind: LexicalQueryKind,
        include_context: bool,
        context_lines: Option<usize>,
    ) -> Result<SearchOutcome, SearchError> {
        let limit = clamp_limit(limit, self.config.default_limit, self.config.max_limit);
        let symbol_query = symbol_query(query_text);
        let context_lines = clamp_context_lines(context_lines);
        let needs_cache = include_context || matches!(mode, SearchMode::Semantic | SearchMode::Hybrid);
        let mut related_defs = symbol_query.as_ref().map(|_| BTreeSet::new());
        let mut cache = if needs_cache {
            Some(DocCache::new(DOC_CACHE_CAPACITY))
        } else {
            None
        };
        let hits = match mode {
            SearchMode::Lexical => self.search_lexical(
                query_text,
                limit,
                source,
                query_kind,
                symbol_query.as_deref(),
                include_context,
                context_lines,
                related_defs.as_mut(),
                cache.as_mut(),
            ),
            SearchMode::Semantic => {
                if self.semantic.is_some() {
                    self.search_semantic(
                        query_text,
                        limit,
                        source,
                        symbol_query.as_deref(),
                        include_context,
                        context_lines,
                        related_defs.as_mut(),
                        cache.as_mut(),
                    )
                } else {
                    self.search_lexical(
                        query_text,
                        limit,
                        source,
                        query_kind,
                        symbol_query.as_deref(),
                        include_context,
                        context_lines,
                        related_defs.as_mut(),
                        cache.as_mut(),
                    )
                }
            }
            SearchMode::Hybrid => {
                if self.semantic.is_some() {
                    self.search_hybrid(
                        query_text,
                        limit,
                        source,
                        query_kind,
                        symbol_query.as_deref(),
                        include_context,
                        context_lines,
                        related_defs.as_mut(),
                        cache.as_mut(),
                    )
                } else {
                    self.search_lexical(
                        query_text,
                        limit,
                        source,
                        query_kind,
                        symbol_query.as_deref(),
                        include_context,
                        context_lines,
                        related_defs.as_mut(),
                        cache.as_mut(),
                    )
                }
            }
        }?;

        let related_defs = if let (Some(symbol), Some(set)) = (symbol_query.as_deref(), related_defs)
        {
            finalize_related_defs(set, symbol)
        } else {
            Vec::new()
        };

        Ok(SearchOutcome {
            hits,
            related_defs,
        })
    }

    /// Return a summary of all indexed corpus sources.
    pub fn list_sources(&self) -> Vec<SourceSummary> {
        self.sources
            .read()
            .expect("search sources lock poisoned")
            .clone()
    }

    pub fn index_metadata(&self) -> IndexMetadata {
        let mut metadata = self
            .index_metadata
            .read()
            .expect("search metadata lock poisoned")
            .clone();
        metadata.indexed_at_unix_ms =
            index_timestamp_unix_ms(&self.index_dir).or(metadata.indexed_at_unix_ms);
        metadata
    }

    pub fn local_freshness_report(&self) -> Result<LocalFreshnessReport, SearchError> {
        let indexed_at = self.index_metadata().indexed_at_unix_ms;
        let mut sources = Vec::new();

        for mount in &self.corpus_mounts {
            if !mount.label.starts_with("local-") {
                continue;
            }
            let mut latest_mtime: Option<u64> = None;
            let mut file_count = 0usize;
            for_each_corpus_file(
                std::slice::from_ref(mount),
                self.config.max_file_bytes,
                |_mount, _path, metadata| {
                    file_count += 1;
                    if let Ok(modified) = metadata.modified() {
                        if let Some(unix_ms) = system_time_to_unix_ms(modified) {
                            latest_mtime = Some(latest_mtime.map_or(unix_ms, |current| current.max(unix_ms)));
                        }
                    }
                    Ok(WalkOutcome::Continue)
                },
            )?;

            let stale = match (indexed_at, latest_mtime) {
                (Some(index_ms), Some(latest_ms)) => latest_ms > index_ms,
                (None, Some(_)) => true,
                _ => false,
            };
            let staleness_ms = match (indexed_at, latest_mtime) {
                (Some(index_ms), Some(latest_ms)) if latest_ms > index_ms => Some(latest_ms.saturating_sub(index_ms)),
                _ => None,
            };
            sources.push(SourceFreshness {
                source: mount.label.clone(),
                indexed_at_unix_ms: indexed_at,
                latest_file_mtime_unix_ms: latest_mtime,
                stale,
                staleness_ms,
                file_count,
            });
        }

        let any_stale = sources.iter().any(|source| source.stale);
        Ok(LocalFreshnessReport {
            any_stale,
            indexed_at_unix_ms: indexed_at,
            sources,
        })
    }

    /// Trigger a lexical reindex without restarting the service.
    pub fn reindex_scoped(
        &self,
        requested_sources: &[String],
        workspace_paths: &[String],
        reason: &str,
    ) -> Result<ReindexReport, SearchError> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(SearchError::ReindexScope(
                "reason must not be empty".to_string(),
            ));
        }
        let _lock = self
            .reindex_lock
            .try_lock()
            .map_err(|_| SearchError::ReindexBusy)?;
        let effective_sources = self.resolve_reindex_sources(requested_sources, workspace_paths)?;

        let scan_at_unix_ms = now_unix_ms();
        let scanned_sources = scan_sources(&self.corpus_mounts, &self.config)?;
        let scan_file_count = scanned_sources
            .iter()
            .map(|source| source.file_count)
            .sum::<usize>();
        let scan_total_bytes = scanned_sources
            .iter()
            .map(|source| source.total_bytes)
            .sum::<u64>();

        build_index(&self._index, &self.corpus_mounts, &self.fields, &self.config)?;
        self.reader.reload()?;

        let mut metadata = build_index_metadata(&self.index_dir, &scanned_sources);
        metadata.indexed_at_unix_ms = index_timestamp_unix_ms(&self.index_dir);
        {
            let mut sources_guard = self.sources.write().map_err(|_| {
                SearchError::ReindexState("sources lock poisoned".to_string())
            })?;
            *sources_guard = scanned_sources;
        }
        {
            let mut metadata_guard = self.index_metadata.write().map_err(|_| {
                SearchError::ReindexState("index metadata lock poisoned".to_string())
            })?;
            *metadata_guard = metadata.clone();
        }

        Ok(ReindexReport {
            requested_sources: requested_sources.to_vec(),
            effective_sources,
            workspace_paths: workspace_paths.to_vec(),
            reason: reason.to_string(),
            indexed_at_unix_ms: metadata.indexed_at_unix_ms,
            scan_at_unix_ms,
            scan_file_count,
            scan_total_bytes,
        })
    }

    fn resolve_reindex_sources(
        &self,
        requested_sources: &[String],
        workspace_paths: &[String],
    ) -> Result<Vec<String>, SearchError> {
        let sources = self.list_sources();
        let mut known = HashMap::new();
        for source in &sources {
            known.insert(source.source.to_ascii_lowercase(), source.source.clone());
        }

        let mut selected = Vec::new();
        if requested_sources.is_empty() {
            selected.extend(
                sources
                    .iter()
                    .filter(|source| source.source.starts_with("local-"))
                    .map(|source| source.source.clone()),
            );
        } else {
            for raw in requested_sources {
                let source = raw.trim();
                if source.is_empty() {
                    continue;
                }
                if source.eq_ignore_ascii_case("local") {
                    selected.extend(
                        sources
                            .iter()
                            .filter(|source| source.source.starts_with("local-"))
                            .map(|source| source.source.clone()),
                    );
                    continue;
                }
                let key = source.to_ascii_lowercase();
                let Some(canonical) = known.get(&key) else {
                    return Err(SearchError::ReindexScope(format!(
                        "unknown source label: {key}"
                    )));
                };
                selected.push(canonical.clone());
            }
        }
        selected.sort();
        selected.dedup();

        if selected.is_empty() {
            return Err(SearchError::ReindexScope(
                "no eligible sources selected".to_string(),
            ));
        }
        self.validate_workspace_paths(workspace_paths)?;
        Ok(selected)
    }

    fn validate_workspace_paths(&self, workspace_paths: &[String]) -> Result<(), SearchError> {
        if workspace_paths.is_empty() {
            return Ok(());
        }
        let Some(workspace_root) = self.workspace_root() else {
            return Err(SearchError::ReindexScope(
                "workspace_paths require a configured local workspace mount".to_string(),
            ));
        };
        let workspace_root = workspace_root.canonicalize()?;
        for raw in workspace_paths {
            let path = raw.trim();
            if path.is_empty() {
                return Err(SearchError::ReindexScope(
                    "workspace_paths must not contain empty entries".to_string(),
                ));
            }
            let rel = Path::new(path);
            if rel.is_absolute() {
                return Err(SearchError::ReindexScope(format!(
                    "workspace_paths must be relative (got {path})"
                )));
            }
            if rel.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            }) {
                return Err(SearchError::ReindexScope(format!(
                    "workspace_paths must not contain '..' or absolute prefixes (got {path})"
                )));
            }
            let candidate = workspace_root.join(rel);
            let canonical = candidate.canonicalize().map_err(|err| {
                SearchError::ReindexScope(format!(
                    "failed to canonicalize workspace path {path}: {err}"
                ))
            })?;
            if !canonical.starts_with(&workspace_root) {
                return Err(SearchError::ReindexScope(format!(
                    "workspace path escapes configured workspace root: {path}"
                )));
            }
        }
        Ok(())
    }

    pub fn workspace_root(&self) -> Option<PathBuf> {
        for mount in &self.corpus_mounts {
            if !mount.label.starts_with("local-") {
                continue;
            }
            if let Some(parent) = mount.root.parent() {
                return Some(parent.to_path_buf());
            }
        }
        None
    }

    fn resolve_doc_path(&self, doc_id: &str) -> Result<Option<ResolvedDoc<'_>>, SearchError> {
        let doc_id = doc_id.trim();
        if doc_id.is_empty() {
            return Ok(None);
        }

        let mut parts = doc_id.splitn(2, '/');
        let first = parts.next().unwrap_or("");
        let mut mount = None;
        let mut relative = doc_id;

        if !first.is_empty() {
            for candidate in &self.corpus_mounts {
                if candidate.label.is_empty() {
                    continue;
                }
                if candidate.label == first {
                    let rest = parts.next().unwrap_or("");
                    if rest.is_empty() {
                        return Ok(None);
                    }
                    mount = Some(candidate);
                    relative = rest;
                    break;
                }
            }
        }

        let mount = match mount {
            Some(mount) => mount,
            None => self
                .corpus_mounts
                .iter()
                .find(|candidate| candidate.label.is_empty())
                .expect("default corpus mount"),
        };

        let candidate = mount.root.join(relative);
        if !candidate.exists() {
            return Ok(None);
        }

        let canonical = candidate.canonicalize()?;
        if !canonical.starts_with(&mount.root) {
            return Err(SearchError::InvalidDocId(doc_id.to_string()));
        }

        Ok(Some(ResolvedDoc { mount, canonical }))
    }

    /// Retrieve the full content of a document.
    ///
    /// # Security
    /// * **Traversal Check**: Ensures the `doc_id` does not point outside allowed corpus roots.
    pub fn get_doc(
        &self,
        doc_id: &str,
        max_chars: Option<usize>,
    ) -> Result<Option<DocumentContent>, SearchError> {
        let resolved = match self.resolve_doc_path(doc_id)? {
            Some(resolved) => resolved,
            None => return Ok(None),
        };

        let metadata = resolved.canonical.metadata()?;
        if metadata.len() > self.config.max_file_bytes {
            return Err(SearchError::InvalidDocId(format!(
                "document too large ({} bytes)",
                metadata.len()
            )));
        }

        let text = match read_text(&resolved.canonical) {
            Some(text) => text,
            None => return Ok(None),
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            return Ok(None);
        }

        let char_len = text.chars().count();
        let max_chars = clamp_doc_chars(max_chars);
        let truncated = char_len > max_chars;
        let content = if truncated {
            text.chars().take(max_chars).collect()
        } else {
            text.clone()
        };

        let meta = document_meta(resolved.mount, &resolved.canonical);
        Ok(Some(DocumentContent {
            doc_id: meta.id,
            path: resolved.canonical.to_string_lossy().to_string(),
            title: meta.title,
            source: meta.source,
            byte_len: metadata.len(),
            char_len,
            truncated,
            content,
        }))
    }

    /// Retrieve a specific chunk of a document.
    pub fn get_chunk(
        &self,
        doc_id: &str,
        chunk_index: u64,
    ) -> Result<Option<ChunkContent>, SearchError> {
        let resolved = match self.resolve_doc_path(doc_id)? {
            Some(resolved) => resolved,
            None => return Ok(None),
        };

        let metadata = resolved.canonical.metadata()?;
        if metadata.len() > self.config.max_file_bytes {
            return Err(SearchError::InvalidDocId(format!(
                "document too large ({} bytes)",
                metadata.len()
            )));
        }

        let text = match read_text(&resolved.canonical) {
            Some(text) => text,
            None => return Ok(None),
        };
        let text = text.trim();
        if text.is_empty() {
            return Ok(None);
        }

        let chunks = chunk_text(text, self.config.chunk_config);
        let idx = chunk_index as usize;
        if idx >= chunks.len() {
            return Ok(None);
        }
        let chunk = &chunks[idx];

        let meta = document_meta(resolved.mount, &resolved.canonical);
        let provenance = provenance_from_source(meta.source.as_deref());
        Ok(Some(ChunkContent {
            doc_id: meta.id,
            path: resolved.canonical.to_string_lossy().to_string(),
            title: meta.title,
            source: meta.source,
            chunk_index: chunk.index as u64,
            start: chunk.start,
            end: chunk.end,
            content: chunk.text.clone(),
            provenance,
        }))
    }

    /// Resolve lexical hover context at a file position.
    pub fn hover(
        &self,
        file: &str,
        line: u32,
        column: u32,
        symbol: Option<&str>,
        include_context: bool,
        context_lines: Option<usize>,
    ) -> Result<Option<HoverResult>, SearchError> {
        let doc_id = match self.resolve_hover_doc_id(file)? {
            Some(doc_id) => doc_id,
            None => return Ok(None),
        };
        let cached = match self.load_cached_doc(&doc_id)? {
            Some(cached) => cached,
            None => return Ok(None),
        };
        if cached.chunks.is_empty() || cached.lines.is_empty() {
            return Ok(None);
        }

        let clamped_line = line.clamp(1, cached.lines.len() as u32);
        let clamped_column = column.max(1);
        let line_idx = clamped_line.saturating_sub(1) as usize;
        let offset =
            offset_for_position(&cached.lines, &cached.line_starts, line_idx, clamped_column);
        let Some(chunk) = chunk_for_offset(&cached.chunks, offset) else {
            return Ok(None);
        };

        let resolved_symbol =
            normalize_hover_symbol(symbol).or_else(|| infer_symbol_from_line(
                &cached.lines[line_idx],
                clamped_column,
            ));
        let symbol_match = resolved_symbol
            .as_deref()
            .and_then(|value| find_symbol_match(&chunk.text, value));
        let related_defs = if let Some(value) = resolved_symbol.as_deref() {
            let mut defs = BTreeSet::new();
            collect_related_defs(&chunk.text, value, Some(&mut defs));
            finalize_related_defs(defs, value)
        } else {
            Vec::new()
        };
        let context_lines = clamp_context_lines(context_lines);
        let context = if include_context {
            build_line_context_from_cached(
                &cached.lines,
                &cached.line_starts,
                offset,
                offset.saturating_add(1),
                context_lines,
            )
        } else {
            None
        };
        let snippet_query = resolved_symbol
            .as_deref()
            .unwrap_or_else(|| cached.lines[line_idx].as_str());
        let snippet = make_snippet(&chunk.text, snippet_query, self.config.snippet_max_chars);
        let provenance = provenance_from_source(cached.meta.source.as_deref());

        Ok(Some(HoverResult {
            doc_id: cached.meta.id.clone(),
            path: cached.meta.path.clone(),
            line: clamped_line,
            column: clamped_column,
            symbol: resolved_symbol,
            symbol_match,
            chunk_index: chunk.index as u64,
            snippet,
            context,
            related_defs,
            provenance,
        }))
    }

    fn resolve_hover_doc_id(&self, file: &str) -> Result<Option<String>, SearchError> {
        self.resolve_tool_file_doc_id(file)
    }

    /// Normalize a file input (doc_id, absolute path, repo-relative path) and resolve to doc_id.
    ///
    /// This helper is intended for reuse by file-based tools that need consistent path handling.
    fn resolve_tool_file_doc_id(&self, file: &str) -> Result<Option<String>, SearchError> {
        let file = file.trim();
        if file.is_empty() {
            return Ok(None);
        }

        let input = PathBuf::from(file);
        if input.is_absolute() {
            return self
                .resolve_existing_file_candidates(vec![input], file)
                .map(|resolved| {
                    resolved.map(|doc| document_meta(doc.mount, &doc.canonical).id)
                });
        }

        if let Some(resolved) = self.resolve_doc_path(file)? {
            return Ok(Some(document_meta(resolved.mount, &resolved.canonical).id));
        }

        self.resolve_existing_file_candidates(hover_file_candidates(self, &input), file)
            .map(|resolved| resolved.map(|doc| document_meta(doc.mount, &doc.canonical).id))
    }

    fn resolve_existing_file_candidates<'a>(
        &'a self,
        candidates: Vec<PathBuf>,
        raw_input: &str,
    ) -> Result<Option<ResolvedDoc<'a>>, SearchError> {
        let mut saw_outside_root = false;
        for candidate in candidates {
            if !candidate.exists() {
                continue;
            }
            let canonical = match candidate.canonicalize() {
                Ok(path) => path,
                Err(_) => continue,
            };
            if let Some(mount) = self
                .corpus_mounts
                .iter()
                .find(|mount| canonical.starts_with(&mount.root))
            {
                return Ok(Some(ResolvedDoc { mount, canonical }));
            }
            saw_outside_root = true;
        }

        if saw_outside_root {
            return Err(SearchError::InvalidDocId(format!(
                "path resolves outside indexed corpus mounts: {raw_input}"
            )));
        }

        Ok(None)
    }

    fn load_cached_doc(&self, doc_id: &str) -> Result<Option<CachedDoc>, SearchError> {
        let resolved = match self.resolve_doc_path(doc_id)? {
            Some(resolved) => resolved,
            None => return Ok(None),
        };

        let metadata = resolved.canonical.metadata()?;
        if metadata.len() > self.config.max_file_bytes {
            return Err(SearchError::InvalidDocId(format!(
                "document too large ({} bytes)",
                metadata.len()
            )));
        }

        let raw = match read_text(&resolved.canonical) {
            Some(text) => text,
            None => return Ok(None),
        };
        let text = raw.trim();
        if text.is_empty() {
            return Ok(None);
        }

        let chunks = chunk_text(text, self.config.chunk_config);
        if chunks.is_empty() {
            return Ok(None);
        }
        let lines = text.lines().map(|line| line.to_string()).collect();
        let line_starts = compute_line_starts(text);
        let meta = document_meta(resolved.mount, &resolved.canonical);
        Ok(Some(CachedDoc {
            chunks,
            meta,
            lines,
            line_starts,
        }))
    }

    fn get_chunk_cached(
        &self,
        cache: &mut DocCache,
        doc_id: &str,
        chunk_index: u64,
        include_context: bool,
        context_lines: usize,
    ) -> Result<Option<CachedChunk>, SearchError> {
        let cached = match cache.get_or_load(doc_id, || self.load_cached_doc(doc_id))? {
            Some(cached) => cached,
            None => return Ok(None),
        };
        let idx = chunk_index as usize;
        if idx >= cached.chunks.len() {
            return Ok(None);
        }
        let chunk = &cached.chunks[idx];
        let context = if include_context {
            build_line_context_from_cached(
                &cached.lines,
                &cached.line_starts,
                chunk.start,
                chunk.end,
                context_lines,
            )
        } else {
            None
        };
        let provenance = provenance_from_source(cached.meta.source.as_deref());
        Ok(Some(CachedChunk {
            doc_id: cached.meta.id.clone(),
            path: cached.meta.path.clone(),
            title: cached.meta.title.clone(),
            source: cached.meta.source.clone(),
            chunk_index: chunk.index as u64,
            content: chunk.text.clone(),
            context,
            provenance,
        }))
    }

    pub fn semantic_available(&self) -> bool {
        self.semantic.is_some()
    }

    pub fn build_semantic(
        corpus_dir: &Path,
        config: &SearchConfig,
        reindex: bool,
        extra_mounts: Vec<CorpusMountConfig>,
    ) -> Result<(), SearchError> {
        let mounts = build_mounts(corpus_dir, extra_mounts);
        SemanticIndex::build_only(&mounts, config, reindex)
    }

    pub fn locate_symbol(
        &self,
        symbol: &str,
        limit: usize,
        source: Option<&str>,
        kind: SymbolMatchKind,
        include_context: bool,
        context_lines: Option<usize>,
    ) -> Result<Vec<SymbolOccurrence>, SearchError> {
        collect_symbol_matches(
            &self.corpus_mounts,
            symbol,
            limit,
            source,
            kind,
            include_context,
            context_lines,
            self.config.max_file_bytes,
        )
    }

    pub fn refs_symbol(
        &self,
        symbol: &str,
        limit: usize,
        source: Option<&str>,
        include_context: bool,
        context_lines: Option<usize>,
    ) -> Result<Vec<SymbolOccurrence>, SearchError> {
        collect_symbol_matches(
            &self.corpus_mounts,
            symbol,
            limit,
            source,
            SymbolMatchKind::Reference,
            include_context,
            context_lines,
            self.config.max_file_bytes,
        )
    }

    fn build_source_filter_query(&self, source: &str) -> Box<dyn Query> {
        let filters = split_source_filter(source);
        if filters.is_empty() {
            let term = Term::from_field_text(self.fields.source, source);
            return Box::new(TermQuery::new(term, IndexRecordOption::Basic));
        }
        if filters.len() == 1 {
            return self.build_single_source_filter_query(&filters[0]);
        }
        let clauses: Vec<(Occur, Box<dyn Query>)> = filters
            .iter()
            .map(|filter| (Occur::Should, self.build_single_source_filter_query(filter)))
            .collect();
        Box::new(BooleanQuery::from(clauses))
    }

    fn build_single_source_filter_query(&self, source: &str) -> Box<dyn Query> {
        if source.eq_ignore_ascii_case("local") {
            let locals: Vec<String> = self
                .corpus_mounts
                .iter()
                .filter_map(|mount| {
                    if mount.label.starts_with("local-") {
                        Some(mount.label.clone())
                    } else {
                        None
                    }
                })
                .collect();

            if locals.is_empty() {
                let term = Term::from_field_text(self.fields.source, "local");
                return Box::new(TermQuery::new(term, IndexRecordOption::Basic));
            }
            if locals.len() == 1 {
                let term = Term::from_field_text(self.fields.source, &locals[0]);
                return Box::new(TermQuery::new(term, IndexRecordOption::Basic));
            }
            let clauses: Vec<(Occur, Box<dyn Query>)> = locals
                .iter()
                .map(|label| {
                    let term = Term::from_field_text(self.fields.source, label);
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)) as Box<dyn Query>,
                    )
                })
                .collect();
            return Box::new(BooleanQuery::from(clauses));
        }

        let term = Term::from_field_text(self.fields.source, source);
        Box::new(TermQuery::new(term, IndexRecordOption::Basic))
    }

    fn parse_query(
        &self,
        query_text: &str,
        kind: LexicalQueryKind,
    ) -> Result<Box<dyn Query>, SearchError> {
        match kind {
            LexicalQueryKind::Literal => self.parse_query_literal(query_text),
            LexicalQueryKind::Tantivy => Ok(self.query_parser.parse_query(query_text)?),
        }
    }

    fn parse_query_literal(&self, query_text: &str) -> Result<Box<dyn Query>, SearchError> {
        const MAX_TOKENS: usize = 32;
        const MAX_TOKEN_BYTES: usize = 64;

        let tokens = self.tokenize_query_text(query_text, MAX_TOKENS);
        if tokens.is_empty() {
            return Ok(Box::new(EmptyQuery));
        }

        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(tokens.len());
        for token in tokens {
            if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
                continue;
            }
            let term_title = Term::from_field_text(self.fields.title, &token);
            let term_body = Term::from_field_text(self.fields.body, &token);
            let sub = BooleanQuery::from(vec![
                (
                    Occur::Should,
                    Box::new(TermQuery::new(term_title, IndexRecordOption::WithFreqs))
                        as Box<dyn Query>,
                ),
                (
                    Occur::Should,
                    Box::new(TermQuery::new(term_body, IndexRecordOption::WithFreqs))
                        as Box<dyn Query>,
                ),
            ]);
            clauses.push((Occur::Should, Box::new(sub) as Box<dyn Query>));
        }

        if clauses.is_empty() {
            return Ok(Box::new(EmptyQuery));
        }

        Ok(Box::new(BooleanQuery::from(clauses)))
    }

    fn tokenize_query_text(&self, query_text: &str, max_tokens: usize) -> Vec<String> {
        let tokenizer = tokenizer_for_field(&self._index, self.fields.body)
            .or_else(|| tokenizer_for_field(&self._index, self.fields.title))
            .or_else(|| self._index.tokenizers().get("default"));

        let Some(mut tokenizer) = tokenizer else {
            return query_text
                .split_whitespace()
                .filter(|token| !token.is_empty())
                .take(max_tokens)
                .map(|token| token.to_ascii_lowercase())
                .collect();
        };

        let mut tokens = Vec::new();
        let mut seen = HashSet::new();
        let mut stream = tokenizer.token_stream(query_text);
        while tokens.len() < max_tokens && stream.advance() {
            let text = stream.token().text.trim();
            if text.is_empty() {
                continue;
            }
            if !seen.insert(text.to_string()) {
                continue;
            }
            tokens.push(text.to_string());
        }
        tokens
    }

    fn search_lexical(
        &self,
        query_text: &str,
        limit: usize,
        source: Option<&str>,
        query_kind: LexicalQueryKind,
        symbol_query: Option<&str>,
        include_context: bool,
        context_lines: usize,
        mut related_defs: Option<&mut BTreeSet<String>>,
        mut cache: Option<&mut DocCache>,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let query = self.parse_query(query_text, query_kind)?;
        let query = if let Some(source) = source {
            let source_query = self.build_source_filter_query(source);
            let clauses: Vec<(Occur, Box<dyn Query>)> = vec![
                (Occur::Must, query.box_clone()),
                (Occur::Must, source_query),
            ];
            BooleanQuery::from(clauses)
        } else {
            BooleanQuery::from(vec![(Occur::Must, query.box_clone())])
        };

        let searcher = self.reader.searcher();
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut hits = Vec::with_capacity(top_docs.len());

        for (score, addr) in top_docs {
            let doc = searcher.doc(addr)?;
            let doc_id = extract_text(&doc, self.fields.doc_id);
            let mut path = extract_text(&doc, self.fields.path);
            let mut title = extract_text_opt(&doc, self.fields.title);
            let mut source_val = extract_text_opt(&doc, self.fields.source);
            let chunk_index = extract_u64(&doc, self.fields.chunk_index);
            let body = extract_text(&doc, self.fields.body);
            let mut snippet = make_snippet(&body, query_text, self.config.snippet_max_chars);
            let mut context = None;
            let mut symbol_match = None;

            if include_context {
                if let Some(cache) = cache.as_deref_mut() {
                    if let Ok(Some(chunk)) = self.get_chunk_cached(
                        cache,
                        &doc_id,
                        chunk_index,
                        include_context,
                        context_lines,
                    ) {
                        path = chunk.path;
                        title = chunk.title;
                        source_val = chunk.source;
                        snippet = make_snippet(
                            &chunk.content,
                            query_text,
                            self.config.snippet_max_chars,
                        );
                        context = chunk.context;
                        if let Some(symbol) = symbol_query {
                            symbol_match = find_symbol_match(&chunk.content, symbol);
                        }
                        if let Some(symbol) = symbol_query {
                            collect_related_defs(&chunk.content, symbol, related_defs.as_deref_mut());
                        }
                    }
                }
            }
            if symbol_match.is_none() {
                if let Some(symbol) = symbol_query {
                    symbol_match = find_symbol_match(&body, symbol);
                }
            }
            if let Some(symbol) = symbol_query {
                collect_related_defs(&body, symbol, related_defs.as_deref_mut());
            }
            let provenance = provenance_from_source(source_val.as_deref());

            hits.push(SearchHit {
                doc_id,
                path,
                title,
                source: source_val,
                score,
                snippet,
                chunk_index,
                context,
                provenance,
                symbol: symbol_match,
            });
        }

        Ok(hits)
    }

    fn search_semantic_matches(
        &self,
        query_text: &str,
        limit: usize,
        source: Option<&str>,
    ) -> Result<Vec<SemanticMatch>, SearchError> {
        let Some(semantic) = self.semantic.as_ref() else {
            return Ok(Vec::new());
        };
        let query_embedding = semantic.embed_query(query_text)?;
        let min_score = self.config.semantic.min_score.clamp(-1.0, 1.0);
        let matches = semantic.search(&query_embedding, limit, source, min_score);
        Ok(matches)
    }

    fn search_semantic(
        &self,
        query_text: &str,
        limit: usize,
        source: Option<&str>,
        symbol_query: Option<&str>,
        include_context: bool,
        context_lines: usize,
        related_defs: Option<&mut BTreeSet<String>>,
        cache: Option<&mut DocCache>,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let matches = self.search_semantic_matches(query_text, limit, source)?;
        self.semantic_matches_to_hits(
            matches,
            query_text,
            symbol_query,
            include_context,
            context_lines,
            related_defs,
            cache,
        )
    }

    fn search_hybrid(
        &self,
        query_text: &str,
        limit: usize,
        source: Option<&str>,
        query_kind: LexicalQueryKind,
        symbol_query: Option<&str>,
        include_context: bool,
        context_lines: usize,
        mut related_defs: Option<&mut BTreeSet<String>>,
        mut cache: Option<&mut DocCache>,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let lexical = self.search_lexical(
            query_text,
            limit,
            source,
            query_kind,
            symbol_query,
            include_context,
            context_lines,
            related_defs.as_deref_mut(),
            cache.as_deref_mut(),
        )?;
        let semantic_limit = limit.max(self.config.semantic.top_k);
        let semantic_matches = self.search_semantic_matches(query_text, semantic_limit, source)?;
        let semantic_hits = self.semantic_matches_to_hits(
            semantic_matches,
            query_text,
            symbol_query,
            include_context,
            context_lines,
            related_defs,
            cache.as_deref_mut(),
        )?;

        let max_lex = lexical
            .iter()
            .map(|hit| hit.score)
            .fold(0.0_f32, f32::max);
        let max_sem = semantic_hits
            .iter()
            .map(|hit| hit.score)
            .fold(0.0_f32, f32::max);
        let semantic_weight = self.config.semantic.weight.clamp(0.0, 1.0);
        let lexical_weight = 1.0 - semantic_weight;

        struct CombinedHit {
            hit: SearchHit,
            lex_score: f32,
            sem_score: f32,
        }

        let mut combined: HashMap<(String, u64), CombinedHit> = HashMap::new();

        for hit in lexical {
            let score = hit.score;
            let key = (hit.doc_id.clone(), hit.chunk_index);
            combined.insert(
                key,
                CombinedHit {
                    hit,
                    lex_score: score,
                    sem_score: 0.0,
                },
            );
        }

        for hit in semantic_hits {
            let score = hit.score;
            let key = (hit.doc_id.clone(), hit.chunk_index);
            combined
                .entry(key)
                .and_modify(|existing| {
                    existing.sem_score = score;
                })
                .or_insert(CombinedHit {
                    hit,
                    lex_score: 0.0,
                    sem_score: score,
                });
        }

        let mut hits: Vec<SearchHit> = combined
            .into_values()
            .map(|mut combined| {
                let lex_norm = if max_lex > 0.0 {
                    combined.lex_score / max_lex
                } else {
                    0.0
                };
                let sem_norm = if max_sem > 0.0 {
                    combined.sem_score / max_sem
                } else {
                    0.0
                };
                combined.hit.score = lex_norm * lexical_weight + sem_norm * semantic_weight;
                combined.hit
            })
            .collect();

        hits.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
        });
        hits.truncate(limit);
        Ok(hits)
    }

    fn semantic_matches_to_hits(
        &self,
        matches: Vec<SemanticMatch>,
        query_text: &str,
        symbol_query: Option<&str>,
        include_context: bool,
        context_lines: usize,
        mut related_defs: Option<&mut BTreeSet<String>>,
        mut cache: Option<&mut DocCache>,
    ) -> Result<Vec<SearchHit>, SearchError> {
        let mut hits = Vec::with_capacity(matches.len());
        for matched in matches {
            let chunk = if let Some(cache) = cache.as_deref_mut() {
                match self.get_chunk_cached(
                    cache,
                    &matched.doc_id,
                    matched.chunk_index,
                    include_context,
                    context_lines,
                )? {
                    Some(chunk) => chunk,
                    None => continue,
                }
            } else {
                match self.get_chunk(&matched.doc_id, matched.chunk_index)? {
                    Some(chunk) => {
                        let provenance = provenance_from_source(chunk.source.as_deref());
                        CachedChunk {
                            doc_id: chunk.doc_id,
                            path: chunk.path,
                            title: chunk.title,
                            source: chunk.source,
                            chunk_index: chunk.chunk_index,
                            content: chunk.content,
                            context: None,
                            provenance,
                        }
                    }
                    None => continue,
                }
            };
            let snippet = make_snippet(&chunk.content, query_text, self.config.snippet_max_chars);
            let symbol_match = symbol_query.and_then(|symbol| find_symbol_match(&chunk.content, symbol));
            if let Some(symbol) = symbol_query {
                collect_related_defs(&chunk.content, symbol, related_defs.as_deref_mut());
            }
            hits.push(SearchHit {
                doc_id: chunk.doc_id,
                path: chunk.path,
                title: chunk.title,
                source: chunk.source,
                score: matched.score,
                snippet,
                chunk_index: chunk.chunk_index,
                context: chunk.context,
                provenance: chunk.provenance,
                symbol: symbol_match,
            });
        }
        Ok(hits)
    }
}

fn tokenizer_for_field(index: &Index, field: Field) -> Option<TextAnalyzer> {
    let schema = index.schema();
    let entry = schema.get_field_entry(field);
    let FieldType::Str(text_options) = entry.field_type() else {
        return None;
    };
    let indexing = text_options.get_indexing_options()?;
    index.tokenizers().get(indexing.tokenizer())
}

fn hover_file_candidates(index: &SearchIndex, input: &Path) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if input.is_absolute() {
        add_unique_path(&mut candidates, input.to_path_buf());
    } else {
        if let Ok(cwd) = std::env::current_dir() {
            add_unique_path(&mut candidates, cwd.join(input));
        }
        if let Some(workspace_root) = index.workspace_root() {
            add_unique_path(&mut candidates, workspace_root.join(input));
        }
        for mount in &index.corpus_mounts {
            add_unique_path(&mut candidates, mount.root.join(input));
        }
    }
    if candidates.is_empty() {
        add_unique_path(&mut candidates, input.to_path_buf());
    }
    candidates
}

fn add_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|existing| existing == &candidate) {
        paths.push(candidate);
    }
}

fn normalize_hover_symbol(symbol: Option<&str>) -> Option<String> {
    symbol
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn infer_symbol_from_line(line: &str, column: u32) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }

    let mut col = column.saturating_sub(1) as usize;
    col = col.min(chars.len().saturating_sub(1));
    if !is_hover_ident_char(chars[col]) && col > 0 && is_hover_ident_char(chars[col - 1]) {
        col -= 1;
    }
    if !is_hover_ident_char(chars[col]) {
        return None;
    }

    let mut start = col;
    while start > 0 && is_hover_ident_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && is_hover_ident_char(chars[end]) {
        end += 1;
    }
    if start >= end {
        return None;
    }

    let raw: String = chars[start..end].iter().collect();
    let trimmed = raw.trim_matches('.');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn is_hover_ident_char(ch: char) -> bool {
    is_ident_char(ch) || ch == '.'
}

fn offset_for_position(
    lines: &[String],
    line_starts: &[usize],
    line_idx: usize,
    column: u32,
) -> usize {
    let line_start = line_starts.get(line_idx).copied().unwrap_or(0);
    let line_len = lines
        .get(line_idx)
        .map(|line| line.chars().count())
        .unwrap_or(0);
    let col = (column.saturating_sub(1) as usize).min(line_len);
    line_start + col
}

fn chunk_for_offset<'a>(
    chunks: &'a [mcp_toolkit_docs::Chunk],
    offset: usize,
) -> Option<&'a mcp_toolkit_docs::Chunk> {
    let mut fallback = None;
    for chunk in chunks {
        if offset >= chunk.start {
            fallback = Some(chunk);
        }
        if offset >= chunk.start && offset < chunk.end {
            return Some(chunk);
        }
    }
    fallback.or_else(|| chunks.first())
}

#[derive(Debug)]
struct CachedDoc {
    chunks: Vec<mcp_toolkit_docs::Chunk>,
    meta: DocumentMeta,
    lines: Vec<String>,
    line_starts: Vec<usize>,
}

#[derive(Debug, Clone)]
struct CachedChunk {
    doc_id: String,
    path: String,
    title: Option<String>,
    source: Option<String>,
    chunk_index: u64,
    content: String,
    context: Option<LineContext>,
    provenance: Option<Provenance>,
}

#[derive(Debug)]
struct DocCache {
    capacity: usize,
    order: VecDeque<String>,
    entries: HashMap<String, CachedDoc>,
}

impl DocCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            order: VecDeque::new(),
            entries: HashMap::new(),
        }
    }

    fn get_or_load<F>(
        &mut self,
        key: &str,
        loader: F,
    ) -> Result<Option<&CachedDoc>, SearchError>
    where
        F: FnOnce() -> Result<Option<CachedDoc>, SearchError>,
    {
        if self.entries.contains_key(key) {
            self.touch(key);
            return Ok(self.entries.get(key));
        }
        let Some(entry) = loader()? else {
            return Ok(None);
        };
        self.insert(key.to_string(), entry);
        Ok(self.entries.get(key))
    }

    fn insert(&mut self, key: String, value: CachedDoc) {
        if self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(key.clone());
        self.entries.insert(key, value);
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|item| item == key) {
            self.order.remove(pos);
        }
        self.order.push_back(key.to_string());
    }
}

impl SemanticIndex {
    fn open_or_build(
        mounts: &[CorpusMount],
        config: &SearchConfig,
        reindex: bool,
    ) -> Result<Option<Self>, SearchError> {
        let semantic = &config.semantic;
        let model = parse_embedding_model(&semantic.model)?;
        tracing::info!(
            model = %sanitize_log_value(&semantic.model),
            "initializing embedding model"
        );
        let mut options = InitOptions::new(model);
        if let Some(cache_dir) = &semantic.cache_dir {
            options = options.with_cache_dir(cache_dir.clone());
        }
        let mut embedder = TextEmbedding::try_new(options)?;
        tracing::info!("embedding model ready");

        if reindex && semantic.index_dir.exists() {
            fs::remove_dir_all(&semantic.index_dir)?;
        }
        fs::create_dir_all(&semantic.index_dir)?;

        let meta_path = semantic.index_dir.join("meta.json");
        let data_path = semantic.index_dir.join("embeddings.bin");

        let mut meta: Option<SemanticMeta> = None;
        let mut records = Vec::new();
        let mut dim = 0usize;
        let mut needs_build = reindex;
        let mut embeddings_built = false;

        if !needs_build && meta_path.exists() && data_path.exists() {
            let meta_raw = fs::read_to_string(&meta_path)?;
            let parsed: SemanticMeta = serde_json::from_str(&meta_raw)?;
            if meta_matches(&parsed, config, semantic) {
                records = load_embeddings(&data_path)?;
                for record in records.iter_mut() {
                    normalize_embedding(&mut record.embedding).map_err(|err| {
                        SearchError::SemanticConfig(format!(
                            "invalid stored embedding for {} chunk {}: {err}",
                            record.doc_id, record.chunk_index
                        ))
                    })?;
                }
                if records.len() != parsed.records {
                    needs_build = true;
                } else {
                    dim = parsed.dim;
                    meta = Some(parsed);
                }
            } else {
                needs_build = true;
            }
        } else {
            needs_build = true;
        }

        if needs_build {
            if !semantic.build_on_start {
                tracing::warn!(
                    "semantic index missing or stale; set SPARK_MCP_SEMANTIC_BUILD_ON_START=1 or run spark-embed"
                );
                return Ok(None);
            }
            let (built, built_dim) = build_embeddings(mounts, config, &mut embedder)?;
            records = built;
            dim = built_dim;
            embeddings_built = true;
        }

        let mut hnsw_meta = None;
        let mut hnsw_index = None;
        if semantic.backend == SemanticBackend::Hnsw && !records.is_empty() {
            let expected_meta = HnswMeta {
                m: semantic.hnsw.m,
                ef_construction: semantic.hnsw.ef_construction,
            };
            let meta_matches = meta
                .as_ref()
                .and_then(|stored| stored.hnsw.as_ref())
                .map(|stored| hnsw_meta_matches(stored, &expected_meta))
                .unwrap_or(false);
            let files_exist = hnsw_files_exist(&semantic.index_dir);
            let needs_hnsw = reindex || embeddings_built || !files_exist || !meta_matches;
            if needs_hnsw {
                if semantic.build_on_start {
                    hnsw_index = Some(build_hnsw_index(
                        &records,
                        &semantic.hnsw,
                        &semantic.index_dir,
                    )?);
                    hnsw_meta = Some(expected_meta.clone());
                } else {
                    tracing::warn!(
                        "semantic HNSW index missing or stale; run spark-embed or enable SPARK_MCP_SEMANTIC_BUILD_ON_START"
                    );
                }
            }
            if hnsw_index.is_none() && files_exist && meta_matches {
                hnsw_index = Some(load_hnsw_index(&semantic.index_dir, semantic.hnsw.ef_search)?);
                hnsw_meta = Some(expected_meta.clone());
            }
            if let Some(loaded) = hnsw_index.as_ref() {
                if loaded.index.get_nb_point() != records.len() {
                    tracing::warn!(
                        hnsw_len = loaded.index.get_nb_point(),
                        records_len = records.len(),
                        "HNSW index size does not match embeddings; falling back to flat semantic search"
                    );
                    hnsw_index = None;
                }
            }
            if hnsw_index.is_none() {
                tracing::warn!(
                    "HNSW backend requested but index unavailable; falling back to flat semantic search"
                );
            }
        }

        let existing_hnsw = if hnsw_files_exist(&semantic.index_dir) {
            meta.as_ref().and_then(|stored| stored.hnsw.clone())
        } else {
            None
        };
        let persisted_hnsw = match semantic.backend {
            SemanticBackend::Hnsw => hnsw_meta.or(existing_hnsw),
            SemanticBackend::Flat => existing_hnsw,
        };
        let updated_meta = SemanticMeta {
            model: semantic.model.clone(),
            chunk_max_chars: config.chunk_config.max_chars,
            chunk_overlap: config.chunk_config.overlap,
            max_file_bytes: config.max_file_bytes,
            records: records.len(),
            dim,
            embedding_scale: EMBEDDING_SCALE,
            backend: semantic.backend.as_str().to_string(),
            hnsw: persisted_hnsw,
        };
        fs::write(&meta_path, serde_json::to_string_pretty(&updated_meta)?)?;
        if embeddings_built {
            save_embeddings(&data_path, &records)?;
        }

        Ok(Some(Self {
            records,
            dim,
            model: Mutex::new(embedder),
            backend: semantic.backend,
            hnsw: hnsw_index,
        }))
    }

    fn build_only(
        mounts: &[CorpusMount],
        config: &SearchConfig,
        reindex: bool,
    ) -> Result<(), SearchError> {
        let semantic = &config.semantic;
        let model = parse_embedding_model(&semantic.model)?;
        tracing::info!(
            model = %sanitize_log_value(&semantic.model),
            "initializing embedding model"
        );
        let mut options = InitOptions::new(model);
        if let Some(cache_dir) = &semantic.cache_dir {
            options = options.with_cache_dir(cache_dir.clone());
        }
        let mut embedder = TextEmbedding::try_new(options)?;
        tracing::info!("embedding model ready");

        if reindex && semantic.index_dir.exists() {
            fs::remove_dir_all(&semantic.index_dir)?;
        }
        fs::create_dir_all(&semantic.index_dir)?;

        let meta_path = semantic.index_dir.join("meta.json");
        let data_path = semantic.index_dir.join("embeddings.bin");

        let (records, dim) = build_embeddings(mounts, config, &mut embedder)?;

        let mut hnsw_meta = None;
        if semantic.backend == SemanticBackend::Hnsw && !records.is_empty() {
            let expected_meta = HnswMeta {
                m: semantic.hnsw.m,
                ef_construction: semantic.hnsw.ef_construction,
            };
            build_hnsw_index(&records, &semantic.hnsw, &semantic.index_dir)?;
            hnsw_meta = Some(expected_meta);
        }

        let meta = SemanticMeta {
            model: semantic.model.clone(),
            chunk_max_chars: config.chunk_config.max_chars,
            chunk_overlap: config.chunk_config.overlap,
            max_file_bytes: config.max_file_bytes,
            records: records.len(),
            dim,
            embedding_scale: EMBEDDING_SCALE,
            backend: semantic.backend.as_str().to_string(),
            hnsw: hnsw_meta,
        };
        fs::write(&meta_path, serde_json::to_string_pretty(&meta)?)?;
        save_embeddings(&data_path, &records)?;
        Ok(())
    }

    fn embed_query(&self, query: &str) -> Result<Vec<f32>, SearchError> {
        let query = format!("query: {}", query.trim());
        let mut model = self
            .model
            .lock()
            .map_err(|_| SearchError::SemanticConfig("embedding model lock poisoned".into()))?;
        let mut embeddings = model.embed(vec![query], None)?;
        let mut embedding = embeddings
            .pop()
            .ok_or_else(|| SearchError::SemanticConfig("no embedding returned".into()))?;
        normalize_embedding(&mut embedding)
            .map_err(|err| SearchError::SemanticConfig(format!("query embedding invalid: {err}")))?;
        Ok(embedding)
    }

    fn search(
        &self,
        query_embedding: &[f32],
        limit: usize,
        source: Option<&str>,
        min_score: f32,
    ) -> Vec<SemanticMatch> {
        if self.dim != 0 && query_embedding.len() != self.dim {
            return Vec::new();
        }
        match self.backend {
            SemanticBackend::Flat => self.search_flat(query_embedding, limit, source, min_score),
            SemanticBackend::Hnsw => {
                if let Some(hnsw) = self.hnsw.as_ref() {
                    self.search_hnsw(hnsw, query_embedding, limit, source, min_score)
                } else {
                    self.search_flat(query_embedding, limit, source, min_score)
                }
            }
        }
    }

    fn search_flat(
        &self,
        query_embedding: &[f32],
        limit: usize,
        source: Option<&str>,
        min_score: f32,
    ) -> Vec<SemanticMatch> {
        let mut matches = Vec::with_capacity(limit);
        let mut scored = Vec::new();
        for record in &self.records {
            if record.embedding.len() != query_embedding.len() {
                continue;
            }
            if let Some(source) = source {
                if !doc_id_matches_source(&record.doc_id, source) {
                    continue;
                }
            }
            let score = dot_product(query_embedding, &record.embedding);
            if score < min_score {
                continue;
            }
            scored.push(SemanticMatch {
                doc_id: record.doc_id.clone(),
                chunk_index: record.chunk_index,
                score,
            });
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
        });

        let take = limit.min(scored.len());
        matches.extend(scored.into_iter().take(take));
        matches
    }

    fn search_hnsw(
        &self,
        hnsw: &HnswIndex,
        query_embedding: &[f32],
        limit: usize,
        source: Option<&str>,
        min_score: f32,
    ) -> Vec<SemanticMatch> {
        if self.records.is_empty() {
            return Vec::new();
        }
        let requested = limit.min(self.records.len()).max(1);
        let ef_search = hnsw.ef_search.max(requested);
        let neighbors = hnsw.index.search(query_embedding, requested, ef_search);

        let mut scored = Vec::new();
        for neighbor in neighbors {
            let idx = neighbor.get_origin_id();
            let Some(record) = self.records.get(idx) else {
                continue;
            };
            if let Some(source) = source {
                if !doc_id_matches_source(&record.doc_id, source) {
                    continue;
                }
            }
            let score = dot_product(query_embedding, &record.embedding);
            if score < min_score {
                continue;
            }
            scored.push(SemanticMatch {
                doc_id: record.doc_id.clone(),
                chunk_index: record.chunk_index,
                score,
            });
        }

        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(Ordering::Equal)
        });
        scored.truncate(limit.min(scored.len()));
        scored
    }
}

fn system_time_to_unix_ms(time: std::time::SystemTime) -> Option<u64> {
    time.duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
}

#[cfg(test)]
mod runtime_tests {
    use super::*;
    use crate::search::{
        CorpusMountConfig, HnswConfig, SearchConfig, SemanticBackend, SemanticConfig,
    };
    use mcp_toolkit_docs::ChunkConfig;
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

    struct TestHarness {
        workspace_root: PathBuf,
        corpus_dir: PathBuf,
        local_mount: PathBuf,
        index_dir: PathBuf,
        semantic_dir: PathBuf,
        search: SearchIndex,
    }

    impl TestHarness {
        fn new() -> Self {
            let workspace_root = temp_dir("spark-mcp-runtime-workspace");
            let corpus_dir = workspace_root.join("corpus");
            let local_mount = workspace_root.join("spark");
            let index_dir = temp_dir("spark-mcp-runtime-index");
            let semantic_dir = temp_dir("spark-mcp-runtime-semantic");
            fs::create_dir_all(corpus_dir.join("seed")).expect("create corpus dir");
            fs::create_dir_all(local_mount.join("src")).expect("create local mount dir");
            fs::write(corpus_dir.join("seed/readme.md"), "seed").expect("write seed doc");
            fs::write(
                local_mount.join("src/policy_gateway.ads"),
                "procedure Gateway_Decision;",
            )
            .expect("write local file");

            let search = SearchIndex::open_or_create(
                &corpus_dir,
                &index_dir,
                test_config(&semantic_dir),
                false,
                vec![CorpusMountConfig::new("local-spark", local_mount.clone())],
            )
            .expect("build search index");

            Self {
                workspace_root,
                corpus_dir,
                local_mount,
                index_dir,
                semantic_dir,
                search,
            }
        }

        fn local_file(&self) -> PathBuf {
            self.local_mount.join("src/policy_gateway.ads")
        }
    }

    impl Drop for TestHarness {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.workspace_root);
            let _ = fs::remove_dir_all(&self.index_dir);
            let _ = fs::remove_dir_all(&self.semantic_dir);
            let _ = fs::remove_dir_all(&self.corpus_dir);
        }
    }

    #[test]
    fn resolve_tool_file_doc_id_accepts_absolute_in_root_path() {
        let harness = TestHarness::new();
        let file = harness.local_file();
        let doc_id = harness
            .search
            .resolve_tool_file_doc_id(file.to_string_lossy().as_ref())
            .expect("resolve absolute path")
            .expect("mapped doc_id");
        assert_eq!(doc_id, "local-spark/src/policy_gateway.ads");
    }

    #[test]
    fn resolve_tool_file_doc_id_accepts_repo_relative_path() {
        let harness = TestHarness::new();
        let doc_id = harness
            .search
            .resolve_tool_file_doc_id("spark/src/policy_gateway.ads")
            .expect("resolve repo-relative path")
            .expect("mapped doc_id");
        assert_eq!(doc_id, "local-spark/src/policy_gateway.ads");
    }

    #[test]
    fn resolve_tool_file_doc_id_rejects_out_of_root_absolute_path() {
        let harness = TestHarness::new();
        let outside_dir = temp_dir("spark-mcp-runtime-outside");
        let outside_file = outside_dir.join("outside.ads");
        fs::write(&outside_file, "procedure Outside;").expect("write outside file");
        let err = harness
            .search
            .resolve_tool_file_doc_id(outside_file.to_string_lossy().as_ref())
            .expect_err("outside path should fail");
        assert!(matches!(err, SearchError::InvalidDocId(_)));
        let _ = fs::remove_dir_all(outside_dir);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_tool_file_doc_id_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let harness = TestHarness::new();
        let outside_dir = temp_dir("spark-mcp-runtime-symlink-outside");
        let outside_file = outside_dir.join("escape.ads");
        fs::write(&outside_file, "procedure Escape;").expect("write outside target");

        let symlink_path = harness.local_mount.join("src/escape-link.ads");
        symlink(&outside_file, &symlink_path).expect("create symlink");

        let err = harness
            .search
            .resolve_tool_file_doc_id(symlink_path.to_string_lossy().as_ref())
            .expect_err("symlink escape should fail");
        assert!(matches!(err, SearchError::InvalidDocId(_)));
        let _ = fs::remove_dir_all(outside_dir);
    }

    #[test]
    fn resolve_tool_file_doc_id_returns_none_for_unmapped_file() {
        let harness = TestHarness::new();
        let unresolved = harness
            .search
            .resolve_tool_file_doc_id("spark/src/does_not_exist.ads")
            .expect("resolve missing file");
        assert!(unresolved.is_none());
    }

    #[test]
    fn local_freshness_refreshes_after_external_reindex_without_restart() {
        use std::thread::sleep;
        use std::time::Duration;

        let harness = TestHarness::new();
        let local_file = harness.local_file();
        fs::write(&local_file, "procedure Gateway_Decision is null;")
            .expect("update local file");

        let stale_before = harness
            .search
            .local_freshness_report()
            .expect("stale report before refresh");
        assert!(
            stale_before.any_stale,
            "expected stale freshness before external reindex"
        );

        // Ensure the index metadata mtime advances beyond the edited source mtime.
        sleep(Duration::from_millis(10));

        let _external_refresh = SearchIndex::open_or_create(
            &harness.corpus_dir,
            &harness.index_dir,
            test_config(&harness.semantic_dir),
            true,
            vec![CorpusMountConfig::new("local-spark", harness.local_mount.clone())],
        )
        .expect("external refresh process");

        let stale_after = harness
            .search
            .local_freshness_report()
            .expect("freshness after external refresh");
        assert!(
            !stale_after.any_stale,
            "freshness should clear after external reindex writes a new index timestamp"
        );
    }
}
