//! Diagnostics publishing helpers.

use serde_json::{json, Map, Value};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use tower_lsp::lsp_types::{
    CodeDescription, Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity,
    DocumentDiagnosticParams, DocumentDiagnosticReport, DocumentDiagnosticReportResult,
    FullDocumentDiagnosticReport, Location, NumberOrString, Range,
    RelatedFullDocumentDiagnosticReport, RelatedUnchangedDocumentDiagnosticReport,
    UnchangedDocumentDiagnosticReport, Url, WorkspaceDiagnosticParams, WorkspaceDiagnosticReport,
    WorkspaceDiagnosticReportResult, WorkspaceDocumentDiagnosticReport,
    WorkspaceFullDocumentDiagnosticReport, WorkspaceUnchangedDocumentDiagnosticReport,
};
use tower_lsp::Client;

use trust_hir::db::FileId;
use trust_hir::DiagnosticSeverity as HirSeverity;
use trust_syntax::parser::parse;

use crate::config::{DiagnosticSettings, ProjectConfig, CONFIG_FILES};
use crate::external_diagnostics::collect_external_diagnostics;
use crate::library_graph::library_dependency_issues;
use crate::state::{path_to_uri, uri_to_path, ServerState};

use super::lsp_utils::offset_to_position;

pub(crate) async fn publish_diagnostics(
    client: &Client,
    state: &ServerState,
    uri: &Url,
    content: &str,
    file_id: FileId,
) {
    let diagnostics = collect_diagnostics(state, uri, content, file_id);
    let content_hash = hash_content(content);
    let diagnostic_hash = hash_diagnostics(&diagnostics);
    let _ = state.store_diagnostics(uri.clone(), content_hash, diagnostic_hash);

    client
        .publish_diagnostics(uri.clone(), diagnostics, None)
        .await;
}

pub(crate) fn document_diagnostic(
    state: &ServerState,
    params: DocumentDiagnosticParams,
) -> DocumentDiagnosticReportResult {
    let uri = params.text_document.uri;
    let Some(doc) = state.ensure_document(&uri) else {
        return DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
            RelatedFullDocumentDiagnosticReport {
                related_documents: None,
                full_document_diagnostic_report: FullDocumentDiagnosticReport {
                    result_id: None,
                    items: Vec::new(),
                },
            },
        ));
    };

    let diagnostics = collect_diagnostics(state, &uri, &doc.content, doc.file_id);
    let content_hash = hash_content(&doc.content);
    let diagnostic_hash = hash_diagnostics(&diagnostics);
    let result_id = state.store_diagnostics(uri.clone(), content_hash, diagnostic_hash);

    if params
        .previous_result_id
        .as_ref()
        .is_some_and(|previous| previous == &result_id)
    {
        return DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Unchanged(
            RelatedUnchangedDocumentDiagnosticReport {
                related_documents: None,
                unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                    result_id,
                },
            },
        ));
    }

    DocumentDiagnosticReportResult::Report(DocumentDiagnosticReport::Full(
        RelatedFullDocumentDiagnosticReport {
            related_documents: None,
            full_document_diagnostic_report: FullDocumentDiagnosticReport {
                result_id: Some(result_id),
                items: diagnostics,
            },
        },
    ))
}

pub(crate) fn workspace_diagnostic(
    state: &ServerState,
    params: WorkspaceDiagnosticParams,
) -> WorkspaceDiagnosticReportResult {
    let mut previous = std::collections::HashMap::new();
    for entry in params.previous_result_ids {
        previous.insert(entry.uri, entry.value);
    }

    let mut items = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for doc in state.documents() {
        seen.insert(doc.uri.clone());
        let diagnostics = collect_diagnostics(state, &doc.uri, &doc.content, doc.file_id);
        let content_hash = hash_content(&doc.content);
        let diagnostic_hash = hash_diagnostics(&diagnostics);
        let result_id = state.store_diagnostics(doc.uri.clone(), content_hash, diagnostic_hash);

        if previous
            .get(&doc.uri)
            .is_some_and(|prev| prev == &result_id)
        {
            items.push(WorkspaceDocumentDiagnosticReport::Unchanged(
                WorkspaceUnchangedDocumentDiagnosticReport {
                    uri: doc.uri.clone(),
                    version: doc.is_open.then_some(doc.version as i64),
                    unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                        result_id,
                    },
                },
            ));
        } else {
            items.push(WorkspaceDocumentDiagnosticReport::Full(
                WorkspaceFullDocumentDiagnosticReport {
                    uri: doc.uri.clone(),
                    version: doc.is_open.then_some(doc.version as i64),
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: Some(result_id),
                        items: diagnostics,
                    },
                },
            ));
        }
    }

    for (root, config) in state.workspace_configs() {
        let Some(config_path) = config.config_path.clone() else {
            continue;
        };
        let Some(uri) = path_to_uri(&config_path) else {
            continue;
        };
        if seen.contains(&uri) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&config_path) else {
            continue;
        };
        let diagnostics = collect_config_diagnostics(state, &uri, &content, Some(&root));
        let content_hash = hash_content(&content);
        let diagnostic_hash = hash_diagnostics(&diagnostics);
        let result_id = state.store_diagnostics(uri.clone(), content_hash, diagnostic_hash);
        if previous.get(&uri).is_some_and(|prev| prev == &result_id) {
            items.push(WorkspaceDocumentDiagnosticReport::Unchanged(
                WorkspaceUnchangedDocumentDiagnosticReport {
                    uri: uri.clone(),
                    version: None,
                    unchanged_document_diagnostic_report: UnchangedDocumentDiagnosticReport {
                        result_id,
                    },
                },
            ));
        } else {
            items.push(WorkspaceDocumentDiagnosticReport::Full(
                WorkspaceFullDocumentDiagnosticReport {
                    uri: uri.clone(),
                    version: None,
                    full_document_diagnostic_report: FullDocumentDiagnosticReport {
                        result_id: Some(result_id),
                        items: diagnostics,
                    },
                },
            ));
        }
    }

    WorkspaceDiagnosticReportResult::Report(WorkspaceDiagnosticReport { items })
}

pub(crate) fn collect_diagnostics(
    state: &ServerState,
    uri: &Url,
    content: &str,
    file_id: FileId,
) -> Vec<Diagnostic> {
    if is_config_uri(uri) {
        let diagnostics = collect_config_diagnostics(state, uri, content, None);
        let mut diagnostics = diagnostics;
        apply_diagnostic_filters(state, uri, &mut diagnostics);
        apply_diagnostic_overrides(state, uri, &mut diagnostics);
        attach_explainers(state, &mut diagnostics);
        return diagnostics;
    }
    let parsed = parse(content);

    let mut diagnostics: Vec<Diagnostic> = parsed
        .errors()
        .iter()
        .map(|err| {
            let range = Range {
                start: offset_to_position(content, err.range.start().into()),
                end: offset_to_position(content, err.range.end().into()),
            };
            let code = if err.message.starts_with("expected ") {
                "E002"
            } else {
                "E001"
            };

            Diagnostic {
                range,
                severity: Some(DiagnosticSeverity::ERROR),
                code: Some(NumberOrString::String(code.to_string())),
                source: Some("trust-lsp".to_string()),
                message: err.message.clone(),
                ..Default::default()
            }
        })
        .collect();

    let semantic =
        state.with_database(|db| trust_ide::diagnostics::collect_diagnostics(db, file_id));

    for diag in semantic {
        let range = Range {
            start: offset_to_position(content, diag.range.start().into()),
            end: offset_to_position(content, diag.range.end().into()),
        };
        let severity = match diag.severity {
            HirSeverity::Error => DiagnosticSeverity::ERROR,
            HirSeverity::Warning => DiagnosticSeverity::WARNING,
            HirSeverity::Info => DiagnosticSeverity::INFORMATION,
            HirSeverity::Hint => DiagnosticSeverity::HINT,
        };
        let related_information = if diag.related.is_empty() {
            None
        } else {
            Some(
                diag.related
                    .into_iter()
                    .map(|rel| DiagnosticRelatedInformation {
                        location: Location {
                            uri: uri.clone(),
                            range: Range {
                                start: offset_to_position(content, rel.range.start().into()),
                                end: offset_to_position(content, rel.range.end().into()),
                            },
                        },
                        message: rel.message,
                    })
                    .collect(),
            )
        };

        diagnostics.push(Diagnostic {
            range,
            severity: Some(severity),
            code: Some(NumberOrString::String(diag.code.code().to_string())),
            source: Some("trust-lsp".to_string()),
            message: diag.message,
            related_information,
            ..Default::default()
        });
    }

    if let Some(config) = state.workspace_config_for_uri(uri) {
        diagnostics.extend(collect_external_diagnostics(&config, uri));
    }

    apply_diagnostic_filters(state, uri, &mut diagnostics);
    apply_diagnostic_overrides(state, uri, &mut diagnostics);
    attach_explainers(state, &mut diagnostics);
    diagnostics
}

fn apply_diagnostic_filters(state: &ServerState, uri: &Url, diagnostics: &mut Vec<Diagnostic>) {
    let Some(config) = state.workspace_config_for_uri(uri) else {
        return;
    };
    let settings = config.diagnostics;
    diagnostics.retain(|diagnostic| diagnostic_allowed(&settings, diagnostic));
}

fn apply_diagnostic_overrides(state: &ServerState, uri: &Url, diagnostics: &mut [Diagnostic]) {
    let Some(config) = state.workspace_config_for_uri(uri) else {
        return;
    };
    let overrides = &config.diagnostics.severity_overrides;
    if overrides.is_empty() {
        return;
    }
    for diagnostic in diagnostics {
        let Some(code) = diagnostic_code(diagnostic) else {
            continue;
        };
        if let Some(severity) = overrides.get(&code) {
            diagnostic.severity = Some(*severity);
        }
    }
}

fn diagnostic_allowed(settings: &DiagnosticSettings, diagnostic: &Diagnostic) -> bool {
    let Some(code) = diagnostic_code(diagnostic) else {
        return true;
    };
    match code.as_str() {
        "W001" | "W002" | "W009" => settings.warn_unused,
        "W003" => settings.warn_unreachable,
        "W004" => settings.warn_missing_else,
        "W005" => settings.warn_implicit_conversion,
        "W006" => settings.warn_shadowed,
        "W007" => settings.warn_deprecated,
        "W008" => settings.warn_complexity,
        "W010" | "W011" => settings.warn_nondeterminism,
        _ => true,
    }
}

fn attach_explainers(state: &ServerState, diagnostics: &mut [Diagnostic]) {
    for diagnostic in diagnostics {
        let Some(code) = diagnostic_code(diagnostic) else {
            continue;
        };
        let Some(explainer) = diagnostic_explainer(&code, &diagnostic.message) else {
            continue;
        };
        if diagnostic.code_description.is_none() {
            if let Some(href) = spec_url(state, explainer.spec_path) {
                diagnostic.code_description = Some(CodeDescription { href });
            }
        }
        let mut data = match diagnostic.data.take() {
            Some(Value::Object(map)) => map,
            _ => Map::new(),
        };
        data.insert(
            "explain".to_string(),
            json!({
                "iec": explainer.iec_ref,
                "spec": explainer.spec_path,
            }),
        );
        diagnostic.data = Some(Value::Object(data));
    }
}

fn diagnostic_code(diagnostic: &Diagnostic) -> Option<String> {
    diagnostic.code.as_ref().map(|code| match code {
        NumberOrString::String(value) => value.clone(),
        NumberOrString::Number(value) => value.to_string(),
    })
}

fn hash_content(content: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn hash_diagnostics(diagnostics: &[Diagnostic]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for diagnostic in diagnostics {
        diagnostic.range.start.line.hash(&mut hasher);
        diagnostic.range.start.character.hash(&mut hasher);
        diagnostic.range.end.line.hash(&mut hasher);
        diagnostic.range.end.character.hash(&mut hasher);
        let severity_key = match diagnostic.severity {
            Some(severity) if severity == DiagnosticSeverity::ERROR => 1,
            Some(severity) if severity == DiagnosticSeverity::WARNING => 2,
            Some(severity) if severity == DiagnosticSeverity::INFORMATION => 3,
            Some(severity) if severity == DiagnosticSeverity::HINT => 4,
            Some(_) => 0,
            None => 0,
        };
        severity_key.hash(&mut hasher);
        diagnostic_code(diagnostic).hash(&mut hasher);
        diagnostic.source.hash(&mut hasher);
        diagnostic.message.hash(&mut hasher);
        if let Some(related) = &diagnostic.related_information {
            related.len().hash(&mut hasher);
            for item in related {
                item.location.range.start.line.hash(&mut hasher);
                item.location.range.start.character.hash(&mut hasher);
                item.location.range.end.line.hash(&mut hasher);
                item.location.range.end.character.hash(&mut hasher);
                item.message.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

struct DiagnosticExplainer {
    iec_ref: &'static str,
    spec_path: &'static str,
}

fn diagnostic_explainer(code: &str, message: &str) -> Option<DiagnosticExplainer> {
    if code == "E202" && is_oop_access_message(message) {
        return Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.6.5; Table 50",
            spec_path: "docs/specs/09-semantic-rules.md",
        });
    }
    match code {
        "E001" | "E002" | "E003" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §7.3",
            spec_path: "docs/specs/06-statements.md",
        }),
        "E101" | "E104" | "E105" | "W001" | "W002" | "W006" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.5.2.2",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "E102" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.2",
            spec_path: "docs/specs/02-data-types.md",
        }),
        "E103" | "E204" | "E205" | "E206" | "E207" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.6.1",
            spec_path: "docs/specs/04-pou-declarations.md",
        }),
        "E106" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.1.2",
            spec_path: "docs/specs/01-lexical-elements.md",
        }),
        "E201" | "E202" | "E203" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §7.3.2",
            spec_path: "docs/specs/05-expressions.md",
        }),
        "E301" | "E302" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §7.3.1",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "E306" | "E307" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.2; §6.8.2; Table 62",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "E303" | "E304" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.2.6",
            spec_path: "docs/specs/02-data-types.md",
        }),
        "W004" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §7.3.3.3.3",
            spec_path: "docs/specs/06-statements.md",
        }),
        "W003" => Some(DiagnosticExplainer {
            iec_ref: "Tooling quality lint (non-IEC)",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "W005" => Some(DiagnosticExplainer {
            iec_ref: "IEC 61131-3 Ed.3 §6.4.2",
            spec_path: "docs/specs/02-data-types.md",
        }),
        "W008" => Some(DiagnosticExplainer {
            iec_ref: "Tooling quality lint (non-IEC)",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "W009" => Some(DiagnosticExplainer {
            iec_ref: "Tooling quality lint (non-IEC)",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "W010" => Some(DiagnosticExplainer {
            iec_ref: "Tooling quality lint (non-IEC); TIME/DATE types per IEC 61131-3 Ed.3 §6.4.2 (Table 10)",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "W011" => Some(DiagnosticExplainer {
            iec_ref: "Tooling quality lint (non-IEC); Direct variables per IEC 61131-3 Ed.3 §6.5.5 (Table 16)",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "W012" => Some(DiagnosticExplainer {
            iec_ref: "Tooling quality lint (non-IEC); shared globals across tasks (IEC 61131-3 Ed.3 §6.5.2.2 Tables 13-16; §6.2/§6.8.2 Table 62)",
            spec_path: "docs/specs/09-semantic-rules.md",
        }),
        "L001" | "L002" | "L003" => Some(DiagnosticExplainer {
            iec_ref: "Tooling config lint (non-IEC)",
            spec_path: "docs/specs/10-runtime.md",
        }),
        _ => None,
    }
}

fn is_oop_access_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("cannot access")
        || lower.contains("access specifier")
        || lower.contains("must be public or internal")
}

fn spec_url(state: &ServerState, spec_path: &str) -> Option<Url> {
    for root in state.workspace_folders() {
        let Some(root_path) = uri_to_path(&root) else {
            continue;
        };
        let candidate = root_path.join(spec_path);
        if candidate.exists() {
            return path_to_uri(&candidate);
        }
    }
    None
}

fn is_config_uri(uri: &Url) -> bool {
    let Some(path) = uri_to_path(uri) else {
        return false;
    };
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| CONFIG_FILES.iter().any(|candidate| candidate == &name))
        .unwrap_or(false)
}

fn collect_config_diagnostics(
    state: &ServerState,
    uri: &Url,
    content: &str,
    root_hint: Option<&Url>,
) -> Vec<Diagnostic> {
    let root = root_hint
        .and_then(uri_to_path)
        .or_else(|| config_root_for_uri(state, uri))
        .unwrap_or_else(|| PathBuf::from("."));
    let config_path = uri_to_path(uri);
    let config = ProjectConfig::from_contents(&root, config_path, content);
    let mut diagnostics = Vec::new();
    for issue in library_dependency_issues(&config) {
        let target = issue
            .dependency
            .as_deref()
            .unwrap_or(issue.subject.as_str());
        let range = find_name_range(content, target);
        diagnostics.push(Diagnostic {
            range,
            severity: Some(DiagnosticSeverity::ERROR),
            code: Some(NumberOrString::String(issue.code.to_string())),
            source: Some("trust-lsp".to_string()),
            message: issue.message,
            ..Default::default()
        });
    }
    diagnostics
}

fn config_root_for_uri(state: &ServerState, uri: &Url) -> Option<PathBuf> {
    state
        .workspace_config_for_uri(uri)
        .map(|config| config.root)
        .or_else(|| uri_to_path(uri).and_then(|path| path.parent().map(Path::to_path_buf)))
}

fn find_name_range(content: &str, name: &str) -> Range {
    if name.is_empty() {
        return fallback_range(content);
    }
    let quoted = format!("\"{name}\"");
    for (line_idx, line) in content.lines().enumerate() {
        if let Some(pos) = line.find(&quoted) {
            let start = pos + 1;
            let end = start + name.len();
            return Range {
                start: tower_lsp::lsp_types::Position::new(line_idx as u32, start as u32),
                end: tower_lsp::lsp_types::Position::new(line_idx as u32, end as u32),
            };
        }
        if let Some(pos) = line.find(name) {
            let end = pos + name.len();
            return Range {
                start: tower_lsp::lsp_types::Position::new(line_idx as u32, pos as u32),
                end: tower_lsp::lsp_types::Position::new(line_idx as u32, end as u32),
            };
        }
    }
    fallback_range(content)
}

fn fallback_range(content: &str) -> Range {
    let end = content
        .lines()
        .next()
        .map(|line| line.len() as u32)
        .unwrap_or(0);
    Range {
        start: tower_lsp::lsp_types::Position::new(0, 0),
        end: tower_lsp::lsp_types::Position::new(0, end),
    }
}
