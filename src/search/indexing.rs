fn build_schema() -> Schema {
    let mut schema = SchemaBuilder::new();
    schema.add_text_field("doc_id", STRING | STORED);
    schema.add_text_field("path", STRING | STORED);
    schema.add_text_field("title", TEXT | STORED);
    schema.add_text_field("source", STRING | STORED);
    schema.add_u64_field("chunk_index", STORED);
    schema.add_text_field("body", TEXT | STORED);
    schema.build()
}

impl Fields {
    fn from_schema(schema: &Schema) -> Self {
        Self {
            doc_id: schema.get_field("doc_id").expect("doc_id field"),
            path: schema.get_field("path").expect("path field"),
            title: schema.get_field("title").expect("title field"),
            source: schema.get_field("source").expect("source field"),
            chunk_index: schema.get_field("chunk_index").expect("chunk_index field"),
            body: schema.get_field("body").expect("body field"),
        }
    }
}

fn build_index(
    index: &Index,
    mounts: &[CorpusMount],
    fields: &Fields,
    config: &SearchConfig,
) -> Result<(), SearchError> {
    let mut writer = index.writer(50_000_000)?;
    writer.delete_all_documents()?;

    let mut total_chunks = 0usize;
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
            let mut doc = TantivyDocument::default();
            doc.add_text(fields.doc_id, &doc_meta.id);
            doc.add_text(fields.path, &doc_meta.path);
            if let Some(title) = doc_meta.title.as_ref() {
                doc.add_text(fields.title, title);
            }
            if let Some(source) = doc_meta.source.as_ref() {
                doc.add_text(fields.source, source);
            }
            doc.add_u64(fields.chunk_index, chunk.index as u64);
            doc.add_text(fields.body, &chunk.text);
            writer.add_document(doc)?;
            total_chunks += 1;
        }
        Ok(WalkOutcome::Continue)
    })?;

    writer.commit()?;
    tracing::info!(chunks = total_chunks, "spark corpus indexed");
    Ok(())
}

fn document_meta(mount: &CorpusMount, path: &Path) -> DocumentMeta {
    let relative = path
        .strip_prefix(&mount.root)
        .unwrap_or(path)
        .to_path_buf();
    let id = if mount.label.is_empty() {
        relative.to_string_lossy().to_string()
    } else {
        format!("{}/{}", mount.label, relative.to_string_lossy())
    };
    let title = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string());
    let source = if mount.label.is_empty() {
        relative
            .components()
            .next()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
    } else {
        Some(mount.label.clone())
    };

    DocumentMeta {
        id,
        path: path.to_string_lossy().to_string(),
        title,
        source,
    }
}

fn is_allowed_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "md"
            | "rst"
            | "txt"
            | "html"
            | "htm"
            | "adoc"
            | "org"
            | "ada"
            | "adb"
            | "ads"
            | "c"
            | "h"
            | "cpp"
            | "hpp"
            | "rs"
            | "py"
            | "java"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
    )
}

fn build_mounts(corpus_dir: &Path, extra_mounts: Vec<CorpusMountConfig>) -> Vec<CorpusMount> {
    let base_root = corpus_dir
        .canonicalize()
        .unwrap_or_else(|_| corpus_dir.to_path_buf());
    let mut mounts = Vec::new();
    mounts.push(CorpusMount {
        label: String::new(),
        root: base_root.clone(),
    });

    let mut labels = HashSet::new();
    labels.insert(String::new());

    for mount in extra_mounts {
        let label = mount.label.trim().to_lowercase();
        if label.is_empty() {
            tracing::warn!("skipping corpus mount with empty label");
            continue;
        }
        if label.contains('/') || label.contains('\\') {
            let log_label = sanitize_log_value(&label);
            tracing::warn!(label = %log_label, "skipping corpus mount with invalid label");
            continue;
        }
        if !labels.insert(label.clone()) {
            let log_label = sanitize_log_value(&label);
            tracing::warn!(label = %log_label, "skipping duplicate corpus mount label");
            continue;
        }
        if !mount.path.exists() {
            let log_label = sanitize_log_value(&label);
            let log_path = sanitize_log_value(&mount.path.display().to_string());
            tracing::warn!(label = %log_label, path = %log_path, "corpus mount path missing");
            continue;
        }
        let root = mount
            .path
            .canonicalize()
            .unwrap_or_else(|_| mount.path.clone());
        if !root.is_dir() {
            let log_label = sanitize_log_value(&label);
            let log_path = sanitize_log_value(&root.display().to_string());
            tracing::warn!(label = %log_label, path = %log_path, "corpus mount path is not a directory");
            continue;
        }
        if root == base_root {
            let log_label = sanitize_log_value(&label);
            tracing::warn!(label = %log_label, "corpus mount path equals corpus root; skipping");
            continue;
        }
        let log_label = sanitize_log_value(&label);
        let log_path = sanitize_log_value(&root.display().to_string());
        tracing::info!(label = %log_label, path = %log_path, "adding corpus mount");
        mounts.push(CorpusMount { label, root });
    }

    mounts
}

enum WalkOutcome {
    Continue,
    Break,
}

fn for_each_corpus_file<F>(
    mounts: &[CorpusMount],
    max_file_bytes: u64,
    mut visit: F,
) -> Result<(), SearchError>
where
    F: FnMut(&CorpusMount, &Path, &fs::Metadata) -> Result<WalkOutcome, SearchError>,
{
    for mount in mounts {
        if !mount.root.exists() {
            continue;
        }
        let is_local = mount.label.starts_with("local-");
        let root = mount.root.clone();
        let walker = WalkBuilder::new(&mount.root)
            .hidden(false)
            .standard_filters(true)
            .add_custom_ignore_filename(".spark-mcp-ignore")
            .parents(false)
            .filter_entry(move |entry| !is_excluded_workspace_path(&root, is_local, entry.path()))
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry
                .file_type()
                .map(|ft| ft.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let path = entry.path();
            if !is_allowed_extension(path) {
                continue;
            }
            if is_excluded_workspace_path(&mount.root, is_local, path) {
                continue;
            }
            let metadata = match path.metadata() {
                Ok(meta) => meta,
                Err(_) => continue,
            };
            if metadata.len() > max_file_bytes {
                continue;
            }
            match visit(mount, path, &metadata)? {
                WalkOutcome::Continue => {}
                WalkOutcome::Break => return Ok(()),
            }
        }
    }
    Ok(())
}

fn is_excluded_workspace_path(root: &Path, is_local: bool, path: &Path) -> bool {
    if !is_local {
        return false;
    }
    let relative = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return false,
    };
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            continue;
        };
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), "target" | "data" | "corpus") {
            return true;
        }
    }
    false
}

fn read_text(path: &Path) -> Option<String> {
    let raw = fs::read(path).ok()?;
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    if matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm") {
        let text = from_read(raw.as_slice(), 120);
        return Some(text);
    }
    String::from_utf8(raw).ok()
}

fn extract_text(doc: &TantivyDocument, field: Field) -> String {
    doc.get_first(field)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

fn extract_text_opt(doc: &TantivyDocument, field: Field) -> Option<String> {
    doc.get_first(field)
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
}

fn extract_u64(doc: &TantivyDocument, field: Field) -> u64 {
    doc.get_first(field)
        .and_then(|value| value.as_u64())
        .unwrap_or(0)
}

fn index_exists(index_dir: &Path) -> bool {
    index_dir.join("meta.json").exists()
}

fn make_snippet(text: &str, query: &str, max_chars: usize) -> String {
    let max_chars = max_chars.max(1);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }

    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    let mut start_idx = 0usize;
    for term in query_lower.split_whitespace() {
        if term.is_empty() {
            continue;
        }
        if let Some(byte_pos) = text_lower.find(term) {
            start_idx = text_lower[..byte_pos].chars().count();
            break;
        }
    }

    let context = max_chars / 2;
    let start = start_idx.saturating_sub(context);
    let end = (start + max_chars).min(chars.len());
    chars[start..end].iter().collect()
}

fn symbol_query(query: &str) -> Option<String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .chars()
        .all(|ch| is_ident_char(ch) || ch == '.')
    {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn find_symbol_match(text: &str, symbol: &str) -> Option<SymbolMatch> {
    let mut comment_depth = 0usize;
    for line in text.lines() {
        let sanitized = sanitize_line(line, &mut comment_depth);
        if sanitized.trim().is_empty() {
            continue;
        }
        if !line_contains_symbol(&sanitized, symbol) {
            continue;
        }
        let kind = if definition_kind_for_line(&sanitized, symbol).is_some() {
            "definition"
        } else {
            "reference"
        };
        return Some(SymbolMatch {
            symbol: symbol.to_string(),
            kind: kind.to_string(),
        });
    }
    None
}

fn collect_related_defs(
    text: &str,
    symbol: &str,
    related_defs: Option<&mut BTreeSet<String>>,
) {
    let Some(related_defs) = related_defs else {
        return;
    };
    let mut comment_depth = 0usize;
    for line in text.lines() {
        let sanitized = sanitize_line(line, &mut comment_depth);
        if sanitized.trim().is_empty() {
            continue;
        }
        let Some(def_symbol) = extract_definition_symbol(&sanitized) else {
            continue;
        };
        if is_related_symbol(&def_symbol, symbol) {
            related_defs.insert(def_symbol);
        }
    }
}

fn provenance_from_source(source: Option<&str>) -> Option<Provenance> {
    let source = source?.trim();
    if source.is_empty() {
        return None;
    }
    let lower = source.to_ascii_lowercase();
    let (kind, confidence) = if lower.starts_with("local-") || lower == "local" {
        ("local", 0.9)
    } else if lower.contains("blog") {
        ("upstream", 0.4)
    } else if lower.contains("spark") || lower.contains("ada") || lower.contains("gnat") {
        ("upstream", 0.7)
    } else {
        ("upstream", 0.6)
    };
    Some(Provenance {
        kind: kind.to_string(),
        source: source.to_string(),
        confidence,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefinitionKind {
    Definition,
}

fn definition_kind_for_line(line: &str, symbol: &str) -> Option<DefinitionKind> {
    if !line_contains_symbol(line, symbol) {
        return None;
    }
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    for (idx, token) in tokens.iter().enumerate() {
        if normalize_symbol_token(token) != symbol {
            continue;
        }
        let start = idx.saturating_sub(3);
        for prior in &tokens[start..idx] {
            let keyword = normalize_keyword(prior);
            if def_keyword_kind(&keyword) {
                return Some(DefinitionKind::Definition);
            }
        }
    }
    None
}

fn def_keyword_kind(keyword: &str) -> bool {
    matches!(
        keyword,
        "procedure"
            | "function"
            | "package"
            | "type"
            | "subtype"
            | "task"
            | "protected"
            | "entry"
            | "generic"
    )
}

fn extract_definition_symbol(line: &str) -> Option<String> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    for (idx, token) in tokens.iter().enumerate() {
        let keyword = normalize_keyword(token);
        if !def_keyword_kind(&keyword) {
            continue;
        }
        let next = tokens.get(idx + 1)?;
        let symbol = normalize_symbol_token(next);
        if symbol.is_empty() {
            continue;
        }
        return Some(symbol);
    }
    None
}

fn is_related_symbol(candidate: &str, symbol: &str) -> bool {
    let candidate = candidate.trim();
    let symbol = symbol.trim();
    if candidate.is_empty() || symbol.is_empty() {
        return false;
    }
    if candidate.eq_ignore_ascii_case(symbol) {
        return false;
    }
    let candidate_lower = candidate.to_ascii_lowercase();
    let symbol_lower = symbol.to_ascii_lowercase();
    if candidate_lower.starts_with(&symbol_lower) || symbol_lower.starts_with(&candidate_lower) {
        return true;
    }
    symbol_prefix(&candidate_lower) == symbol_prefix(&symbol_lower)
}

fn symbol_prefix(symbol: &str) -> &str {
    symbol
        .split(|ch| ch == '_' || ch == '.')
        .next()
        .unwrap_or(symbol)
}

fn normalize_keyword(token: &str) -> String {
    normalize_symbol_token(token).to_ascii_lowercase()
}

fn normalize_symbol_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| !is_ident_char(ch) && ch != '.')
        .to_string()
}

fn sanitize_line(line: &str, comment_depth: &mut usize) -> String {
    let mut output = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    let mut in_string = false;

    while let Some(ch) = chars.next() {
        if *comment_depth > 0 {
            if (ch == '*' && matches!(chars.peek(), Some('/')))
                || (ch == '*' && matches!(chars.peek(), Some(')')))
            {
                chars.next();
                *comment_depth = comment_depth.saturating_sub(1);
                output.push(' ');
                output.push(' ');
                continue;
            }
            output.push(' ');
            continue;
        }

        if in_string {
            if ch == '\\' {
                output.push(' ');
                if chars.next().is_some() {
                    output.push(' ');
                }
                continue;
            }
            if ch == '"' {
                if matches!(chars.peek(), Some('"')) {
                    chars.next();
                    output.push(' ');
                    output.push(' ');
                    continue;
                }
                in_string = false;
                output.push(' ');
                continue;
            }
            output.push(' ');
            continue;
        }

        if ch == '/' && matches!(chars.peek(), Some('*')) {
            chars.next();
            *comment_depth += 1;
            output.push(' ');
            output.push(' ');
            continue;
        }
        if ch == '(' && matches!(chars.peek(), Some('*')) {
            chars.next();
            *comment_depth += 1;
            output.push(' ');
            output.push(' ');
            continue;
        }
        if (ch == '-' && matches!(chars.peek(), Some('-')))
            || (ch == '/' && matches!(chars.peek(), Some('/')))
        {
            break;
        }
        if ch == '"' {
            in_string = true;
            output.push(' ');
            continue;
        }

        output.push(ch);
    }

    output
}

fn line_contains_symbol(line: &str, symbol: &str) -> bool {
    let mut search = line;
    let mut offset = 0usize;
    while let Some(pos) = search.find(symbol) {
        let start = offset + pos;
        let end = start + symbol.len();
        let prev = line[..start].chars().last();
        let next = line[end..].chars().next();
        if !is_ident_char_opt(prev) && !is_ident_char_opt(next) {
            return true;
        }
        offset = end;
        if offset >= line.len() {
            break;
        }
        search = &line[offset..];
    }
    false
}

fn is_ident_char_opt(ch: Option<char>) -> bool {
    ch.map(is_ident_char).unwrap_or(false)
}

fn is_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '\''
}

fn finalize_related_defs(mut related_defs: BTreeSet<String>, symbol: &str) -> Vec<String> {
    related_defs.retain(|value| !value.eq_ignore_ascii_case(symbol));
    related_defs.into_iter().take(5).collect()
}

fn build_line_context_from_lines(
    lines: &[&str],
    line_start: usize,
    line_end: usize,
    context_lines: usize,
) -> LineContext {
    let total = lines.len().max(1);
    let context_start = line_start.saturating_sub(context_lines).max(1);
    let context_end = (line_end + context_lines).min(total);
    let slice = lines[context_start - 1..context_end]
        .iter()
        .map(|line| line.to_string())
        .collect();
    LineContext {
        line_start,
        line_end,
        context_start,
        context_end,
        lines: slice,
    }
}

fn build_line_context_from_cached(
    lines: &[String],
    line_starts: &[usize],
    start: usize,
    end: usize,
    context_lines: usize,
) -> Option<LineContext> {
    if lines.is_empty() {
        return None;
    }
    let end_offset = end.saturating_sub(1).max(start);
    let line_start = line_for_offset(line_starts, start);
    let line_end = line_for_offset(line_starts, end_offset);
    let total = lines.len().max(1);
    let context_start = line_start.saturating_sub(context_lines).max(1);
    let context_end = (line_end + context_lines).min(total);
    let slice = lines[context_start - 1..context_end].to_vec();
    Some(LineContext {
        line_start,
        line_end,
        context_start,
        context_end,
        lines: slice,
    })
}

fn line_for_offset(line_starts: &[usize], offset: usize) -> usize {
    if line_starts.is_empty() {
        return 1;
    }
    let mut low = 0usize;
    let mut high = line_starts.len();
    while low + 1 < high {
        let mid = (low + high) / 2;
        if line_starts[mid] <= offset {
            low = mid;
        } else {
            high = mid;
        }
    }
    low + 1
}

fn compute_line_starts(text: &str) -> Vec<usize> {
    let mut line_starts = Vec::new();
    line_starts.push(0usize);
    for (idx, ch) in text.chars().enumerate() {
        if ch == '\n' {
            line_starts.push(idx + 1);
        }
    }
    line_starts
}

fn scan_sources(mounts: &[CorpusMount], config: &SearchConfig) -> Result<Vec<SourceSummary>, SearchError> {
    let mut map: HashMap<String, SourceSummary> = HashMap::new();
    for_each_corpus_file(mounts, config.max_file_bytes, |mount, path, metadata| {
        let source = if mount.label.is_empty() {
            let relative = path
                .strip_prefix(&mount.root)
                .unwrap_or(path)
                .to_path_buf();
            relative
                .components()
                .next()
                .map(|component| component.as_os_str().to_string_lossy().to_string())
                .unwrap_or_else(|| "root".to_string())
        } else {
            mount.label.clone()
        };

        let entry = map.entry(source.clone()).or_insert(SourceSummary {
            source,
            file_count: 0,
            total_bytes: 0,
        });
        entry.file_count += 1;
        entry.total_bytes += metadata.len();
        Ok(WalkOutcome::Continue)
    })?;

    let mut sources: Vec<SourceSummary> = map.into_values().collect();
    sources.sort_by(|a, b| a.source.cmp(&b.source));
    Ok(sources)
}

fn build_index_metadata(index_dir: &Path, sources: &[SourceSummary]) -> IndexMetadata {
    let indexed_at_unix_ms = index_timestamp_unix_ms(index_dir);

    let mut file_count = 0usize;
    let mut total_bytes = 0u64;
    for source in sources {
        file_count += source.file_count;
        total_bytes += source.total_bytes;
    }

    IndexMetadata {
        indexed_at_unix_ms,
        source_count: sources.len(),
        file_count,
        total_bytes,
    }
}

fn index_timestamp_unix_ms(index_dir: &Path) -> Option<u64> {
    fs::metadata(index_dir.join("meta.json"))
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
}

fn now_unix_ms() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis() as u64)
}

fn clamp_limit(requested: Option<usize>, default_limit: usize, max_limit: usize) -> usize {
    requested.unwrap_or(default_limit).min(max_limit).max(1)
}

fn clamp_context_lines(requested: Option<usize>) -> usize {
    requested
        .unwrap_or(CONTEXT_LINES)
        .min(CONTEXT_LINES_MAX)
}
