use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use glob::glob;

use trust_hir::{db::FileId, SourceKey, SourceRegistry};
use trust_runtime::control::SourceFile as ControlSourceFile;
use trust_runtime::debug::{DebugBreakpoint, DebugControl, HitCondition, LogFragment};
#[cfg(test)]
use trust_runtime::harness::TestHarness;
use trust_runtime::harness::{
    parse_debug_expression, CompileError, CompileSession, SourceFile as HarnessSourceFile,
};
use trust_runtime::{Runtime, RuntimeMetadata};

use crate::protocol::{
    Breakpoint, SetBreakpointsArguments, SetBreakpointsResponseBody, Source, SourceBreakpoint,
};
use crate::runtime::DebugRuntime;

const MSG_MISSING_SOURCE: &str = "source path not provided";
const MSG_UNKNOWN_SOURCE: &str = "source not registered";
const MSG_PENDING_SOURCE: &str = "source pending (program not loaded yet)";
const MSG_INVALID_POSITION: &str = "line/column are 1-based";
const MSG_INVALID_LOG_MESSAGE: &str = "invalid log message";
const MSG_INVALID_CONDITION: &str = "invalid breakpoint condition";
const MSG_INVALID_HIT_CONDITION: &str = "invalid hit condition";
const MSG_NO_STATEMENT: &str = "no statement at or after requested location";
const DEFAULT_IGNORE_PRAGMAS: &[&str] = &["@trustlsp:runtime-ignore"];
const PRAGMA_SCAN_LINES: usize = 20;

#[derive(Debug, Clone)]
pub struct SourceFile {
    pub file_id: u32,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct SourceOptions {
    pub root: Option<String>,
    pub include_globs: Vec<String>,
    pub exclude_globs: Vec<String>,
    pub ignore_pragmas: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default)]
pub struct SourceOptionsUpdate {
    pub root: Option<String>,
    pub include_globs: Option<Vec<String>>,
    pub exclude_globs: Option<Vec<String>>,
    pub ignore_pragmas: Option<Vec<String>>,
}

impl SourceOptions {
    pub fn apply_update(&mut self, update: SourceOptionsUpdate) {
        if let Some(root) = update.root {
            let trimmed = root.trim();
            if !trimmed.is_empty() {
                self.root = Some(trimmed.to_string());
            }
        }
        if let Some(include_globs) = update.include_globs {
            self.include_globs = include_globs;
        }
        if let Some(exclude_globs) = update.exclude_globs {
            self.exclude_globs = exclude_globs;
        }
        if let Some(ignore_pragmas) = update.ignore_pragmas {
            self.ignore_pragmas = Some(ignore_pragmas);
        }
    }
}

#[derive(Debug)]
pub struct DebugSession {
    runtime: Arc<Mutex<Runtime>>,
    metadata: RuntimeMetadata,
    control: DebugControl,
    sources: HashMap<SourceKey, SourceFile>,
    source_registry: SourceRegistry,
    breakpoints: BreakpointManager,
    program_path: Option<String>,
    source_options: SourceOptions,
}

impl DebugSession {
    #[must_use]
    pub fn new(runtime: Runtime) -> Self {
        let mut runtime = runtime;
        let control = DebugControl::new();
        runtime.set_debug_control(control.clone());
        let metadata = runtime.metadata_snapshot();
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
            metadata,
            control,
            sources: HashMap::new(),
            source_registry: SourceRegistry::new(),
            breakpoints: BreakpointManager::new(),
            program_path: None,
            source_options: SourceOptions::default(),
        }
    }

    #[must_use]
    pub fn with_control(runtime: Runtime, control: DebugControl) -> Self {
        let mut runtime = runtime;
        runtime.set_debug_control(control.clone());
        let metadata = runtime.metadata_snapshot();
        Self {
            runtime: Arc::new(Mutex::new(runtime)),
            metadata,
            control,
            sources: HashMap::new(),
            source_registry: SourceRegistry::new(),
            breakpoints: BreakpointManager::new(),
            program_path: None,
            source_options: SourceOptions::default(),
        }
    }

    #[must_use]
    pub fn take_breakpoint_report(&mut self) -> Option<String> {
        self.breakpoints.take_report()
    }

    pub fn register_source(
        &mut self,
        path: impl Into<String>,
        file_id: u32,
        text: impl Into<String>,
    ) {
        let path = path.into();
        let key = SourceKey::from_path(Path::new(&path));
        let file_id = self
            .source_registry
            .insert_with_id(key.clone(), FileId(file_id));
        self.sources.insert(
            key,
            SourceFile {
                file_id: file_id.0,
                text: text.into(),
            },
        );
    }

    /// Replace all sources with a single file.
    pub fn replace_single_source(&mut self, path: impl Into<String>, text: impl Into<String>) {
        self.sources.clear();
        self.source_registry.clear();
        self.register_source(path, 0, text);
    }

    /// Replace all sources with multiple files.
    pub fn replace_sources(&mut self, sources: &[(String, String)]) {
        self.sources.clear();
        self.source_registry.clear();
        for (idx, (path, text)) in sources.iter().enumerate() {
            self.register_source(path.clone(), idx as u32, text.clone());
        }
    }

    /// Remember the active program path (for reload).
    pub fn set_program_path(&mut self, path: impl Into<String>) {
        self.program_path = Some(path.into());
    }

    pub fn update_source_options(&mut self, update: SourceOptionsUpdate) {
        self.source_options.apply_update(update);
    }

    /// Reload the current program from disk.
    pub fn reload_program(&mut self, path: Option<&str>) -> Result<Vec<Breakpoint>, CompileError> {
        let path = match path {
            Some(path) => path.to_string(),
            None => self
                .program_path
                .clone()
                .ok_or_else(|| CompileError::new("no program path for reload"))?,
        };
        let sources = collect_sources(&path, &self.source_options)?;
        let (retained, current_time) = {
            let runtime = self
                .runtime
                .lock()
                .map_err(|_| CompileError::new("runtime lock poisoned"))?;
            (runtime.retain_snapshot(), runtime.current_time())
        };

        let source_files = sources
            .iter()
            .map(|(path, text)| HarnessSourceFile::with_path(path.clone(), text.clone()))
            .collect::<Vec<_>>();
        let compile = CompileSession::from_sources(source_files);
        let mut runtime = compile.build_runtime()?;
        runtime.set_debug_control(self.control.clone());
        runtime.apply_retain_snapshot(&retained);
        runtime.set_current_time(current_time);

        let metadata = runtime.metadata_snapshot();
        {
            let mut guard = self
                .runtime
                .lock()
                .map_err(|_| CompileError::new("runtime lock poisoned"))?;
            *guard = runtime;
        }
        self.metadata = metadata;
        self.replace_sources(&sources);

        // Ensure no stale breakpoints linger across reloads.
        self.control.clear_breakpoints();

        Ok(self.revalidate_breakpoints())
    }

    #[must_use]
    pub fn source_file_for_path(&self, path: &str) -> Option<&SourceFile> {
        let key = SourceKey::from_path(Path::new(path));
        self.sources
            .get(&key)
            .or_else(|| self.sources.get(&SourceKey::from_virtual(path.to_string())))
    }

    #[must_use]
    pub fn debug_control(&self) -> DebugControl {
        self.control.clone()
    }

    #[must_use]
    pub fn runtime_handle(&self) -> Arc<Mutex<Runtime>> {
        Arc::clone(&self.runtime)
    }

    #[must_use]
    pub fn metadata(&self) -> &RuntimeMetadata {
        &self.metadata
    }

    #[must_use]
    pub fn source_for_file_id(&self, file_id: u32) -> Option<Source> {
        let key = self.source_registry.key_for_file_id(FileId(file_id))?;
        let path = key.display();
        Some(Source {
            name: Some(path.clone()),
            path: Some(path),
            source_reference: None,
        })
    }

    #[must_use]
    pub fn source_text_for_file_id(&self, file_id: u32) -> Option<&str> {
        let key = self.source_registry.key_for_file_id(FileId(file_id))?;
        self.sources.get(key).map(|file| file.text.as_str())
    }

    #[must_use]
    pub fn set_breakpoints(
        &mut self,
        args: &SetBreakpointsArguments,
    ) -> SetBreakpointsResponseBody {
        let context = BreakpointContext::new(&self.sources, &self.metadata, &self.control);
        self.breakpoints.set_breakpoints(context, args)
    }

    /// Revalidate previously requested breakpoints after reload.
    pub fn revalidate_breakpoints(&mut self) -> Vec<Breakpoint> {
        let context = BreakpointContext::new(&self.sources, &self.metadata, &self.control);
        self.breakpoints.revalidate_breakpoints(context)
    }

    #[cfg(test)]
    pub fn clear_requested_breakpoints(&mut self) {
        self.breakpoints.clear_requested();
    }
}

impl DebugRuntime for DebugSession {
    fn update_source_options(&mut self, update: SourceOptionsUpdate) {
        DebugSession::update_source_options(self, update);
    }

    fn set_program_path(&mut self, path: String) {
        DebugSession::set_program_path(self, path);
    }

    fn reload_program(&mut self, path: Option<&str>) -> Result<Vec<Breakpoint>, CompileError> {
        DebugSession::reload_program(self, path)
    }

    fn set_breakpoints(&mut self, args: &SetBreakpointsArguments) -> SetBreakpointsResponseBody {
        DebugSession::set_breakpoints(self, args)
    }

    fn take_breakpoint_report(&mut self) -> Option<String> {
        DebugSession::take_breakpoint_report(self)
    }

    fn debug_control(&self) -> DebugControl {
        DebugSession::debug_control(self)
    }

    fn runtime_handle(&self) -> Arc<Mutex<Runtime>> {
        DebugSession::runtime_handle(self)
    }

    fn metadata(&self) -> &RuntimeMetadata {
        DebugSession::metadata(self)
    }

    fn source_file_for_path(&self, path: &str) -> Option<&SourceFile> {
        DebugSession::source_file_for_path(self, path)
    }

    fn source_for_file_id(&self, file_id: u32) -> Option<Source> {
        DebugSession::source_for_file_id(self, file_id)
    }

    fn source_text_for_file_id(&self, file_id: u32) -> Option<&str> {
        DebugSession::source_text_for_file_id(self, file_id)
    }

    fn control_sources(&self) -> Vec<ControlSourceFile> {
        self.sources
            .iter()
            .map(|(key, source)| ControlSourceFile {
                id: source.file_id,
                path: PathBuf::from(key.display()),
                text: source.text.clone(),
            })
            .collect()
    }
}

#[derive(Debug)]
struct BreakpointManager {
    requested: HashMap<SourceKey, Vec<SourceBreakpoint>>,
    last_report: Option<String>,
}

impl BreakpointManager {
    fn new() -> Self {
        Self {
            requested: HashMap::new(),
            last_report: None,
        }
    }

    fn take_report(&mut self) -> Option<String> {
        self.last_report.take()
    }

    #[cfg(test)]
    fn clear_requested(&mut self) {
        self.requested.clear();
    }

    fn set_breakpoints(
        &mut self,
        context: BreakpointContext<'_>,
        args: &SetBreakpointsArguments,
    ) -> SetBreakpointsResponseBody {
        let requested = requested_breakpoints(args);
        let raw_path = args.source.path.as_deref();
        let source_key = raw_path.map(source_key_for_path);

        if let Some(key) = source_key.as_ref() {
            if requested.is_empty() {
                self.requested.remove(key);
            } else {
                self.requested.insert(key.clone(), requested.clone());
            }
        }
        let mut report_lines = Vec::new();
        if let Some(path) = raw_path {
            report_lines.push(format!("[trust-debug] breakpoint resolve: {path}"));
        } else {
            report_lines.push("[trust-debug] breakpoint resolve: <unknown source>".to_string());
        }
        if requested.is_empty() {
            if let Some(key) = source_key.as_ref() {
                if let Some(source_file) = context.sources.get(key) {
                    context
                        .control
                        .set_breakpoints_for_file(source_file.file_id, Vec::new());
                }
            }
            report_lines.push("  cleared".to_string());
            self.last_report = Some(report_lines.join("\n"));
            return SetBreakpointsResponseBody {
                breakpoints: Vec::new(),
            };
        }

        let Some(key) = source_key else {
            report_lines.push("  error: missing source path".to_string());
            self.last_report = Some(report_lines.join("\n"));
            return SetBreakpointsResponseBody {
                breakpoints: requested
                    .into_iter()
                    .map(|bp| {
                        Breakpoint::unverified(
                            bp.line,
                            bp.column,
                            None,
                            Some(MSG_MISSING_SOURCE.into()),
                        )
                    })
                    .collect(),
            };
        };

        let source_file = context.sources.get(&key);
        let Some(source_file) = source_file else {
            if context.sources.is_empty() {
                report_lines.push("  pending: program not loaded yet".to_string());
                self.last_report = Some(report_lines.join("\n"));
                return SetBreakpointsResponseBody {
                    breakpoints: requested
                        .into_iter()
                        .map(|bp| {
                            Breakpoint::unverified(
                                bp.line,
                                bp.column,
                                Some(args.source.clone()),
                                Some(MSG_PENDING_SOURCE.into()),
                            )
                        })
                        .collect(),
                };
            }
            report_lines.push("  error: unknown source file".to_string());
            self.last_report = Some(report_lines.join("\n"));
            return SetBreakpointsResponseBody {
                breakpoints: requested
                    .into_iter()
                    .map(|bp| {
                        Breakpoint::unverified(
                            bp.line,
                            bp.column,
                            Some(args.source.clone()),
                            Some(MSG_UNKNOWN_SOURCE.into()),
                        )
                    })
                    .collect(),
            };
        };

        let source_text = source_file.text.clone();
        let file_id = source_file.file_id;
        let profile = context.metadata.profile();
        let mut registry = context.metadata.registry().clone();
        let using = context
            .control
            .snapshot()
            .and_then(|snapshot| {
                snapshot
                    .storage
                    .current_frame()
                    .map(|frame| frame.id)
                    .and_then(|frame_id| {
                        context
                            .metadata
                            .using_for_frame(&snapshot.storage, frame_id)
                    })
            })
            .unwrap_or_default();

        let mut resolved_breakpoints = Vec::new();
        let mut breakpoints = Vec::with_capacity(requested.len());
        for requested_bp in requested {
            let requested_line = requested_bp.line;
            let requested_column = requested_bp.column.unwrap_or(1);
            let first_non_ws =
                first_non_whitespace_column(&source_text, requested_bp.line.saturating_sub(1))
                    .map(|col| col.saturating_add(1));
            let column_override = match requested_bp.column {
                None => first_non_ws,
                Some(col) => match first_non_ws {
                    Some(first) if col <= first => Some(first),
                    _ => None,
                },
            };
            let Some((line, column)) =
                to_zero_based(requested_bp.line, column_override.or(requested_bp.column))
            else {
                report_lines.push(format!(
                    "  req {requested_line}:{requested_column} -> invalid position"
                ));
                breakpoints.push(Breakpoint::unverified(
                    requested_bp.line,
                    requested_bp.column,
                    Some(args.source.clone()),
                    Some(MSG_INVALID_POSITION.into()),
                ));
                continue;
            };

            match context
                .metadata
                .resolve_breakpoint_position(&source_text, file_id, line, column)
            {
                Some((location, resolved_line, resolved_col)) => {
                    let line_text = source_text
                        .lines()
                        .nth(resolved_line as usize)
                        .unwrap_or("")
                        .trim();
                    let mut column_note = String::new();
                    if let Some(override_col) = column_override {
                        if override_col != requested_column {
                            column_note = format!(" (snapped col {override_col})");
                        }
                    }
                    report_lines.push(format!(
                        "  req {requested_line}:{requested_column} -> resolved {}:{} range {}..{}{} text='{}'",
                        resolved_line + 1,
                        resolved_col + 1,
                        location.start,
                        location.end,
                        column_note,
                        line_text
                    ));
                    let condition = match requested_bp.condition.as_deref() {
                        Some(condition) => {
                            match parse_debug_expression(condition, &mut registry, profile, &using)
                            {
                                Ok(expr) => Some(expr),
                                Err(err) => {
                                    breakpoints.push(Breakpoint::unverified(
                                        requested_bp.line,
                                        requested_bp.column,
                                        Some(args.source.clone()),
                                        Some(format!("{MSG_INVALID_CONDITION}: {err}")),
                                    ));
                                    continue;
                                }
                            }
                        }
                        None => None,
                    };

                    let hit_condition = match requested_bp.hit_condition.as_deref() {
                        Some(hit_condition) => match parse_hit_condition(hit_condition) {
                            Some(parsed) => Some(parsed),
                            None => {
                                breakpoints.push(Breakpoint::unverified(
                                    requested_bp.line,
                                    requested_bp.column,
                                    Some(args.source.clone()),
                                    Some(MSG_INVALID_HIT_CONDITION.into()),
                                ));
                                continue;
                            }
                        },
                        None => None,
                    };

                    let log_message = match requested_bp.log_message.as_deref() {
                        Some(template) => {
                            match parse_log_message(template, &mut registry, profile, &using) {
                                Ok(fragments) => Some(fragments),
                                Err(err) => {
                                    breakpoints.push(Breakpoint::unverified(
                                        requested_bp.line,
                                        requested_bp.column,
                                        Some(args.source.clone()),
                                        Some(format!("{MSG_INVALID_LOG_MESSAGE}: {err}")),
                                    ));
                                    continue;
                                }
                            }
                        }
                        None => None,
                    };

                    resolved_breakpoints.push(DebugBreakpoint {
                        location,
                        condition,
                        hit_condition,
                        log_message,
                        hits: 0,
                        generation: 0,
                    });
                    breakpoints.push(Breakpoint::verified(
                        resolved_line + 1,
                        resolved_col + 1,
                        Some(args.source.clone()),
                    ));
                }
                None => {
                    report_lines.push(format!(
                        "  req {requested_line}:{requested_column} -> unresolved (no statement)"
                    ));
                    breakpoints.push(Breakpoint::unverified(
                        requested_bp.line,
                        requested_bp.column,
                        Some(args.source.clone()),
                        Some(MSG_NO_STATEMENT.into()),
                    ));
                }
            }
        }

        context
            .control
            .set_breakpoints_for_file(file_id, resolved_breakpoints);
        self.last_report = Some(report_lines.join("\n"));

        SetBreakpointsResponseBody { breakpoints }
    }

    fn revalidate_breakpoints(&mut self, context: BreakpointContext<'_>) -> Vec<Breakpoint> {
        let mut updated = Vec::new();
        let entries = self
            .requested
            .iter()
            .map(|(key, breakpoints)| (key.clone(), breakpoints.clone()))
            .collect::<Vec<_>>();
        for (key, breakpoints) in entries {
            let path = key.display();
            let args = SetBreakpointsArguments {
                source: Source {
                    name: Some(path.clone()),
                    path: Some(path.clone()),
                    source_reference: None,
                },
                breakpoints: Some(breakpoints),
                lines: None,
                source_modified: None,
            };
            let result = self.set_breakpoints(context, &args);
            updated.extend(result.breakpoints);
        }
        updated
    }
}

#[derive(Clone, Copy)]
struct BreakpointContext<'a> {
    sources: &'a HashMap<SourceKey, SourceFile>,
    metadata: &'a RuntimeMetadata,
    control: &'a DebugControl,
}

impl<'a> BreakpointContext<'a> {
    fn new(
        sources: &'a HashMap<SourceKey, SourceFile>,
        metadata: &'a RuntimeMetadata,
        control: &'a DebugControl,
    ) -> Self {
        Self {
            sources,
            metadata,
            control,
        }
    }
}

fn collect_sources(
    path: &str,
    options: &SourceOptions,
) -> Result<Vec<(String, String)>, CompileError> {
    let entry_path = canonicalize_lossy(Path::new(path));
    let root = resolve_root(options, &entry_path)?;
    let include_globs = normalize_globs(&options.include_globs);
    let exclude_globs = normalize_globs(&options.exclude_globs);

    let mut candidates = if include_globs.is_empty() {
        read_folder_sources(&entry_path)?
    } else {
        expand_globs(&root, &include_globs)?
    };

    if !candidates.iter().any(|candidate| candidate == &entry_path) {
        candidates.push(entry_path.clone());
    }

    let (excluded_files, excluded_dirs) = resolve_excludes(&root, &exclude_globs)?;

    let mut unique = HashSet::new();
    let mut sources = Vec::new();
    let ignore_pragmas = resolve_ignore_pragmas(options);

    for candidate in candidates {
        let candidate = canonicalize_lossy(&candidate);
        if !unique.insert(candidate.clone()) {
            continue;
        }
        if !candidate.is_file() {
            continue;
        }
        if !is_structured_text_file(&candidate) {
            continue;
        }
        if candidate != entry_path && is_excluded(&candidate, &excluded_files, &excluded_dirs) {
            continue;
        }
        let content = std::fs::read_to_string(&candidate).map_err(|err| {
            CompileError::new(format!(
                "failed to read source '{}': {err}",
                candidate.display()
            ))
        })?;
        if candidate != entry_path
            && !ignore_pragmas.is_empty()
            && has_ignore_pragma(&content, &ignore_pragmas)
        {
            continue;
        }
        sources.push((candidate.to_string_lossy().to_string(), content));
    }

    if sources.is_empty() {
        let content = std::fs::read_to_string(&entry_path)
            .map_err(|err| CompileError::new(format!("failed to read program: {err}")))?;
        sources.push((entry_path.to_string_lossy().to_string(), content));
    }

    sources.sort_by(|(a, _), (b, _)| a.cmp(b));
    Ok(sources)
}

fn source_key_for_path(path: &str) -> SourceKey {
    let normalized = canonicalize_lossy(Path::new(path));
    SourceKey::from_path(normalized)
}

fn resolve_root(options: &SourceOptions, entry_path: &Path) -> Result<PathBuf, CompileError> {
    if let Some(root) = options.root.as_ref().map(PathBuf::from) {
        return Ok(canonicalize_lossy(&root));
    }
    let parent = entry_path
        .parent()
        .ok_or_else(|| CompileError::new("program path has no parent directory"))?;
    Ok(canonicalize_lossy(parent))
}

fn normalize_globs(globs: &[String]) -> Vec<String> {
    globs
        .iter()
        .map(|glob| glob.trim())
        .filter(|glob| !glob.is_empty())
        .map(|glob| glob.to_string())
        .collect()
}

fn read_folder_sources(entry_path: &Path) -> Result<Vec<PathBuf>, CompileError> {
    let parent = entry_path
        .parent()
        .ok_or_else(|| CompileError::new("program path has no parent directory"))?;
    let read_dir = std::fs::read_dir(parent)
        .map_err(|err| CompileError::new(format!("failed to read project folder: {err}")))?;
    let mut entries = Vec::new();
    for entry in read_dir {
        let entry = entry.map_err(|err| CompileError::new(format!("read_dir error: {err}")))?;
        let file_path = entry.path();
        if !file_path.is_file() {
            continue;
        }
        entries.push(file_path);
    }
    Ok(entries)
}

fn expand_globs(root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>, CompileError> {
    let mut matches = Vec::new();
    for pattern in patterns {
        for expanded in expand_braces(pattern) {
            let resolved = resolve_glob_pattern(root, &expanded);
            let entries = glob(&resolved)
                .map_err(|err| CompileError::new(format!("invalid glob '{expanded}': {err}")))?;
            for entry in entries {
                let entry = entry
                    .map_err(|err| CompileError::new(format!("glob error '{expanded}': {err}")))?;
                matches.push(entry);
            }
        }
    }
    Ok(matches)
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some((start, end)) = find_brace_range(pattern) else {
        return vec![pattern.to_string()];
    };
    let prefix = &pattern[..start];
    let suffix = &pattern[end + 1..];
    let inner = &pattern[start + 1..end];
    let options = split_brace_options(inner);
    let mut results = Vec::new();
    for option in options {
        let combined = format!("{prefix}{option}{suffix}");
        results.extend(expand_braces(&combined));
    }
    results
}

fn find_brace_range(pattern: &str) -> Option<(usize, usize)> {
    let mut depth = 0usize;
    let mut start = None;
    for (idx, ch) in pattern.char_indices() {
        match ch {
            '{' => {
                if depth == 0 {
                    start = Some(idx);
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
                if depth == 0 {
                    return start.map(|s| (s, idx));
                }
            }
            _ => {}
        }
    }
    None
}

fn split_brace_options(inner: &str) -> Vec<String> {
    let mut options = Vec::new();
    let mut depth = 0usize;
    let mut last = 0usize;
    for (idx, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
            }
            ',' if depth == 0 => {
                options.push(inner[last..idx].to_string());
                last = idx + 1;
            }
            _ => {}
        }
    }
    options.push(inner[last..].to_string());
    if options.is_empty() {
        options.push(String::new());
    }
    options
}

fn resolve_glob_pattern(root: &Path, pattern: &str) -> String {
    let pattern_path = if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        root.join(pattern)
    };
    pattern_path.to_string_lossy().replace('\\', "/")
}

fn resolve_excludes(
    root: &Path,
    patterns: &[String],
) -> Result<(HashSet<PathBuf>, Vec<PathBuf>), CompileError> {
    if patterns.is_empty() {
        return Ok((HashSet::new(), Vec::new()));
    }
    let mut files = HashSet::new();
    let mut dirs = Vec::new();
    for path in expand_globs(root, patterns)? {
        let resolved = canonicalize_lossy(&path);
        if resolved.is_dir() {
            dirs.push(resolved);
        } else {
            files.insert(resolved);
        }
    }
    Ok((files, dirs))
}

fn resolve_ignore_pragmas(options: &SourceOptions) -> Vec<String> {
    match options.ignore_pragmas.as_ref() {
        Some(list) => list
            .iter()
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect(),
        None => DEFAULT_IGNORE_PRAGMAS
            .iter()
            .map(|item| item.to_string())
            .collect(),
    }
}

fn is_excluded(path: &Path, excluded_files: &HashSet<PathBuf>, excluded_dirs: &[PathBuf]) -> bool {
    if excluded_files.contains(path) {
        return true;
    }
    excluded_dirs.iter().any(|dir| path.starts_with(dir))
}

fn is_structured_text_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    matches!(ext, "st" | "ST" | "pou" | "POU")
}

fn has_ignore_pragma(text: &str, pragmas: &[String]) -> bool {
    if pragmas.is_empty() {
        return false;
    }
    for line in text.lines().take(PRAGMA_SCAN_LINES) {
        for pragma in pragmas {
            if line.contains(pragma) {
                return true;
            }
        }
    }
    false
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(canon) = std::fs::canonicalize(path) {
            return strip_windows_device_prefix(canon);
        }
        return strip_windows_device_prefix(path.to_path_buf());
    }

    #[cfg(not(windows))]
    {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    }
}

#[cfg(windows)]
fn strip_windows_device_prefix(path: PathBuf) -> PathBuf {
    let raw = match path.to_str() {
        Some(raw) => raw,
        None => return path,
    };

    if let Some(rest) = raw
        .strip_prefix(r"\?\UNC\")
        .or_else(|| raw.strip_prefix(r"\?\UNC\"))
        .or_else(|| raw.strip_prefix(r"\\.\UNC\"))
        .or_else(|| raw.strip_prefix(r"\??\UNC\"))
    {
        let mut unc = String::from(r"\\");
        unc.push_str(rest);
        return PathBuf::from(unc);
    }

    if let Some(rest) = raw
        .strip_prefix(r"\?\")
        .or_else(|| raw.strip_prefix(r"\?\"))
        .or_else(|| raw.strip_prefix(r"\\.\"))
        .or_else(|| raw.strip_prefix(r"\??\"))
    {
        return PathBuf::from(rest);
    }

    path
}

fn requested_breakpoints(args: &SetBreakpointsArguments) -> Vec<SourceBreakpoint> {
    if let Some(breakpoints) = &args.breakpoints {
        return breakpoints.clone();
    }
    let Some(lines) = &args.lines else {
        return Vec::new();
    };
    lines
        .iter()
        .map(|line| SourceBreakpoint {
            line: *line,
            column: None,
            condition: None,
            hit_condition: None,
            log_message: None,
        })
        .collect()
}

fn parse_hit_condition(raw: &str) -> Option<HitCondition> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (op, rest) = if let Some(rest) = trimmed.strip_prefix(">=") {
        ("ge", rest)
    } else if let Some(rest) = trimmed.strip_prefix("==") {
        ("eq", rest)
    } else if let Some(rest) = trimmed.strip_prefix('>') {
        ("gt", rest)
    } else {
        ("eq", trimmed)
    };
    let value: u64 = rest.trim().parse().ok()?;
    if value == 0 {
        return None;
    }
    match op {
        "ge" => Some(HitCondition::AtLeast(value)),
        "gt" => Some(HitCondition::GreaterThan(value)),
        _ => Some(HitCondition::Equal(value)),
    }
}

fn parse_log_message(
    template: &str,
    registry: &mut trust_hir::types::TypeRegistry,
    profile: trust_runtime::value::DateTimeProfile,
    using: &[smol_str::SmolStr],
) -> Result<Vec<LogFragment>, String> {
    let mut fragments = Vec::new();
    let mut literal = String::new();
    let mut chars = template.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '{' => {
                if chars.peek() == Some(&'{') {
                    chars.next();
                    literal.push('{');
                    continue;
                }
                if !literal.is_empty() {
                    fragments.push(LogFragment::Text(std::mem::take(&mut literal)));
                }
                let mut expr = String::new();
                let mut closed = false;
                for next in chars.by_ref() {
                    if next == '}' {
                        closed = true;
                        break;
                    }
                    expr.push(next);
                }
                if !closed {
                    return Err("unterminated '{' in log message".to_string());
                }
                let expr = expr.trim();
                if expr.is_empty() {
                    return Err("empty log expression".to_string());
                }
                let compiled = parse_debug_expression(expr, registry, profile, using)
                    .map_err(|err| err.to_string())?;
                fragments.push(LogFragment::Expr(compiled));
            }
            '}' => {
                if chars.peek() == Some(&'}') {
                    chars.next();
                    literal.push('}');
                } else {
                    return Err("unmatched '}' in log message".to_string());
                }
            }
            _ => literal.push(ch),
        }
    }

    if !literal.is_empty() {
        fragments.push(LogFragment::Text(literal));
    }

    Ok(fragments)
}

fn to_zero_based(line: u32, column: Option<u32>) -> Option<(u32, u32)> {
    if line == 0 {
        return None;
    }
    let column = column.unwrap_or(1);
    if column == 0 {
        return None;
    }
    Some((line.saturating_sub(1), column.saturating_sub(1)))
}

fn first_non_whitespace_column(source: &str, line: u32) -> Option<u32> {
    let line_idx = usize::try_from(line).ok()?;
    let line_str = source.lines().nth(line_idx)?;
    let mut col = 0u32;
    for ch in line_str.chars() {
        if !ch.is_whitespace() {
            return Some(col);
        }
        col = col.saturating_add(ch.len_utf8() as u32);
    }
    Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Source;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use trust_runtime::debug::SourceLocation;

    static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

    fn temp_source_path(label: &str) -> std::path::PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut dir = std::env::temp_dir();
        dir.push(format!("trust-debug-{label}-{id}"));
        let _ = std::fs::create_dir_all(&dir);
        let mut path = dir;
        path.push("main.st");
        path
    }

    #[test]
    fn expands_brace_globs() {
        let patterns = expand_braces("**/*.{st,ST,pou,POU}");
        assert_eq!(patterns.len(), 4);
        assert!(patterns.contains(&"**/*.st".to_string()));
        assert!(patterns.contains(&"**/*.ST".to_string()));
        assert!(patterns.contains(&"**/*.pou".to_string()));
        assert!(patterns.contains(&"**/*.POU".to_string()));
    }

    #[test]
    fn expands_nested_braces() {
        let patterns = expand_braces("a{b,c}d{e,f}");
        let mut sorted = patterns.clone();
        sorted.sort();
        assert_eq!(sorted, vec!["abde", "abdf", "acde", "acdf"]);
    }

    #[test]
    fn session_resolves_breakpoints_to_statement_start() {
        let mut runtime = Runtime::new();
        let source = "x := 1;\n  y := 2;\n";
        let x_start = source.find("x := 1;").unwrap();
        let x_end = x_start + "x := 1;".len();
        let y_start = source.find("y := 2;").unwrap();
        let y_end = y_start + "y := 2;".len();
        runtime.register_statement_locations(
            0,
            vec![
                SourceLocation::new(0, x_start as u32, x_end as u32),
                SourceLocation::new(0, y_start as u32, y_end as u32),
            ],
        );

        let mut session = DebugSession::new(runtime);
        session.register_source("main.st", 0, source);

        let args = SetBreakpointsArguments {
            source: Source {
                name: Some("main".into()),
                path: Some("main.st".into()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: 2,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: None,
            }]),
            lines: None,
            source_modified: None,
        };

        let response = session.set_breakpoints(&args);
        assert_eq!(response.breakpoints.len(), 1);
        let breakpoint = &response.breakpoints[0];
        assert!(breakpoint.verified);
        assert_eq!(breakpoint.line, Some(2));
        assert_eq!(breakpoint.column, Some(3));
    }

    #[test]
    fn session_snaps_breakpoints_inside_indent() {
        let mut runtime = Runtime::new();
        let source = "x := 1;\n  y := 2;\n";
        let x_start = source.find("x := 1;").unwrap();
        let x_end = x_start + "x := 1;".len();
        let y_start = source.find("y := 2;").unwrap();
        let y_end = y_start + "y := 2;".len();
        runtime.register_statement_locations(
            0,
            vec![
                SourceLocation::new(0, x_start as u32, x_end as u32),
                SourceLocation::new(0, y_start as u32, y_end as u32),
            ],
        );

        let mut session = DebugSession::new(runtime);
        session.register_source("main.st", 0, source);

        let args = SetBreakpointsArguments {
            source: Source {
                name: Some("main".into()),
                path: Some("main.st".into()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: 2,
                column: Some(2),
                condition: None,
                hit_condition: None,
                log_message: None,
            }]),
            lines: None,
            source_modified: None,
        };

        let response = session.set_breakpoints(&args);
        assert_eq!(response.breakpoints.len(), 1);
        let breakpoint = &response.breakpoints[0];
        assert!(breakpoint.verified);
        assert_eq!(breakpoint.line, Some(2));
        assert_eq!(breakpoint.column, Some(3));
    }

    #[test]
    fn session_accepts_logpoint_templates() {
        let mut runtime = Runtime::new();
        let source = "x := 1;\n";
        let x_start = source.find("x := 1;").unwrap();
        let x_end = x_start + "x := 1;".len();
        runtime.register_statement_locations(
            0,
            vec![SourceLocation::new(0, x_start as u32, x_end as u32)],
        );

        let mut session = DebugSession::new(runtime);
        session.register_source("main.st", 0, source);

        let args = SetBreakpointsArguments {
            source: Source {
                name: Some("main".into()),
                path: Some("main.st".into()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: 1,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: Some("x={x}".into()),
            }]),
            lines: None,
            source_modified: None,
        };

        let response = session.set_breakpoints(&args);
        assert_eq!(response.breakpoints.len(), 1);
        assert!(response.breakpoints[0].verified);
    }

    #[test]
    fn session_rejects_invalid_log_message() {
        let mut runtime = Runtime::new();
        let source = "x := 1;\n";
        let x_start = source.find("x := 1;").unwrap();
        let x_end = x_start + "x := 1;".len();
        runtime.register_statement_locations(
            0,
            vec![SourceLocation::new(0, x_start as u32, x_end as u32)],
        );

        let mut session = DebugSession::new(runtime);
        session.register_source("main.st", 0, source);

        let args = SetBreakpointsArguments {
            source: Source {
                name: Some("main".into()),
                path: Some("main.st".into()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: 1,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: Some("{".into()),
            }]),
            lines: None,
            source_modified: None,
        };

        let response = session.set_breakpoints(&args);
        assert_eq!(response.breakpoints.len(), 1);
        assert!(!response.breakpoints[0].verified);
    }

    #[test]
    fn parse_hit_condition_supports_basic_operators() {
        assert_eq!(parse_hit_condition("3"), Some(HitCondition::Equal(3)));
        assert_eq!(parse_hit_condition(">= 4"), Some(HitCondition::AtLeast(4)));
        assert_eq!(
            parse_hit_condition("> 5"),
            Some(HitCondition::GreaterThan(5))
        );
        assert_eq!(parse_hit_condition("==6"), Some(HitCondition::Equal(6)));
        assert!(parse_hit_condition("nope").is_none());
    }

    #[test]
    fn session_reload_revalidates_breakpoints() {
        let path = temp_source_path("reload");
        let source_v1 = r#"PROGRAM Main
VAR
    x : INT;
END_VAR
x := INT#1;
END_PROGRAM
"#;
        std::fs::write(&path, source_v1).unwrap();

        let mut session = DebugSession::new(Runtime::new());
        session.set_program_path(path.to_string_lossy().to_string());
        session
            .reload_program(Some(path.to_string_lossy().as_ref()))
            .unwrap();

        let args = SetBreakpointsArguments {
            source: Source {
                name: Some(path.to_string_lossy().to_string()),
                path: Some(path.to_string_lossy().to_string()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: 5,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: None,
            }]),
            lines: None,
            source_modified: None,
        };
        let response = session.set_breakpoints(&args);
        assert_eq!(response.breakpoints.len(), 1);
        assert_eq!(response.breakpoints[0].line, Some(5));

        let source_v2 = format!("\n{source_v1}");
        std::fs::write(&path, source_v2).unwrap();
        let updated = session.reload_program(None).unwrap();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].line, Some(6));
    }

    #[test]
    fn session_reload_clears_breakpoints_without_requests() {
        let path = temp_source_path("reload_clear");
        let source = r#"PROGRAM Main
VAR
    x : INT;
END_VAR
x := INT#1;
END_PROGRAM
"#;
        std::fs::write(&path, source).unwrap();

        let mut session = DebugSession::new(Runtime::new());
        session.set_program_path(path.to_string_lossy().to_string());
        session
            .reload_program(Some(path.to_string_lossy().as_ref()))
            .unwrap();

        let args = SetBreakpointsArguments {
            source: Source {
                name: Some(path.to_string_lossy().to_string()),
                path: Some(path.to_string_lossy().to_string()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: 5,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: None,
            }]),
            lines: None,
            source_modified: None,
        };
        let _ = session.set_breakpoints(&args);
        assert_eq!(session.control.breakpoint_count(), 1);

        session.clear_requested_breakpoints();
        session.reload_program(None).unwrap();
        assert_eq!(session.control.breakpoint_count(), 0);
    }

    #[test]
    fn session_revalidates_breakpoints_after_source_registration() {
        let source = r#"PROGRAM Main
VAR
    x : INT := 0;
END_VAR
IF x = 0 THEN
    x := x + 1;
END_IF;
END_PROGRAM
"#;
        let harness = TestHarness::from_source(source).unwrap();
        let mut session = DebugSession::new(harness.into_runtime());

        let line_index = source
            .lines()
            .position(|line| line.contains("x := x + 1;"))
            .unwrap();
        let line = line_index as u32 + 1;
        let args = SetBreakpointsArguments {
            source: Source {
                name: Some("main".into()),
                path: Some("main.st".into()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: None,
            }]),
            lines: None,
            source_modified: None,
        };

        let response = session.set_breakpoints(&args);
        assert!(!response.breakpoints[0].verified);

        session.register_source("main.st", 0, source);
        let updated = session.revalidate_breakpoints();
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].line, Some(line));
        assert!(updated[0].verified);
    }

    #[test]
    fn session_resolves_if_header_breakpoint_to_if_statement() {
        let source = r#"PROGRAM Main
VAR
    startCmd : BOOL := TRUE;
    x : INT := 0;
END_VAR
IF startCmd THEN
    x := x + 1;
END_IF;
END_PROGRAM
"#;
        let harness = TestHarness::from_source(source).unwrap();
        let mut session = DebugSession::new(harness.into_runtime());
        session.register_source("main.st", 0, source);

        let if_line = source
            .lines()
            .position(|line| line.trim_start().starts_with("IF startCmd THEN"))
            .unwrap() as u32
            + 1;
        let args = SetBreakpointsArguments {
            source: Source {
                name: Some("main".into()),
                path: Some("main.st".into()),
                source_reference: None,
            },
            breakpoints: Some(vec![SourceBreakpoint {
                line: if_line,
                column: Some(1),
                condition: None,
                hit_condition: None,
                log_message: None,
            }]),
            lines: None,
            source_modified: None,
        };

        let response = session.set_breakpoints(&args);
        assert_eq!(response.breakpoints.len(), 1);
        let bp = &response.breakpoints[0];
        assert!(bp.verified);
        assert_eq!(bp.line, Some(if_line));
        assert_eq!(bp.column, Some(1));
    }
}
