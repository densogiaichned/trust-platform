use super::features::{references, workspace_symbol};
use super::namespace_move_workspace_edit;
use super::*;
use crate::config::{
    BuildConfig, DiagnosticSettings, IndexingConfig, LibraryDependency, LibrarySpec, ProjectConfig,
    RuntimeConfig, StdlibSettings, TargetProfile, TelemetryConfig, WorkspaceSettings,
};
use crate::state::ServerState;
use crate::test_support::test_client;
use expect_test::expect;
use insta::assert_snapshot;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

fn position_at(source: &str, needle: &str) -> tower_lsp::lsp_types::Position {
    let offset = source
        .find(needle)
        .unwrap_or_else(|| panic!("missing needle '{needle}'"));
    super::lsp_utils::offset_to_position(source, offset as u32)
}

fn inlay_label_contains(label: &tower_lsp::lsp_types::InlayHintLabel, needle: &str) -> bool {
    match label {
        tower_lsp::lsp_types::InlayHintLabel::String(value) => value.contains(needle),
        tower_lsp::lsp_types::InlayHintLabel::LabelParts(parts) => {
            parts.iter().any(|part| part.value.contains(needle))
        }
    }
}

fn temp_dir(prefix: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let dir = std::env::temp_dir().join(format!("{prefix}-{stamp}"));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn document_snapshot(state: &ServerState, uri: &tower_lsp::lsp_types::Url) -> Value {
    match state.get_document(uri) {
        Some(doc) => json!({
            "version": doc.version,
            "isOpen": doc.is_open,
            "content": doc.content,
        }),
        None => Value::Null,
    }
}

#[test]
fn lsp_hover_variable() {
    let source = r#"
PROGRAM Test
    VAR
        speed : INT;
    END_VAR

    speed := 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::HoverParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "speed := 1"),
        },
        work_done_progress_params: Default::default(),
    };
    let hover = hover(&state, params).expect("hover result");
    let tower_lsp::lsp_types::HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markdown hover");
    };
    assert!(markup.value.contains("speed"));
    assert!(markup.value.contains("INT"));
}

#[test]
fn lsp_references_variable() {
    let source = r#"
PROGRAM Test
    VAR x : INT; END_VAR
    x := 1;
    x := x + 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::ReferenceParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "x : INT"),
        },
        context: tower_lsp::lsp_types::ReferenceContext {
            include_declaration: true,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let refs = references(&state, params).expect("references");
    assert!(refs.len() >= 2);
}

#[test]
fn lsp_rename_variable() {
    let source = r#"
PROGRAM Test
    VAR x : INT; END_VAR
    x := 1;
    x := x + 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::RenameParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "x : INT"),
        },
        new_name: "y".to_string(),
        work_done_progress_params: Default::default(),
    };
    let edit = rename(&state, params).expect("rename edits");
    let changes = edit.changes.expect("workspace edits");
    let edits = changes.get(&uri).expect("uri edits");
    assert!(edits.len() >= 2);
    assert!(edits.iter().all(|edit| edit.new_text == "y"));
}

#[test]
fn lsp_rename_namespace_path_updates_using_and_qualified_names() {
    let source = r#"
NAMESPACE LibA
TYPE Foo : INT;
END_TYPE
FUNCTION FooFunc : INT
END_FUNCTION
END_NAMESPACE

PROGRAM Main
    USING LibA;
    VAR
        x : LibA.Foo;
    END_VAR
    x := LibA.FooFunc();
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::RenameParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "LibA\nTYPE"),
        },
        new_name: "Company.LibA".to_string(),
        work_done_progress_params: Default::default(),
    };
    let edit = rename(&state, params).expect("rename edits");
    let changes = edit.changes.expect("workspace edits");
    let edits = changes.get(&uri).expect("uri edits");
    assert!(edits.iter().any(|edit| edit.new_text == "Company.LibA"));
    assert!(edits.iter().any(|edit| edit.new_text == "Company.LibA.Foo"));
    assert!(edits
        .iter()
        .any(|edit| edit.new_text == "Company.LibA.FooFunc"));
}

#[test]
fn lsp_rename_primary_pou_renames_file() {
    let source = r#"
FUNCTION_BLOCK OldName
END_FUNCTION_BLOCK
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///OldName.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::RenameParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "OldName"),
        },
        new_name: "NewName".to_string(),
        work_done_progress_params: Default::default(),
    };
    let edit = rename(&state, params).expect("rename edits");
    assert!(edit.changes.is_none(), "expected document changes");

    let document_changes = edit.document_changes.expect("document changes");
    let document_changes = match document_changes {
        tower_lsp::lsp_types::DocumentChanges::Operations(ops) => ops,
        _ => panic!("expected document change operations"),
    };

    let new_uri = tower_lsp::lsp_types::Url::parse("file:///NewName.st").unwrap();
    let has_rename = document_changes.iter().any(|change| {
        matches!(
            change,
            tower_lsp::lsp_types::DocumentChangeOperation::Op(
                tower_lsp::lsp_types::ResourceOp::Rename(rename)
            ) if rename.old_uri == uri && rename.new_uri == new_uri
        )
    });
    assert!(has_rename, "expected rename file operation");

    let has_text_edit = document_changes.iter().any(|change| match change {
        tower_lsp::lsp_types::DocumentChangeOperation::Edit(edit) => {
            edit.edits.iter().any(|edit| {
                matches!(
                    edit,
                    tower_lsp::lsp_types::OneOf::Left(edit) if edit.new_text == "NewName"
                )
            })
        }
        _ => false,
    });
    assert!(has_text_edit, "expected text edits for new POU name");
}

#[test]
fn lsp_pull_diagnostics_returns_unchanged_and_explainer() {
    let source = r#"
PROGRAM Test
    VAR
        A__B : INT;
    END_VAR
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///diag.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    let full = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReport::Full(full) => full,
        _ => panic!("expected full diagnostic report"),
    };
    let result_id = full
        .full_document_diagnostic_report
        .result_id
        .clone()
        .expect("result id");
    let diagnostics = full.full_document_diagnostic_report.items;
    let invalid_identifier = diagnostics
        .iter()
        .find(|diag| match diag.code.as_ref() {
            Some(tower_lsp::lsp_types::NumberOrString::String(code)) => code == "E106",
            _ => false,
        })
        .expect("E106 diagnostic");
    let data = invalid_identifier
        .data
        .as_ref()
        .and_then(|value| value.as_object());
    let explain = data.and_then(|map| map.get("explain"));
    assert!(explain.is_some(), "expected IEC explainer data");

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        identifier: None,
        previous_result_id: Some(result_id),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    assert!(
        matches!(
            report,
            tower_lsp::lsp_types::DocumentDiagnosticReport::Unchanged(_)
        ),
        "expected unchanged diagnostic report"
    );
}

#[test]
fn lsp_supports_virtual_document_uris() {
    let state = ServerState::new();
    let uri =
        tower_lsp::lsp_types::Url::parse("vscode-notebook-cell:/workspace/notebook#cell1").unwrap();
    state.open_document(uri.clone(), 1, "PROGRAM Test END_PROGRAM".to_string());

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    assert!(
        matches!(
            report,
            tower_lsp::lsp_types::DocumentDiagnosticReport::Full(_)
        ),
        "expected full diagnostic report"
    );
}

#[test]
fn lsp_diagnostics_respect_config_toggles() {
    let mut body = String::new();
    for _ in 0..15 {
        body.push_str("    IF TRUE THEN x := x + 1; END_IF;\n");
    }
    let source = format!(
        r#"
PROGRAM Test
    VAR
        x : INT;
        y : REAL;
    END_VAR
    CASE x OF
        1: x := 1;
    END_CASE;
{body}
    y := 1;
END_PROGRAM
"#
    );
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings {
                warn_unused: true,
                warn_unreachable: true,
                warn_missing_else: false,
                warn_implicit_conversion: false,
                warn_shadowed: true,
                warn_deprecated: true,
                warn_complexity: false,
                warn_nondeterminism: true,
                severity_overrides: Default::default(),
            },
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/diag.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    let full = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReport::Full(full) => full,
        _ => panic!("expected full diagnostic report"),
    };
    let codes: Vec<String> = full
        .full_document_diagnostic_report
        .items
        .iter()
        .filter_map(|diag| diag.code.as_ref())
        .map(|code| match code {
            tower_lsp::lsp_types::NumberOrString::String(value) => value.clone(),
            tower_lsp::lsp_types::NumberOrString::Number(value) => value.to_string(),
        })
        .collect();

    assert!(
        !codes.iter().any(|code| code == "W004"),
        "expected MissingElse warning to be filtered"
    );
    assert!(
        !codes.iter().any(|code| code == "W005"),
        "expected ImplicitConversion warning to be filtered"
    );
    assert!(
        !codes.iter().any(|code| code == "W008"),
        "expected HighComplexity warning to be filtered"
    );
}

#[test]
fn lsp_config_diagnostics_report_library_dependency_issues() {
    let config = r#"
[project]
include_paths = ["src"]

[[libraries]]
name = "Core"
path = "libs/core"
version = "1.0"

[[libraries]]
name = "App"
path = "libs/app"
version = "1.0"
dependencies = [{ name = "Core", version = "2.0" }, { name = "Missing" }]
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri]);

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/trust-lsp.toml").unwrap();
    state.open_document(uri.clone(), 1, config.to_string());

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    let full = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReport::Full(full) => full,
        _ => panic!("expected full diagnostic report"),
    };
    let codes: Vec<String> = full
        .full_document_diagnostic_report
        .items
        .iter()
        .filter_map(|diag| diag.code.as_ref())
        .map(|code| match code {
            tower_lsp::lsp_types::NumberOrString::String(value) => value.clone(),
            tower_lsp::lsp_types::NumberOrString::Number(value) => value.to_string(),
        })
        .collect();

    assert!(codes.contains(&"L001".to_string()));
    assert!(codes.contains(&"L002".to_string()));
}

#[test]
fn lsp_external_diagnostics_provide_quick_fixes() {
    let root = temp_dir("trustlsp-external-diag");
    let lint_path = root.join("lint.json");
    std::fs::write(
        &lint_path,
        r#"
[
  {
    "path": "main.st",
    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 7 } },
    "severity": "warning",
    "code": "X001",
    "message": "External issue",
    "fix": {
      "title": "Fix external issue",
      "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 7 } },
      "new_text": "PROGRAM"
    }
  }
]
"#,
    )
    .expect("write lint json");

    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::from_file_path(&root).unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri.clone(),
        ProjectConfig {
            root: root.clone(),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: vec![lint_path.clone()],
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::from_file_path(root.join("main.st")).unwrap();
    let source = "PROGRAM Main\nEND_PROGRAM\n";
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    let full = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReport::Full(full) => full,
        _ => panic!("expected full diagnostic report"),
    };
    let diagnostics = full.full_document_diagnostic_report.items;
    let external = diagnostics
        .iter()
        .find(|diag| {
            diag.code.as_ref().is_some_and(|code| {
            matches!(code, tower_lsp::lsp_types::NumberOrString::String(value) if value == "X001")
        })
        })
        .expect("external diagnostic");

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: external.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![external.clone()],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let actions = code_action(&state, params).expect("code actions");
    let has_external = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(action) => {
            action.title.contains("Fix external issue")
        }
        _ => false,
    });
    assert!(has_external, "expected external quick fix");

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn lsp_document_symbols_include_configuration_hierarchy() {
    let source = r#"
CONFIGURATION Conf
RESOURCE R ON CPU
    TASK Fast (INTERVAL := T#10ms, PRIORITY := 1);
    PROGRAM P1 WITH Fast : Main;
END_RESOURCE
END_CONFIGURATION

PROGRAM Main
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///config.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentSymbolParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let response = document_symbol(&state, params).expect("document symbols");
    let symbols = match response {
        tower_lsp::lsp_types::DocumentSymbolResponse::Flat(symbols) => symbols,
        tower_lsp::lsp_types::DocumentSymbolResponse::Nested(_) => {
            panic!("expected flat document symbols")
        }
    };
    let names: Vec<String> = symbols.iter().map(|symbol| symbol.name.clone()).collect();
    assert!(names.iter().any(|name| name.contains("Conf")));
    assert!(names.iter().any(|name| name.contains("R")));
    assert!(names.iter().any(|name| name.contains("Fast")));
    assert!(names.iter().any(|name| name.contains("P1")));

    let task_container = symbols
        .iter()
        .find(|symbol| symbol.name.contains("Fast"))
        .and_then(|symbol| symbol.container_name.clone());
    assert_eq!(task_container.as_deref(), Some("R"));
}

#[test]
fn lsp_document_symbols_include_members() {
    let source = r#"
INTERFACE ICounter
    METHOD Next : DINT
    END_METHOD
    PROPERTY Value : DINT
        GET
        END_GET
    END_PROPERTY
END_INTERFACE

FUNCTION_BLOCK CounterFb IMPLEMENTS ICounter
VAR
    x : DINT;
END_VAR

METHOD PUBLIC Next : DINT
    x := x + 1;
    Next := x;
END_METHOD

PUBLIC PROPERTY Value : DINT
    GET
        Value := x;
    END_GET
END_PROPERTY
END_FUNCTION_BLOCK
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///members.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentSymbolParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let response = document_symbol(&state, params).expect("document symbols");
    let symbols = match response {
        tower_lsp::lsp_types::DocumentSymbolResponse::Flat(symbols) => symbols,
        tower_lsp::lsp_types::DocumentSymbolResponse::Nested(_) => {
            panic!("expected flat document symbols")
        }
    };

    let has_next = symbols.iter().any(|symbol| symbol.name.contains("Next"));
    let has_value = symbols.iter().any(|symbol| symbol.name.contains("Value"));
    assert!(has_next, "expected Next in document symbols");
    assert!(has_value, "expected Value in document symbols");

    let has_next_in_fb = symbols.iter().any(|symbol| {
        symbol.name.contains("Next") && symbol.container_name.as_deref() == Some("CounterFb")
    });
    assert!(has_next_in_fb, "expected Next under CounterFb");
}

#[test]
fn lsp_oop_access_diagnostics_include_explainer_and_hint() {
    let source = r#"
CLASS Foo
VAR PRIVATE
    secret : INT;
END_VAR
END_CLASS

PROGRAM Test
VAR
    f : Foo;
    x : INT;
END_VAR
    x := f.secret;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///access.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = document_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected diagnostic report"),
    };
    let full = match report {
        tower_lsp::lsp_types::DocumentDiagnosticReport::Full(full) => full,
        _ => panic!("expected full diagnostic report"),
    };
    let diagnostics = full.full_document_diagnostic_report.items;
    let access_diag = diagnostics
        .iter()
        .find(|diag| diag.message.contains("cannot access PRIVATE member"))
        .expect("expected access violation diagnostic");
    let explain = access_diag
        .data
        .as_ref()
        .and_then(|value| value.as_object())
        .and_then(|map| map.get("explain"))
        .and_then(|value| value.get("iec"))
        .and_then(|value| value.as_str());
    assert!(
        explain.is_some_and(|iec| iec.contains("6.6.5")),
        "expected IEC 6.6.5 explainer"
    );
    let related = access_diag.related_information.as_ref();
    assert!(
        related.is_some_and(|items| items.iter().any(|item| item.message.contains("Hint:"))),
        "expected access hint related information"
    );
}

#[test]
fn lsp_workspace_diagnostics_supports_unchanged_reports() {
    let source = r#"
PROGRAM Test
    VAR
        A__B : INT;
    END_VAR
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace-diag.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::WorkspaceDiagnosticParams {
        identifier: None,
        previous_result_ids: Vec::new(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = workspace_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::WorkspaceDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected workspace diagnostic report"),
    };
    let first_item = report
        .items
        .iter()
        .find(|item| match item {
            tower_lsp::lsp_types::WorkspaceDocumentDiagnosticReport::Full(full) => full.uri == uri,
            tower_lsp::lsp_types::WorkspaceDocumentDiagnosticReport::Unchanged(unchanged) => {
                unchanged.uri == uri
            }
        })
        .expect("expected workspace diagnostic item");
    let result_id = match first_item {
        tower_lsp::lsp_types::WorkspaceDocumentDiagnosticReport::Full(full) => full
            .full_document_diagnostic_report
            .result_id
            .clone()
            .expect("result id"),
        _ => panic!("expected full diagnostic report"),
    };

    let params = tower_lsp::lsp_types::WorkspaceDiagnosticParams {
        identifier: None,
        previous_result_ids: vec![tower_lsp::lsp_types::PreviousResultId {
            uri: uri.clone(),
            value: result_id,
        }],
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let report = workspace_diagnostic(&state, params);
    let report = match report {
        tower_lsp::lsp_types::WorkspaceDiagnosticReportResult::Report(report) => report,
        _ => panic!("expected workspace diagnostic report"),
    };
    let unchanged = report
        .items
        .iter()
        .find(|item| match item {
            tower_lsp::lsp_types::WorkspaceDocumentDiagnosticReport::Full(full) => full.uri == uri,
            tower_lsp::lsp_types::WorkspaceDocumentDiagnosticReport::Unchanged(unchanged) => {
                unchanged.uri == uri
            }
        })
        .expect("expected workspace diagnostic item");
    assert!(
        matches!(
            unchanged,
            tower_lsp::lsp_types::WorkspaceDocumentDiagnosticReport::Unchanged(_)
        ),
        "expected unchanged workspace diagnostic report"
    );
}

#[test]
fn lsp_will_rename_files_updates_pou_name() {
    let source_decl = r#"
FUNCTION_BLOCK OldName
END_FUNCTION_BLOCK
"#;
    let source_ref = r#"
PROGRAM Main
    VAR
        fb : OldName;
    END_VAR
END_PROGRAM
"#;
    let state = ServerState::new();
    let decl_uri = tower_lsp::lsp_types::Url::parse("file:///OldName.st").unwrap();
    let ref_uri = tower_lsp::lsp_types::Url::parse("file:///Ref.st").unwrap();
    state.open_document(decl_uri.clone(), 1, source_decl.to_string());
    state.open_document(ref_uri.clone(), 1, source_ref.to_string());

    let params = tower_lsp::lsp_types::RenameFilesParams {
        files: vec![tower_lsp::lsp_types::FileRename {
            old_uri: decl_uri.to_string(),
            new_uri: "file:///NewName.st".to_string(),
        }],
    };
    let edit = will_rename_files(&state, params).expect("rename edits");
    let changes = edit.changes.expect("workspace edits");
    let decl_edits = changes.get(&decl_uri).expect("declaration edits");
    let ref_edits = changes.get(&ref_uri).expect("reference edits");
    assert!(decl_edits.iter().any(|edit| edit.new_text == "NewName"));
    assert!(ref_edits.iter().any(|edit| edit.new_text == "NewName"));
}

#[test]
fn lsp_will_rename_files_updates_using_namespace() {
    let source_decl = r#"
NAMESPACE Lib
FUNCTION Foo : INT
END_FUNCTION
END_NAMESPACE
"#;
    let source_ref = r#"
USING Lib;
PROGRAM Main
    VAR
        x : INT;
    END_VAR
    x := Foo();
END_PROGRAM
"#;
    let state = ServerState::new();
    let decl_uri = tower_lsp::lsp_types::Url::parse("file:///Lib.st").unwrap();
    let ref_uri = tower_lsp::lsp_types::Url::parse("file:///Main.st").unwrap();
    state.open_document(decl_uri.clone(), 1, source_decl.to_string());
    state.open_document(ref_uri.clone(), 1, source_ref.to_string());

    let params = tower_lsp::lsp_types::RenameFilesParams {
        files: vec![tower_lsp::lsp_types::FileRename {
            old_uri: decl_uri.to_string(),
            new_uri: "file:///NewLib.st".to_string(),
        }],
    };
    let edit = will_rename_files(&state, params).expect("rename edits");
    let changes = edit.changes.expect("workspace edits");
    let decl_edits = changes.get(&decl_uri).expect("namespace edits");
    let ref_edits = changes.get(&ref_uri).expect("using edits");
    assert!(decl_edits.iter().any(|edit| edit.new_text == "NewLib"));
    assert!(ref_edits.iter().any(|edit| edit.new_text == "NewLib"));
}

#[test]
fn lsp_workspace_symbols() {
    let source_one = r#"
FUNCTION_BLOCK Counter
    VAR
        value : INT;
    END_VAR
END_FUNCTION_BLOCK
"#;
    let source_two = r#"
PROGRAM Main
    VAR
        counter : Counter;
    END_VAR
END_PROGRAM
"#;

    let state = ServerState::new();
    let uri_one = tower_lsp::lsp_types::Url::parse("file:///one.st").unwrap();
    let uri_two = tower_lsp::lsp_types::Url::parse("file:///two.st").unwrap();
    state.open_document(uri_one.clone(), 1, source_one.to_string());
    state.open_document(uri_two.clone(), 1, source_two.to_string());

    let params = tower_lsp::lsp_types::WorkspaceSymbolParams {
        query: "counter".to_string(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let items = workspace_symbol(&state, params).expect("workspace symbols");
    assert!(
        items
            .iter()
            .any(|item| { item.name.starts_with("Counter") && item.location.uri == uri_one }),
        "Expected to find Counter symbol from first file"
    );
}

#[test]
fn lsp_workspace_symbols_respect_root_visibility_and_priority() {
    let source = r#"
FUNCTION_BLOCK Counter
END_FUNCTION_BLOCK
"#;
    let state = ServerState::new();
    let root_one = temp_dir("trustlsp-root-one");
    let root_two = temp_dir("trustlsp-root-two");
    let root_one_uri = tower_lsp::lsp_types::Url::from_file_path(&root_one).unwrap();
    let root_two_uri = tower_lsp::lsp_types::Url::from_file_path(&root_two).unwrap();
    state.set_workspace_folders(vec![root_one_uri.clone(), root_two_uri.clone()]);

    state.set_workspace_config(
        root_one_uri.clone(),
        ProjectConfig {
            root: root_one.clone(),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings {
                priority: 10,
                visibility: crate::config::WorkspaceVisibility::Public,
            },
            telemetry: TelemetryConfig::default(),
        },
    );
    state.set_workspace_config(
        root_two_uri.clone(),
        ProjectConfig {
            root: root_two.clone(),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings {
                priority: 1,
                visibility: crate::config::WorkspaceVisibility::Private,
            },
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri_one = tower_lsp::lsp_types::Url::from_file_path(root_one.join("one.st")).unwrap();
    let uri_two = tower_lsp::lsp_types::Url::from_file_path(root_two.join("two.st")).unwrap();
    state.open_document(uri_one.clone(), 1, source.to_string());
    state.open_document(uri_two.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::WorkspaceSymbolParams {
        query: "".to_string(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let items = workspace_symbol(&state, params).expect("workspace symbols");
    assert!(items.iter().any(|item| item.location.uri == uri_one));
    assert!(!items.iter().any(|item| item.location.uri == uri_two));

    let params = tower_lsp::lsp_types::WorkspaceSymbolParams {
        query: "counter".to_string(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let items = workspace_symbol(&state, params).expect("workspace symbols");
    let counters: Vec<_> = items
        .iter()
        .filter(|item| item.name.starts_with("Counter"))
        .collect();
    assert!(counters.len() >= 2);
    assert_eq!(counters[0].location.uri, uri_one);
    assert_eq!(counters[1].location.uri, uri_two);
}

#[test]
fn lsp_document_highlight_variable() {
    let source = r#"
PROGRAM Test
    VAR x : INT; END_VAR
    x := 1;
    x := x + 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentHighlightParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "x := 1"),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let highlights = document_highlight(&state, params).expect("document highlights");
    assert!(highlights.len() >= 3);
}

#[test]
fn lsp_semantic_tokens_delta() {
    let source = r#"
PROGRAM Test
    VAR x : INT; END_VAR
    x := 1;
END_PROGRAM
"#;
    let updated = r#"
PROGRAM Test
    VAR x : INT; END_VAR
    x := x + 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let full_params = tower_lsp::lsp_types::SemanticTokensParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let full = semantic_tokens_full(&state, full_params).expect("semantic tokens full");
    let tower_lsp::lsp_types::SemanticTokensResult::Tokens(tokens) = full else {
        panic!("expected semantic tokens");
    };
    let previous_result_id = tokens.result_id.expect("semantic tokens result id");

    state.update_document(&uri, 2, updated.to_string());

    let delta_params = tower_lsp::lsp_types::SemanticTokensDeltaParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        previous_result_id,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let delta = semantic_tokens_full_delta(&state, delta_params).expect("semantic tokens delta");
    match delta {
        tower_lsp::lsp_types::SemanticTokensFullDeltaResult::TokensDelta(delta) => {
            assert!(delta.result_id.is_some());
            assert!(!delta.edits.is_empty());
        }
        _ => panic!("expected semantic tokens delta response"),
    }
}

#[test]
fn lsp_linked_editing_ranges() {
    let source = r#"
PROGRAM Test
    VAR x : INT; END_VAR
    x := 1;
    x := x + 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::LinkedEditingRangeParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "x := 1"),
        },
        work_done_progress_params: Default::default(),
    };

    let ranges = linked_editing_range(&state, params).expect("linked editing ranges");
    assert_eq!(ranges.ranges.len(), 4);
}

#[test]
fn lsp_inlay_hints_parameters() {
    let source = r#"
FUNCTION Add : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Add := A + B;
END_FUNCTION

PROGRAM Main
VAR
    result : INT;
END_VAR
    result := Add(1, 2);
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "Add(1");
    let end_offset = source.find(");").expect("call end");
    let end = super::lsp_utils::offset_to_position(source, end_offset as u32);

    let params = tower_lsp::lsp_types::InlayHintParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range { start, end },
        work_done_progress_params: Default::default(),
    };

    let hints = inlay_hint(&state, params).expect("inlay hints");
    assert_eq!(hints.len(), 2);
    assert!(hints
        .iter()
        .any(|hint| inlay_label_contains(&hint.label, "A")));
    assert!(hints
        .iter()
        .any(|hint| inlay_label_contains(&hint.label, "B")));
}

#[test]
fn lsp_inline_values_constants() {
    let constants = r#"
CONFIGURATION Conf
VAR_GLOBAL CONSTANT
    ANSWER : INT := 42;
END_VAR
END_CONFIGURATION
"#;
    let program = r#"
PROGRAM Test
VAR
    x : INT;
END_VAR
VAR_EXTERNAL CONSTANT
    ANSWER : INT;
END_VAR
    x := ANSWER;
END_PROGRAM
"#;
    let state = ServerState::new();
    let const_uri = tower_lsp::lsp_types::Url::parse("file:///constants.st").unwrap();
    let prog_uri = tower_lsp::lsp_types::Url::parse("file:///main.st").unwrap();
    state.open_document(const_uri, 1, constants.to_string());
    state.open_document(prog_uri.clone(), 1, program.to_string());

    let end = super::lsp_utils::offset_to_position(program, program.len() as u32);
    let params = tower_lsp::lsp_types::InlineValueParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
            uri: prog_uri.clone(),
        },
        range: tower_lsp::lsp_types::Range {
            start: tower_lsp::lsp_types::Position::new(0, 0),
            end,
        },
        context: tower_lsp::lsp_types::InlineValueContext {
            frame_id: 0,
            stopped_location: tower_lsp::lsp_types::Range {
                start: tower_lsp::lsp_types::Position::new(0, 0),
                end,
            },
        },
        work_done_progress_params: Default::default(),
    };

    let values = inline_value(&state, params).expect("inline values");
    let has_answer = values.iter().any(|value| match value {
        tower_lsp::lsp_types::InlineValue::Text(text) => text.text == " = 42",
        _ => false,
    });
    assert!(has_answer);
}

#[test]
fn lsp_inline_values_fetch_runtime_values_from_control_stub() {
    let (endpoint, handle) = spawn_control_stub();
    let source = r#"
CONFIGURATION Conf
VAR_GLOBAL
    g : INT;
END_VAR
VAR_GLOBAL RETAIN
    r : INT;
END_VAR
END_CONFIGURATION

PROGRAM Test
VAR
    x : INT;
END_VAR
    x := x + g + r;
END_PROGRAM
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig {
                control_endpoint: Some(endpoint),
                control_auth_token: None,
            },
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/runtime.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::InlineValueParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        range: tower_lsp::lsp_types::Range {
            start: position_at(source, "x := x"),
            end: position_at(source, "END_PROGRAM"),
        },
        context: tower_lsp::lsp_types::InlineValueContext {
            frame_id: 1,
            stopped_location: tower_lsp::lsp_types::Range {
                start: tower_lsp::lsp_types::Position::new(0, 0),
                end: tower_lsp::lsp_types::Position::new(0, 0),
            },
        },
        work_done_progress_params: Default::default(),
    };

    let values = inline_value(&state, params).expect("inline values");
    let texts: Vec<String> = values
        .iter()
        .filter_map(|value| match value {
            tower_lsp::lsp_types::InlineValue::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();

    assert!(texts.iter().any(|text| text == " = DInt(7)"));
    assert!(texts.iter().any(|text| text == " = DInt(11)"));
    assert!(texts.iter().any(|text| text == " = DInt(42)"));

    handle.join().expect("control stub thread");
}

#[test]
fn lsp_inline_values_merge_instances_into_locals() {
    let (endpoint, handle) = spawn_control_stub_with_instances("TestProgram#1");
    let source = r#"
PROGRAM TestProgram
VAR
    x : DINT;
END_VAR
    x := x + 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig {
                control_endpoint: Some(endpoint),
                control_auth_token: None,
            },
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/runtime.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::InlineValueParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        range: tower_lsp::lsp_types::Range {
            start: position_at(source, "x := x"),
            end: position_at(source, "END_PROGRAM"),
        },
        context: tower_lsp::lsp_types::InlineValueContext {
            frame_id: 1,
            stopped_location: tower_lsp::lsp_types::Range {
                start: tower_lsp::lsp_types::Position::new(0, 0),
                end: tower_lsp::lsp_types::Position::new(0, 0),
            },
        },
        work_done_progress_params: Default::default(),
    };

    let values = inline_value(&state, params).expect("inline values");
    let texts: Vec<String> = values
        .iter()
        .filter_map(|value| match value {
            tower_lsp::lsp_types::InlineValue::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();

    assert!(texts.iter().any(|text| text == " = DInt(9)"));

    handle.join().expect("control stub thread");
}

#[test]
fn lsp_inline_values_merge_instances_with_namespace() {
    let (endpoint, handle) = spawn_control_stub_with_instances("Ns.TestProgram#1");
    let source = r#"
NAMESPACE Ns
PROGRAM TestProgram
VAR
    x : DINT;
END_VAR
    x := x + 1;
END_PROGRAM
END_NAMESPACE
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig {
                control_endpoint: Some(endpoint),
                control_auth_token: None,
            },
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/runtime.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::InlineValueParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri },
        range: tower_lsp::lsp_types::Range {
            start: position_at(source, "x := x"),
            end: position_at(source, "END_PROGRAM"),
        },
        context: tower_lsp::lsp_types::InlineValueContext {
            frame_id: 1,
            stopped_location: tower_lsp::lsp_types::Range {
                start: tower_lsp::lsp_types::Position::new(0, 0),
                end: tower_lsp::lsp_types::Position::new(0, 0),
            },
        },
        work_done_progress_params: Default::default(),
    };

    let values = inline_value(&state, params).expect("inline values");
    let texts: Vec<String> = values
        .iter()
        .filter_map(|value| match value {
            tower_lsp::lsp_types::InlineValue::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect();

    assert!(texts.iter().any(|text| text == " = DInt(9)"));

    handle.join().expect("control stub thread");
}

fn spawn_control_stub() -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind control stub");
    let addr = listener.local_addr().expect("control stub addr");
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept control stub");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut writer = std::io::BufWriter::new(stream);

        for _ in 0..4 {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read line") == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(line.trim()).expect("parse payload");
            let id = payload
                .get("id")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let kind = payload
                .get("type")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let response = match kind {
                "debug.scopes" => json!({
                    "id": id,
                    "ok": true,
                    "result": {
                        "scopes": [
                            { "name": "Locals", "variablesReference": 1 },
                            { "name": "Globals", "variablesReference": 2 },
                            { "name": "Retain", "variablesReference": 3 },
                        ]
                    }
                }),
                "debug.variables" => {
                    let reference = payload
                        .get("params")
                        .and_then(|value| value.get("variables_reference"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let variables = match reference {
                        1 => vec![json!({
                            "name": "x",
                            "value": "DInt(7)",
                            "variablesReference": 0
                        })],
                        2 => vec![json!({
                            "name": "g",
                            "value": "DInt(11)",
                            "variablesReference": 0
                        })],
                        3 => vec![json!({
                            "name": "r",
                            "value": "DInt(42)",
                            "variablesReference": 0
                        })],
                        _ => Vec::new(),
                    };
                    json!({
                        "id": id,
                        "ok": true,
                        "result": { "variables": variables }
                    })
                }
                _ => json!({ "id": id, "ok": false, "error": "unknown request" }),
            };
            writeln!(writer, "{response}").expect("write response");
            writer.flush().expect("flush response");
        }
    });

    (format!("tcp://{addr}"), handle)
}

fn spawn_control_stub_with_instances(instance_name: &str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind control stub");
    let addr = listener.local_addr().expect("control stub addr");
    let instance_name = instance_name.to_string();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept control stub");
        let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
        let mut writer = std::io::BufWriter::new(stream);

        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).expect("read line") == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let payload: serde_json::Value =
                serde_json::from_str(line.trim()).expect("parse payload");
            let id = payload
                .get("id")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let kind = payload
                .get("type")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let response = match kind {
                "debug.scopes" => json!({
                    "id": id,
                    "ok": true,
                    "result": {
                        "scopes": [
                            { "name": "Locals", "variablesReference": 1 },
                            { "name": "Instances", "variablesReference": 2 },
                        ]
                    }
                }),
                "debug.variables" => {
                    let reference = payload
                        .get("params")
                        .and_then(|value| value.get("variables_reference"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let variables = match reference {
                        1 => vec![json!({
                            "name": "temp",
                            "value": "DInt(3)",
                            "variablesReference": 0
                        })],
                        2 => vec![json!({
                            "name": instance_name.clone(),
                            "value": "Instance(1)",
                            "variablesReference": 10
                        })],
                        10 => vec![json!({
                            "name": "x",
                            "value": "DInt(9)",
                            "variablesReference": 0
                        })],
                        _ => Vec::new(),
                    };
                    json!({
                        "id": id,
                        "ok": true,
                        "result": { "variables": variables }
                    })
                }
                _ => json!({ "id": id, "ok": false, "error": "unknown request" }),
            };
            writeln!(writer, "{response}").expect("write response");
            writer.flush().expect("flush response");
        }
    });

    (format!("tcp://{addr}"), handle)
}

#[test]
fn lsp_completion_respects_stdlib_allowlist() {
    let source = r#"
PROGRAM Test
VAR
    x : INT;
END_VAR
    x := A
END_PROGRAM
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings {
                profile: Some("custom".to_string()),
                allow: Some(vec!["ABS".to_string()]),
            },
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::CompletionParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "A\nEND_PROGRAM"),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };

    let response = completion(&state, params).expect("completion response");
    let items = match response {
        tower_lsp::lsp_types::CompletionResponse::Array(items) => items,
        tower_lsp::lsp_types::CompletionResponse::List(list) => list.items,
    };

    let labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
    assert!(labels.iter().any(|label| label.eq_ignore_ascii_case("ABS")));
    assert!(!labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case("SQRT")));
}

#[test]
fn lsp_completion_respects_stdlib_profile_none() {
    let source = r#"
PROGRAM Test
VAR
    x : INT;
END_VAR
    x := A
END_PROGRAM
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings {
                profile: Some("none".to_string()),
                allow: None,
            },
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::CompletionParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "A\nEND_PROGRAM"),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };

    let response = completion(&state, params).expect("completion response");
    let items = match response {
        tower_lsp::lsp_types::CompletionResponse::Array(items) => items,
        tower_lsp::lsp_types::CompletionResponse::List(list) => list.items,
    };

    let labels: Vec<String> = items.into_iter().map(|item| item.label).collect();
    assert!(!labels.iter().any(|label| label.eq_ignore_ascii_case("ABS")));
}

#[test]
fn lsp_hover_member_method_and_property() {
    let interface = r#"
INTERFACE ICounter
    METHOD Next : DINT
    END_METHOD
    PROPERTY Value : DINT
        GET
        END_GET
    END_PROPERTY
END_INTERFACE
"#;
    let fb = r#"
FUNCTION_BLOCK CounterFb IMPLEMENTS ICounter
VAR
    x : DINT;
END_VAR

METHOD PUBLIC Next : DINT
    x := x + 1;
    Next := x;
END_METHOD

PUBLIC PROPERTY Value : DINT
    GET
        Value := x;
    END_GET
END_PROPERTY
END_FUNCTION_BLOCK
"#;
    let main = r#"
PROGRAM Main
VAR
    counter : CounterFb;
    outVal : DINT;
END_VAR

outVal := counter.Next();
outVal := counter.Value;
END_PROGRAM
"#;

    let state = ServerState::new();
    let iface_uri = tower_lsp::lsp_types::Url::parse("file:///icounter.st").unwrap();
    let fb_uri = tower_lsp::lsp_types::Url::parse("file:///counterfb.st").unwrap();
    let main_uri = tower_lsp::lsp_types::Url::parse("file:///main.st").unwrap();
    state.open_document(iface_uri, 1, interface.to_string());
    state.open_document(fb_uri, 1, fb.to_string());
    state.open_document(main_uri.clone(), 1, main.to_string());

    let next_pos = position_at(main, "Next()");
    let next_params = tower_lsp::lsp_types::HoverParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: next_pos,
        },
        work_done_progress_params: Default::default(),
    };
    let next_hover = hover(&state, next_params).expect("hover next");
    let tower_lsp::lsp_types::HoverContents::Markup(next_markup) = next_hover.contents else {
        panic!("expected markdown hover");
    };
    assert!(
        next_markup.value.contains("Next"),
        "expected hover to include Next"
    );

    let value_pos = position_at(main, "Value;");
    let value_params = tower_lsp::lsp_types::HoverParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: main_uri },
            position: value_pos,
        },
        work_done_progress_params: Default::default(),
    };
    let value_hover = hover(&state, value_params).expect("hover value");
    let tower_lsp::lsp_types::HoverContents::Markup(value_markup) = value_hover.contents else {
        panic!("expected markdown hover");
    };
    assert!(
        value_markup.value.contains("Value"),
        "expected hover to include Value"
    );
}

#[test]
fn lsp_hover_respects_stdlib_filter() {
    let source = r#"
PROGRAM Test
VAR
    x : INT;
END_VAR
    x := ABS(1);
END_PROGRAM
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings {
                profile: Some("none".to_string()),
                allow: None,
            },
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::HoverParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "ABS(1"),
        },
        work_done_progress_params: Default::default(),
    };

    let hover = hover(&state, params);
    assert!(hover.is_none(), "expected stdlib hover to be filtered");
}

#[test]
fn lsp_signature_help_snapshot() {
    let source = r#"
FUNCTION Add : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Add := A + B;
END_FUNCTION

PROGRAM Main
VAR
    x : INT;
END_VAR
    x := Add(1, 2|);
END_PROGRAM
"#;
    let cursor = source.find('|').expect("cursor");
    let mut cleaned = source.to_string();
    cleaned.remove(cursor);

    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, cleaned.to_string());

    let position = super::lsp_utils::offset_to_position(&cleaned, cursor as u32);
    let params = tower_lsp::lsp_types::SignatureHelpParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        work_done_progress_params: Default::default(),
        context: None,
    };

    let result = signature_help(&state, params).expect("signature help");
    let json = serde_json::to_string_pretty(&result).expect("serialize signature help");
    expect![[r#"
{
  "signatures": [
    {
      "label": "Add(A: INT, B: INT) : INT",
      "parameters": [
        {
          "label": "A: INT"
        },
        {
          "label": "B: INT"
        }
      ]
    }
  ],
  "activeSignature": 0,
  "activeParameter": 1
}"#]]
    .assert_eq(&json);
}

#[test]
fn lsp_formatting_snapshot() {
    let source = "PROGRAM Test\nVAR\nx:INT;\nEND_VAR\nx:=1;\nEND_PROGRAM\n";
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentFormattingParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        options: tower_lsp::lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
    };

    let edits = formatting(&state, params).expect("formatting edits");
    let formatted = edits
        .first()
        .map(|edit| edit.new_text.as_str())
        .unwrap_or("");
    expect![[r#"
PROGRAM Test
    VAR
        x: INT;
    END_VAR
    x := 1;
END_PROGRAM
"#]]
    .assert_eq(formatted);
}

#[test]
fn lsp_formatting_vendor_profile_applies_keyword_case() {
    let source = "program Test\nvar\nx:INT;\nend_var\nx:=1+2;\nend_program\n";
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_config(
        root_uri,
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: Some("siemens".to_string()),
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentFormattingParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        options: tower_lsp::lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
    };

    let edits = formatting(&state, params).expect("formatting edits");
    assert!(!edits.is_empty());
    let formatted = edits[0].new_text.as_str();
    let expected = "PROGRAM Test\n  VAR\n    x:INT;\n  END_VAR\n  x:=1+2;\nEND_PROGRAM\n";
    assert_eq!(formatted, expected);
}

#[test]
fn lsp_code_lens_references() {
    let source = r#"
FUNCTION Add : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Add := A + B;
END_FUNCTION

PROGRAM Main
VAR
    result : INT;
END_VAR
    result := Add(1, 2);
    result := Add(2, 3);
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::CodeLensParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let lenses = code_lens(&state, params).expect("code lenses");
    let mut found = false;
    for lens in lenses {
        if let Some(cmd) = &lens.command {
            if let Some(count_str) = cmd.title.strip_prefix("References: ") {
                if let Ok(count) = count_str.trim().parse::<usize>() {
                    if count >= 2 {
                        found = true;
                    }
                }
            }
        }
    }
    assert!(found, "expected references code lens");
}

#[test]
fn lsp_document_link_using_directive() {
    let lib_source = r#"
NAMESPACE Lib
FUNCTION Foo : INT
VAR_INPUT
    A : INT;
END_VAR
    Foo := A;
END_FUNCTION
END_NAMESPACE
"#;
    let main_source = r#"
USING Lib;
FUNCTION Bar : INT
    Bar := Foo(1);
END_FUNCTION
"#;
    let state = ServerState::new();
    let lib_uri = tower_lsp::lsp_types::Url::parse("file:///lib.st").unwrap();
    let main_uri = tower_lsp::lsp_types::Url::parse("file:///main.st").unwrap();
    state.open_document(lib_uri.clone(), 1, lib_source.to_string());
    state.open_document(main_uri.clone(), 1, main_source.to_string());

    let params = tower_lsp::lsp_types::DocumentLinkParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
            uri: main_uri.clone(),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let links = document_link(&state, params).expect("document links");
    let start_offset = main_source.find("Lib").expect("Lib offset") as u32;
    let end_offset = start_offset + "Lib".len() as u32;
    assert!(links.iter().any(|link| {
        link.target.as_ref() == Some(&lib_uri)
            && super::lsp_utils::position_to_offset(main_source, link.range.start)
                .map(|start| start <= start_offset)
                .unwrap_or(false)
            && super::lsp_utils::position_to_offset(main_source, link.range.end)
                .map(|end| end >= end_offset)
                .unwrap_or(false)
    }));
}

#[test]
fn lsp_document_link_config_paths() {
    let source = r#"
[project]
include_paths = ["src", "lib"]
library_paths = ["vendor/lib"]

[[libraries]]
name = "Extra"
path = "extras/ExtraLib"
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///workspace/trust-lsp.toml").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let params = tower_lsp::lsp_types::DocumentLinkParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let links = document_link(&state, params).expect("document links");
    let src_target = tower_lsp::lsp_types::Url::parse("file:///workspace/src").unwrap();
    let lib_target = tower_lsp::lsp_types::Url::parse("file:///workspace/vendor/lib").unwrap();
    let extra_target =
        tower_lsp::lsp_types::Url::parse("file:///workspace/extras/ExtraLib").unwrap();

    assert!(links
        .iter()
        .any(|link| link.target.as_ref() == Some(&src_target)));
    assert!(links
        .iter()
        .any(|link| link.target.as_ref() == Some(&lib_target)));
    assert!(links
        .iter()
        .any(|link| link.target.as_ref() == Some(&extra_target)));
}

#[test]
fn lsp_call_hierarchy_incoming_outgoing() {
    let source = r#"
FUNCTION Add : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Add := A + B;
END_FUNCTION

PROGRAM Main
VAR
    result : INT;
END_VAR
    result := Add(1, 2);
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let prepare_main = tower_lsp::lsp_types::CallHierarchyPrepareParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "Main"),
        },
        work_done_progress_params: Default::default(),
    };
    let main_items = prepare_call_hierarchy(&state, prepare_main).expect("prepare main");
    let main_item = main_items.first().expect("main item").clone();

    let outgoing_params = tower_lsp::lsp_types::CallHierarchyOutgoingCallsParams {
        item: main_item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let outgoing = outgoing_calls(&state, outgoing_params).expect("outgoing calls");
    assert!(outgoing.iter().any(|call| call.to.name.contains("Add")));

    let prepare_add = tower_lsp::lsp_types::CallHierarchyPrepareParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "Add : INT"),
        },
        work_done_progress_params: Default::default(),
    };
    let add_items = prepare_call_hierarchy(&state, prepare_add).expect("prepare add");
    let add_item = add_items.first().expect("add item").clone();

    let incoming_params = tower_lsp::lsp_types::CallHierarchyIncomingCallsParams {
        item: add_item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let incoming = incoming_calls(&state, incoming_params).expect("incoming calls");
    assert!(incoming.iter().any(|call| call.from.name.contains("Main")));
}

#[test]
fn lsp_call_hierarchy_cross_file_incoming() {
    let add_source = r#"
FUNCTION Add : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Add := A + B;
END_FUNCTION
"#;
    let main_source = r#"
PROGRAM Main
VAR
    result : INT;
END_VAR
    result := Add(1, 2);
END_PROGRAM
"#;
    let state = ServerState::new();
    let add_uri = tower_lsp::lsp_types::Url::parse("file:///add.st").unwrap();
    let main_uri = tower_lsp::lsp_types::Url::parse("file:///main.st").unwrap();
    state.open_document(add_uri.clone(), 1, add_source.to_string());
    state.open_document(main_uri.clone(), 1, main_source.to_string());

    let prepare_add = tower_lsp::lsp_types::CallHierarchyPrepareParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: add_uri },
            position: position_at(add_source, "Add : INT"),
        },
        work_done_progress_params: Default::default(),
    };
    let add_items = prepare_call_hierarchy(&state, prepare_add).expect("prepare add");
    let add_item = add_items.first().expect("add item").clone();

    let incoming_params = tower_lsp::lsp_types::CallHierarchyIncomingCallsParams {
        item: add_item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let incoming = incoming_calls(&state, incoming_params).expect("incoming calls");
    assert!(incoming.iter().any(|call| call.from.name.contains("Main")));
}

#[test]
fn lsp_call_hierarchy_cross_file_incoming_named_args() {
    let add_source = r#"
FUNCTION Add : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Add := A + B;
END_FUNCTION
"#;
    let main_source = r#"
PROGRAM Main
VAR
    result : INT;
END_VAR
    result := Add(A := 1, B := 2);
END_PROGRAM
"#;
    let state = ServerState::new();
    let add_uri = tower_lsp::lsp_types::Url::parse("file:///add.st").unwrap();
    let main_uri = tower_lsp::lsp_types::Url::parse("file:///main.st").unwrap();
    state.open_document(add_uri.clone(), 1, add_source.to_string());
    state.open_document(main_uri.clone(), 1, main_source.to_string());

    let prepare_add = tower_lsp::lsp_types::CallHierarchyPrepareParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: add_uri },
            position: position_at(add_source, "Add : INT"),
        },
        work_done_progress_params: Default::default(),
    };
    let add_items = prepare_call_hierarchy(&state, prepare_add).expect("prepare add");
    let add_item = add_items.first().expect("add item").clone();

    let incoming_params = tower_lsp::lsp_types::CallHierarchyIncomingCallsParams {
        item: add_item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let incoming = incoming_calls(&state, incoming_params).expect("incoming calls");
    assert!(incoming.iter().any(|call| call.from.name.contains("Main")));
}

#[test]
fn lsp_type_hierarchy_super_and_subtypes() {
    let source = r#"
CLASS Base
END_CLASS

CLASS Derived EXTENDS Base
END_CLASS
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let prepare_derived = tower_lsp::lsp_types::TypeHierarchyPrepareParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "Derived"),
        },
        work_done_progress_params: Default::default(),
    };
    let derived_items = prepare_type_hierarchy(&state, prepare_derived).expect("prepare derived");
    let derived_item = derived_items.first().expect("derived item").clone();

    let super_params = tower_lsp::lsp_types::TypeHierarchySupertypesParams {
        item: derived_item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let supertypes = type_hierarchy_supertypes(&state, super_params).expect("supertypes");
    assert!(supertypes.iter().any(|item| item.name.contains("Base")));

    let prepare_base = tower_lsp::lsp_types::TypeHierarchyPrepareParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: position_at(source, "Base"),
        },
        work_done_progress_params: Default::default(),
    };
    let base_items = prepare_type_hierarchy(&state, prepare_base).expect("prepare base");
    let base_item = base_items.first().expect("base item").clone();

    let sub_params = tower_lsp::lsp_types::TypeHierarchySubtypesParams {
        item: base_item,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let subtypes = type_hierarchy_subtypes(&state, sub_params).expect("subtypes");
    assert!(subtypes.iter().any(|item| item.name.contains("Derived")));
}

#[test]
fn lsp_range_formatting_formats_selection() {
    let source = "PROGRAM Test\nVAR\nx:INT;\nEND_VAR\nx:=1+2;\nEND_PROGRAM\n";
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "x:=1+2");
    let end = super::lsp_utils::offset_to_position(
        source,
        (source.find("x:=1+2").unwrap() + "x:=1+2;".len()) as u32,
    );

    let params = tower_lsp::lsp_types::DocumentRangeFormattingParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range { start, end },
        options: tower_lsp::lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
    };

    let edits = range_formatting(&state, params).expect("range formatting");
    assert_eq!(edits.len(), 1);
    assert!(edits[0].new_text.contains("x := 1 + 2;"));
}

#[test]
fn lsp_range_formatting_expands_to_syntax_block() {
    let source = "PROGRAM Test\nVAR\nx:INT;\nEND_VAR\nIF x=1 THEN\ny:=1;\nEND_IF\nEND_PROGRAM\n";
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///range-block.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "y:=1");
    let end = super::lsp_utils::offset_to_position(
        source,
        (source.find("y:=1").unwrap() + "y:=1;".len()) as u32,
    );

    let params = tower_lsp::lsp_types::DocumentRangeFormattingParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range { start, end },
        options: tower_lsp::lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
    };

    let edits = range_formatting(&state, params).expect("range formatting");
    assert_eq!(edits.len(), 1);
    let edit = &edits[0];
    let if_pos = position_at(source, "IF x=1 THEN");
    let end_if_pos = position_at(source, "END_IF");
    assert_eq!(edit.range.start.line, if_pos.line);
    assert_eq!(edit.range.end.line, end_if_pos.line + 1);
    assert!(edit.new_text.contains("IF x = 1 THEN"));
    assert!(edit.new_text.contains("END_IF"));
}

#[test]
fn lsp_range_formatting_aligns_assignment_groups() {
    let source = "PROGRAM Test\nVAR\nx:INT;\nEND_VAR\nshort:=1;\nlonger :=2;\nEND_PROGRAM\n";
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///range-align.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "short:=1");
    let end = super::lsp_utils::offset_to_position(
        source,
        (source.find("longer :=2").unwrap() + "longer :=2;".len()) as u32,
    );

    let params = tower_lsp::lsp_types::DocumentRangeFormattingParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range { start, end },
        options: tower_lsp::lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
        work_done_progress_params: Default::default(),
    };

    let edits = range_formatting(&state, params).expect("range formatting");
    assert_eq!(edits.len(), 1);
    let lines: Vec<&str> = edits[0].new_text.lines().collect();
    let short_line = lines.iter().find(|line| line.contains("short")).unwrap();
    let longer_line = lines.iter().find(|line| line.contains("longer")).unwrap();
    assert_eq!(
        short_line.find(":=").unwrap(),
        longer_line.find(":=").unwrap()
    );
}

#[test]
fn lsp_on_type_formatting_formats_line() {
    let source = "PROGRAM Test\nVAR\nx:INT;\nEND_VAR\nx:=1+2;\nEND_PROGRAM\n";
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let position = position_at(source, "x:=1+2");
    let params = tower_lsp::lsp_types::DocumentOnTypeFormattingParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position,
        },
        ch: ";".to_string(),
        options: tower_lsp::lsp_types::FormattingOptions {
            tab_size: 4,
            insert_spaces: true,
            ..Default::default()
        },
    };

    let edits = on_type_formatting(&state, params).expect("on type formatting");
    assert_eq!(edits.len(), 1);
    assert!(edits[0].new_text.contains("x := 1 + 2;"));
}

#[test]
fn lsp_code_action_missing_else() {
    let source = r#"
PROGRAM Test
    VAR
        x : INT;
    END_VAR

    CASE x OF
        1: x := 1;
    END_CASE
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "CASE x OF");
    let end_offset = source
        .find("END_CASE")
        .map(|idx| idx + "END_CASE".len())
        .expect("END_CASE");
    let end = super::lsp_utils::offset_to_position(source, end_offset as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::WARNING),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "W004".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "CASE statement has no ELSE branch".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_else_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("ELSE")
        }
        _ => false,
    });
    assert!(has_else_action, "expected ELSE code action");
}

#[test]
fn lsp_code_action_create_var() {
    let source = r#"
PROGRAM Test
VAR
    x : INT;
END_VAR
    foo := 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "foo");
    let end =
        super::lsp_utils::offset_to_position(source, (source.find("foo").unwrap() + 3) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E101".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "undefined identifier 'foo'".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_var_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("VAR")
                && code_action
                    .edit
                    .as_ref()
                    .and_then(|edit| edit.changes.as_ref())
                    .and_then(|changes| changes.values().next())
                    .and_then(|edits| edits.first())
                    .map(|edit| edit.new_text.contains("foo"))
                    .unwrap_or(false)
        }
        _ => false,
    });
    assert!(has_var_action, "expected VAR creation code action");
}

#[test]
fn lsp_code_action_create_type() {
    let source = r#"
PROGRAM Test
VAR
    x : MissingType;
END_VAR
    x := 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "MissingType");
    let end = super::lsp_utils::offset_to_position(
        source,
        (source.find("MissingType").unwrap() + "MissingType".len()) as u32,
    );

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E102".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "cannot resolve type 'MissingType'".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_type_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("TYPE")
                && code_action
                    .edit
                    .as_ref()
                    .and_then(|edit| edit.changes.as_ref())
                    .and_then(|changes| changes.values().next())
                    .and_then(|edits| edits.first())
                    .map(|edit| edit.new_text.contains("TYPE MissingType"))
                    .unwrap_or(false)
        }
        _ => false,
    });
    assert!(has_type_action, "expected TYPE creation code action");
}

#[test]
fn lsp_code_action_implicit_conversion() {
    let source = r#"
PROGRAM Test
VAR
    x : REAL;
END_VAR
    x := 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "1;");
    let end = super::lsp_utils::offset_to_position(source, (source.find("1;").unwrap() + 1) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::WARNING),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "W005".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "implicit conversion from 'INT' to 'REAL'".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_conversion_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("conversion")
                && code_action
                    .edit
                    .as_ref()
                    .and_then(|edit| edit.changes.as_ref())
                    .and_then(|changes| changes.values().next())
                    .and_then(|edits| edits.first())
                    .map(|edit| edit.new_text.contains("INT_TO_REAL"))
                    .unwrap_or(false)
        }
        _ => false,
    });
    assert!(has_conversion_action, "expected conversion code action");
}

#[test]
fn lsp_code_action_incompatible_assignment_conversion() {
    let source = r#"
PROGRAM Test
VAR
    x : BOOL;
END_VAR
    x := 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "1;");
    let end = super::lsp_utils::offset_to_position(source, (source.find("1;").unwrap() + 1) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E203".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "cannot assign 'INT' to 'BOOL'".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_conversion_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("conversion")
                && code_action
                    .edit
                    .as_ref()
                    .and_then(|edit| edit.changes.as_ref())
                    .and_then(|changes| changes.values().next())
                    .and_then(|edits| edits.first())
                    .map(|edit| edit.new_text.contains("INT_TO_BOOL"))
                    .unwrap_or(false)
        }
        _ => false,
    });
    assert!(has_conversion_action, "expected conversion code action");
}

#[test]
fn lsp_code_action_convert_call_style() {
    let source = r#"
FUNCTION Foo : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Foo := A + B;
END_FUNCTION

PROGRAM Test
VAR
    x : INT;
END_VAR
    x := Foo(1, B := 2);
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "Foo(1");
    let end =
        super::lsp_utils::offset_to_position(source, (source.find("Foo(1").unwrap() + 3) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E205".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "formal calls cannot mix positional arguments".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_convert_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("Convert")
        }
        _ => false,
    });
    assert!(has_convert_action, "expected call style conversion action");
}

#[test]
fn lsp_code_action_reorder_positional_first_call() {
    let source = r#"
FUNCTION Foo : INT
VAR_INPUT
    A : INT;
    B : INT;
END_VAR
    Foo := A + B;
END_FUNCTION

PROGRAM Test
VAR
    x : INT;
END_VAR
    x := Foo(A := 1, 2);
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "Foo(A");
    let end =
        super::lsp_utils::offset_to_position(source, (source.find("Foo(A").unwrap() + 3) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E205".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "positional arguments must precede formal arguments".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_reorder_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action
                .title
                .contains("Reorder to positional-first call")
                && code_action
                    .edit
                    .as_ref()
                    .and_then(|edit| edit.changes.as_ref())
                    .and_then(|changes| changes.values().next())
                    .and_then(|edits| edits.first())
                    .map(|edit| edit.new_text.contains("(2, A := 1)"))
                    .unwrap_or(false)
        }
        _ => false,
    });
    assert!(
        has_reorder_action,
        "expected positional-first reorder code action"
    );
}

#[test]
fn lsp_code_action_namespace_move() {
    let source = r#"
NAMESPACE LibA
END_NAMESPACE
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "LibA\nEND_NAMESPACE");
    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range { start, end: start },
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: Vec::new(),
            only: Some(vec![tower_lsp::lsp_types::CodeActionKind::REFACTOR_REWRITE]),
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let has_move_action = actions.iter().any(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
            code_action.title.contains("Move namespace")
                && code_action
                    .command
                    .as_ref()
                    .map(|cmd| cmd.command == "editor.action.rename")
                    .unwrap_or(false)
        }
        _ => false,
    });
    assert!(has_move_action, "expected namespace move code action");
}

#[test]
fn lsp_code_action_generate_interface_stubs() {
    let source = r#"
INTERFACE IControl
    METHOD Start
    END_METHOD
END_INTERFACE

CLASS Pump IMPLEMENTS IControl
END_CLASS
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let position = position_at(source, "IMPLEMENTS IControl");
    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range {
            start: position,
            end: position,
        },
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: Vec::new(),
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let stub_action = actions.iter().find_map(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action)
            if code_action.title.contains("interface stubs") =>
        {
            Some(code_action)
        }
        _ => None,
    });
    let stub_action = stub_action.expect("stub action");
    let edits = stub_action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .and_then(|changes| changes.get(&uri))
        .expect("stub edits");
    assert!(edits
        .iter()
        .any(|edit| edit.new_text.contains("METHOD PUBLIC Start")));
}

#[test]
fn lsp_code_action_inline_variable() {
    let source = r#"
PROGRAM Test
    VAR
        x : INT := 1 + 2;
    END_VAR
    y := x;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let position = position_at(source, "x;");
    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range {
            start: position,
            end: position,
        },
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: Vec::new(),
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let inline_action = actions.iter().find_map(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action)
            if code_action.title.contains("Inline variable") =>
        {
            Some(code_action)
        }
        _ => None,
    });
    let inline_action = inline_action.expect("inline action");
    let edits = inline_action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .and_then(|changes| changes.get(&uri))
        .expect("inline edits");
    assert!(edits.iter().any(|edit| edit.new_text.contains("1 + 2")));
}

#[test]
fn lsp_code_action_extract_method() {
    let source = r#"
CLASS Controller
    METHOD Run
        x := 1;
        y := 2;
    END_METHOD
END_CLASS
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start_offset = source.find("x := 1;").expect("start");
    let end_offset = source.find("y := 2;").expect("end") + "y := 2;".len();
    let range = tower_lsp::lsp_types::Range {
        start: super::lsp_utils::offset_to_position(source, start_offset as u32),
        end: super::lsp_utils::offset_to_position(source, end_offset as u32),
    };
    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: Vec::new(),
            only: Some(vec![tower_lsp::lsp_types::CodeActionKind::REFACTOR_EXTRACT]),
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let extract_action = actions.iter().find_map(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action)
            if code_action.title.contains("Extract method") =>
        {
            Some(code_action)
        }
        _ => None,
    });
    let extract_action = extract_action.expect("extract action");
    let edits = extract_action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .and_then(|changes| changes.get(&uri))
        .expect("extract edits");
    assert!(edits
        .iter()
        .any(|edit| edit.new_text.contains("METHOD ExtractedMethod")));
}

#[test]
fn lsp_code_action_convert_function_to_function_block() {
    let source = r#"
FUNCTION Foo : INT
    Foo := 1;
END_FUNCTION

PROGRAM Main
    Foo();
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let position = position_at(source, "FUNCTION Foo");
    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range {
            start: position,
            end: position,
        },
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: Vec::new(),
            only: Some(vec![tower_lsp::lsp_types::CodeActionKind::REFACTOR_REWRITE]),
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let convert_action = actions.iter().find_map(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action)
            if code_action
                .title
                .contains("Convert FUNCTION to FUNCTION_BLOCK") =>
        {
            Some(code_action)
        }
        _ => None,
    });
    let convert_action = convert_action.expect("convert action");
    let edits = convert_action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .and_then(|changes| changes.get(&uri))
        .expect("convert edits");
    assert!(edits
        .iter()
        .any(|edit| edit.new_text.contains("FUNCTION_BLOCK")));
    assert!(edits
        .iter()
        .any(|edit| edit.new_text.contains("FooInstance")));
}

#[test]
fn lsp_code_action_convert_function_block_to_function() {
    let source = r#"
FUNCTION_BLOCK Fb
    VAR_OUTPUT
        result : INT;
    END_VAR
    result := 1;
END_FUNCTION_BLOCK
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let position = position_at(source, "FUNCTION_BLOCK Fb");
    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: tower_lsp::lsp_types::Range {
            start: position,
            end: position,
        },
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: Vec::new(),
            only: Some(vec![tower_lsp::lsp_types::CodeActionKind::REFACTOR_REWRITE]),
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let convert_action = actions.iter().find_map(|action| match action {
        tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action)
            if code_action
                .title
                .contains("Convert FUNCTION_BLOCK to FUNCTION") =>
        {
            Some(code_action)
        }
        _ => None,
    });
    let convert_action = convert_action.expect("convert action");
    let edits = convert_action
        .edit
        .as_ref()
        .and_then(|edit| edit.changes.as_ref())
        .and_then(|changes| changes.get(&uri))
        .expect("convert edits");
    assert!(edits.iter().any(|edit| edit.new_text.contains("FUNCTION")));
    assert!(edits.iter().any(|edit| edit.new_text.contains(": INT")));
}

#[test]
fn lsp_execute_command_namespace_move_workspace_edit() {
    let source = r#"
NAMESPACE LibA
TYPE Foo : INT;
END_TYPE
FUNCTION FooFunc : INT
END_FUNCTION
END_NAMESPACE
"#;
    let main_source = r#"
PROGRAM Main
    USING LibA;
    VAR
        x : LibA.Foo;
    END_VAR
    x := LibA.FooFunc();
END_PROGRAM
"#;
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);

    let namespace_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/liba.st").unwrap();
    state.open_document(namespace_uri.clone(), 1, source.to_string());

    let main_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/main.st").unwrap();
    state.open_document(main_uri.clone(), 1, main_source.to_string());

    let args = super::commands::MoveNamespaceCommandArgs {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
            uri: namespace_uri.clone(),
        },
        position: position_at(source, "LibA\nTYPE"),
        new_path: "Company.LibA".to_string(),
        target_uri: None,
    };

    let edit = namespace_move_workspace_edit(&state, args).expect("workspace edit");
    let document_changes = edit.document_changes.expect("document changes");
    let document_changes = match document_changes {
        tower_lsp::lsp_types::DocumentChanges::Operations(ops) => ops,
        _ => panic!("expected document change operations"),
    };

    let target_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/Company/LibA.st").unwrap();
    assert!(
        document_changes.iter().any(|change| {
            matches!(
                change,
                tower_lsp::lsp_types::DocumentChangeOperation::Op(
                    tower_lsp::lsp_types::ResourceOp::Create(create)
                ) if create.uri == target_uri
            )
        }),
        "expected create file for target namespace"
    );

    assert!(
        document_changes.iter().any(|change| {
            matches!(
                change,
                tower_lsp::lsp_types::DocumentChangeOperation::Op(
                    tower_lsp::lsp_types::ResourceOp::Delete(delete)
                ) if delete.uri == namespace_uri
            )
        }),
        "expected delete file for source namespace"
    );

    let target_edit = document_changes.iter().find_map(|change| match change {
        tower_lsp::lsp_types::DocumentChangeOperation::Edit(edit) => {
            if edit.text_document.uri == target_uri {
                Some(edit)
            } else {
                None
            }
        }
        _ => None,
    });
    let target_edit = target_edit.expect("target edit");
    let has_namespace_text = target_edit.edits.iter().any(|edit| match edit {
        tower_lsp::lsp_types::OneOf::Left(edit) => edit.new_text.contains("NAMESPACE Company.LibA"),
        _ => false,
    });
    assert!(has_namespace_text, "expected updated namespace text");

    let main_edit = document_changes.iter().find_map(|change| match change {
        tower_lsp::lsp_types::DocumentChangeOperation::Edit(edit) => {
            if edit.text_document.uri == main_uri {
                Some(edit)
            } else {
                None
            }
        }
        _ => None,
    });
    let main_edit = main_edit.expect("main edit");
    let has_using_update = main_edit.edits.iter().any(|edit| match edit {
        tower_lsp::lsp_types::OneOf::Left(edit) => edit.new_text.contains("Company.LibA"),
        _ => false,
    });
    assert!(has_using_update, "expected USING update");
}

#[test]
fn lsp_project_info_exposes_build_and_targets() {
    let state = ServerState::new();
    let root_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/").unwrap();
    state.set_workspace_folders(vec![root_uri.clone()]);
    state.set_workspace_config(
        root_uri.clone(),
        ProjectConfig {
            root: PathBuf::from("/workspace"),
            config_path: Some(PathBuf::from("/workspace/trust-lsp.toml")),
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: vec![LibrarySpec {
                name: "Core".to_string(),
                path: PathBuf::from("/workspace/libs/core"),
                version: Some("1.0".to_string()),
                dependencies: vec![LibraryDependency {
                    name: "Utils".to_string(),
                    version: None,
                }],
                docs: Vec::new(),
            }],
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig {
                target: Some("x86_64".to_string()),
                profile: Some("release".to_string()),
                flags: vec!["-O2".to_string()],
                defines: vec!["SIM=1".to_string()],
            },
            targets: vec![TargetProfile {
                name: "sim".to_string(),
                profile: Some("debug".to_string()),
                flags: vec!["-g".to_string()],
                defines: vec!["TRACE=1".to_string()],
            }],
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        },
    );

    let info = super::commands::project_info_value(&state, Vec::new()).expect("project info");
    let projects = info
        .get("projects")
        .and_then(|value| value.as_array())
        .expect("projects array");
    assert_eq!(projects.len(), 1);
    let project = &projects[0];
    let build = project.get("build").expect("build");
    assert_eq!(build.get("target").and_then(|v| v.as_str()), Some("x86_64"));
    assert_eq!(
        build.get("profile").and_then(|v| v.as_str()),
        Some("release")
    );
    let targets = project
        .get("targets")
        .and_then(|value| value.as_array())
        .expect("targets");
    assert!(targets.iter().any(|target| {
        target.get("name").and_then(|v| v.as_str()) == Some("sim")
            && target.get("profile").and_then(|v| v.as_str()) == Some("debug")
    }));
    let libraries = project
        .get("libraries")
        .and_then(|value| value.as_array())
        .expect("libraries");
    assert!(libraries.iter().any(|lib| {
        lib.get("name").and_then(|v| v.as_str()) == Some("Core")
            && lib.get("version").and_then(|v| v.as_str()) == Some("1.0")
    }));
}

#[test]
fn lsp_code_action_namespace_disambiguation() {
    let source = r#"
NAMESPACE LibA
FUNCTION Foo : INT
END_FUNCTION
END_NAMESPACE

NAMESPACE LibB
FUNCTION Foo : INT
END_FUNCTION
END_NAMESPACE

PROGRAM Main
    USING LibA;
    USING LibB;
    VAR
        x : INT;
    END_VAR
    x := Foo();
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let start = position_at(source, "Foo()");
    let end =
        super::lsp_utils::offset_to_position(source, (source.find("Foo()").unwrap() + 3) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E105".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "ambiguous reference to 'Foo'; qualify the name".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let mut titles = actions
        .iter()
        .filter_map(|action| match action {
            tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
                Some(code_action.title.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    titles.sort();
    assert!(
        titles.iter().any(|title| title.contains("LibA.Foo")),
        "expected LibA qualification quick fix"
    );
    assert!(
        titles.iter().any(|title| title.contains("LibB.Foo")),
        "expected LibB qualification quick fix"
    );
}

#[test]
fn lsp_code_action_namespace_disambiguation_project_using() {
    let lib_a = r#"
NAMESPACE LibA
FUNCTION Foo : INT
END_FUNCTION
END_NAMESPACE
"#;
    let lib_b = r#"
NAMESPACE LibB
FUNCTION Foo : INT
END_FUNCTION
END_NAMESPACE
"#;
    let main = r#"
USING LibA;
USING LibB;

PROGRAM Main
    VAR
        x : INT;
    END_VAR
    x := Foo();
END_PROGRAM
"#;
    let state = ServerState::new();
    let lib_a_uri = tower_lsp::lsp_types::Url::parse("file:///liba.st").unwrap();
    let lib_b_uri = tower_lsp::lsp_types::Url::parse("file:///libb.st").unwrap();
    let main_uri = tower_lsp::lsp_types::Url::parse("file:///main.st").unwrap();
    state.open_document(lib_a_uri, 1, lib_a.to_string());
    state.open_document(lib_b_uri, 1, lib_b.to_string());
    state.open_document(main_uri.clone(), 1, main.to_string());

    let start = position_at(main, "Foo()");
    let end = super::lsp_utils::offset_to_position(main, (main.find("Foo()").unwrap() + 3) as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E105".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "ambiguous reference to 'Foo'; qualify the name".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: main_uri },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let mut titles = actions
        .iter()
        .filter_map(|action| match action {
            tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
                Some(code_action.title.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    titles.sort();
    assert!(
        titles.iter().any(|title| title.contains("LibA.Foo")),
        "expected LibA qualification quick fix"
    );
    assert!(
        titles.iter().any(|title| title.contains("LibB.Foo")),
        "expected LibB qualification quick fix"
    );
}

#[test]
fn lsp_golden_multi_root_protocol_snapshot() {
    let source_main = r#"
CONFIGURATION Conf
VAR_GLOBAL CONSTANT
    ANSWER : INT := 42;
END_VAR
END_CONFIGURATION

TYPE MyInt : INT;
END_TYPE

USING Lib;

NAMESPACE Lib
FUNCTION Foo : INT
VAR_INPUT
    a : INT;
END_VAR
Foo := a;
END_FUNCTION
END_NAMESPACE

INTERFACE IFace
METHOD Do : INT;
END_METHOD
END_INTERFACE

CLASS Base
END_CLASS

CLASS Derived EXTENDS Base IMPLEMENTS IFace
METHOD Do : INT
    Do := Lib.Foo(ANSWER);
END_METHOD
END_CLASS

PROGRAM Main
VAR
    x : INT;
    y : INT;
    typed : MyInt;
END_VAR
x := Lib.Foo(ANSWER);
END_PROGRAM
"#;

    let source_aux = r#"
PROGRAM Aux
VAR
    counter : INT;
END_VAR
counter := counter + 1;
END_PROGRAM
"#;

    let config_source = r#"
[project]
include_paths = ["src"]
library_paths = ["libs"]

[[libraries]]
name = "Vendor"
path = "vendor"
"#;

    let state = ServerState::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let client = test_client();
    let root_one = PathBuf::from("/workspace/golden/alpha");
    let root_two = PathBuf::from("/workspace/golden/beta");
    let root_one_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/golden/alpha").unwrap();
    let root_two_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/golden/beta").unwrap();
    state.set_workspace_folders(vec![root_one_uri.clone(), root_two_uri.clone()]);
    state.set_workspace_config(
        root_one_uri.clone(),
        ProjectConfig {
            root: root_one.clone(),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings {
                priority: 10,
                visibility: crate::config::WorkspaceVisibility::Public,
            },
            telemetry: TelemetryConfig::default(),
        },
    );
    state.set_workspace_config(
        root_two_uri.clone(),
        ProjectConfig {
            root: root_two.clone(),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig::default(),
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings {
                priority: 1,
                visibility: crate::config::WorkspaceVisibility::Private,
            },
            telemetry: TelemetryConfig::default(),
        },
    );

    let main_uri =
        tower_lsp::lsp_types::Url::parse("file:///workspace/golden/alpha/Main.st").unwrap();
    let aux_uri = tower_lsp::lsp_types::Url::parse("file:///workspace/golden/beta/Aux.st").unwrap();
    let config_uri =
        tower_lsp::lsp_types::Url::parse("file:///workspace/golden/alpha/trust-lsp.toml").unwrap();
    state.open_document(main_uri.clone(), 1, source_main.to_string());
    state.open_document(aux_uri.clone(), 1, source_aux.to_string());
    state.open_document(config_uri.clone(), 1, config_source.to_string());

    let hover_params = tower_lsp::lsp_types::HoverParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: position_at(source_main, "Foo(ANSWER"),
        },
        work_done_progress_params: Default::default(),
    };
    let hover_result = hover(&state, hover_params);

    let completion_position = {
        let mut pos = position_at(source_main, "Lib.Foo");
        pos.character += 4;
        pos
    };
    let completion_params = tower_lsp::lsp_types::CompletionParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: completion_position,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        context: None,
    };
    let completion_result = completion(&state, completion_params);
    let completion_resolve_result = completion_result.as_ref().and_then(|response| {
        let first = match response {
            tower_lsp::lsp_types::CompletionResponse::Array(items) => items.first().cloned(),
            tower_lsp::lsp_types::CompletionResponse::List(list) => list.items.first().cloned(),
        }?;
        Some(completion_resolve(&state, first))
    });

    let signature_params = tower_lsp::lsp_types::SignatureHelpParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: {
                let mut pos = position_at(source_main, "Foo(ANSWER");
                pos.character += 4;
                pos
            },
        },
        context: None,
        work_done_progress_params: Default::default(),
    };
    let signature_result = signature_help(&state, signature_params);

    let def_params = tower_lsp::lsp_types::GotoDefinitionParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: position_at(source_main, "Foo(ANSWER"),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let def_result = goto_definition(&state, def_params);

    let decl_result = goto_declaration(
        &state,
        tower_lsp::lsp_types::request::GotoDeclarationParams {
            text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "Foo(ANSWER"),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let type_def_result = goto_type_definition(
        &state,
        tower_lsp::lsp_types::request::GotoTypeDefinitionParams {
            text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "typed : MyInt"),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let impl_result = goto_implementation(
        &state,
        tower_lsp::lsp_types::request::GotoImplementationParams {
            text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "IFace"),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let ref_params = tower_lsp::lsp_types::ReferenceParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: position_at(source_main, "Foo(ANSWER"),
        },
        context: tower_lsp::lsp_types::ReferenceContext {
            include_declaration: true,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let ref_result = references(&state, ref_params);

    let highlight_params = tower_lsp::lsp_types::DocumentHighlightParams {
        text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: position_at(source_main, "x : INT"),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let highlight_result = document_highlight(&state, highlight_params);

    let doc_symbol_params = tower_lsp::lsp_types::DocumentSymbolParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
            uri: main_uri.clone(),
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let doc_symbol_result = document_symbol(&state, doc_symbol_params);

    let workspace_symbol_empty = workspace_symbol(
        &state,
        tower_lsp::lsp_types::WorkspaceSymbolParams {
            query: "".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );
    let workspace_symbol_aux = workspace_symbol(
        &state,
        tower_lsp::lsp_types::WorkspaceSymbolParams {
            query: "Aux".to_string(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let diagnostic_params = tower_lsp::lsp_types::DocumentDiagnosticParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
            uri: main_uri.clone(),
        },
        identifier: None,
        previous_result_id: None,
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };
    let diagnostic_result = document_diagnostic(&state, diagnostic_params);

    let workspace_diag_params = tower_lsp::lsp_types::WorkspaceDiagnosticParams {
        previous_result_ids: Vec::new(),
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
        identifier: None,
    };
    let workspace_diag_result = workspace_diagnostic(&state, workspace_diag_params);

    let diagnostic_items = match &diagnostic_result {
        tower_lsp::lsp_types::DocumentDiagnosticReportResult::Report(
            tower_lsp::lsp_types::DocumentDiagnosticReport::Full(full),
        ) => full.full_document_diagnostic_report.items.clone(),
        _ => Vec::new(),
    };
    let unused_diag = diagnostic_items
        .iter()
        .find(|diag| {
            diag.code.as_ref().is_some_and(|code| match code {
                tower_lsp::lsp_types::NumberOrString::String(value) => value == "W001",
                _ => false,
            })
        })
        .cloned();
    let code_action_result = unused_diag.as_ref().and_then(|diag| {
        let params = tower_lsp::lsp_types::CodeActionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            range: diag.range,
            context: tower_lsp::lsp_types::CodeActionContext {
                diagnostics: vec![diag.clone()],
                only: None,
                trigger_kind: None,
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        code_action(&state, params)
    });

    let code_lens_result = code_lens(
        &state,
        tower_lsp::lsp_types::CodeLensParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let call_hierarchy_items = prepare_call_hierarchy(
        &state,
        tower_lsp::lsp_types::CallHierarchyPrepareParams {
            text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "Foo : INT"),
            },
            work_done_progress_params: Default::default(),
        },
    )
    .unwrap_or_default();
    let call_hierarchy_incoming = call_hierarchy_items.first().and_then(|item| {
        incoming_calls(
            &state,
            tower_lsp::lsp_types::CallHierarchyIncomingCallsParams {
                item: item.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
    });
    let call_hierarchy_outgoing = call_hierarchy_items.first().and_then(|item| {
        outgoing_calls(
            &state,
            tower_lsp::lsp_types::CallHierarchyOutgoingCallsParams {
                item: item.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
    });

    let type_hierarchy_items = prepare_type_hierarchy(
        &state,
        tower_lsp::lsp_types::TypeHierarchyPrepareParams {
            text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "Derived"),
            },
            work_done_progress_params: Default::default(),
        },
    )
    .unwrap_or_default();
    let type_hierarchy_supertypes = type_hierarchy_items.first().and_then(|item| {
        type_hierarchy_supertypes(
            &state,
            tower_lsp::lsp_types::TypeHierarchySupertypesParams {
                item: item.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
    });
    let type_hierarchy_subtypes = type_hierarchy_items.first().and_then(|item| {
        type_hierarchy_subtypes(
            &state,
            tower_lsp::lsp_types::TypeHierarchySubtypesParams {
                item: item.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
        )
    });

    let rename_params = tower_lsp::lsp_types::RenameParams {
        text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: position_at(source_main, "x : INT"),
        },
        new_name: "counter".to_string(),
        work_done_progress_params: Default::default(),
    };
    let rename_result = rename(&state, rename_params);

    let prepare_rename_result = prepare_rename(
        &state,
        tower_lsp::lsp_types::TextDocumentPositionParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            position: position_at(source_main, "x : INT"),
        },
    );

    let semantic_full = semantic_tokens_full(
        &state,
        tower_lsp::lsp_types::SemanticTokensParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );
    let previous_result_id = semantic_full.as_ref().and_then(|result| match result {
        tower_lsp::lsp_types::SemanticTokensResult::Tokens(tokens) => tokens.result_id.clone(),
        _ => None,
    });
    let semantic_delta = semantic_tokens_full_delta(
        &state,
        tower_lsp::lsp_types::SemanticTokensDeltaParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            previous_result_id: previous_result_id.unwrap_or_else(|| "0".to_string()),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );
    let semantic_range = semantic_tokens_range(
        &state,
        tower_lsp::lsp_types::SemanticTokensRangeParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            range: tower_lsp::lsp_types::Range {
                start: position_at(source_main, "PROGRAM Main"),
                end: position_at(source_main, "END_PROGRAM"),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let folding_result = folding_range(
        &state,
        tower_lsp::lsp_types::FoldingRangeParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );
    let selection_result = selection_range(
        &state,
        tower_lsp::lsp_types::SelectionRangeParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            positions: vec![position_at(source_main, "Lib.Foo(ANSWER)")],
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );
    let linked_editing_result = linked_editing_range(
        &state,
        tower_lsp::lsp_types::LinkedEditingRangeParams {
            text_document_position_params: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "Foo : INT"),
            },
            work_done_progress_params: Default::default(),
        },
    );
    let document_link_st = document_link(
        &state,
        tower_lsp::lsp_types::DocumentLinkParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );
    let document_link_config = document_link(
        &state,
        tower_lsp::lsp_types::DocumentLinkParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: config_uri },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        },
    );

    let inlay_result = inlay_hint(
        &state,
        tower_lsp::lsp_types::InlayHintParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            range: tower_lsp::lsp_types::Range {
                start: position_at(source_main, "x := Lib.Foo"),
                end: position_at(source_main, "END_PROGRAM"),
            },
            work_done_progress_params: Default::default(),
        },
    );

    let inline_value_result = inline_value(
        &state,
        tower_lsp::lsp_types::InlineValueParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            range: tower_lsp::lsp_types::Range {
                start: position_at(source_main, "PROGRAM Main"),
                end: position_at(source_main, "END_PROGRAM"),
            },
            context: tower_lsp::lsp_types::InlineValueContext {
                frame_id: 1,
                stopped_location: tower_lsp::lsp_types::Range {
                    start: tower_lsp::lsp_types::Position::new(0, 0),
                    end: tower_lsp::lsp_types::Position::new(0, 0),
                },
            },
            work_done_progress_params: Default::default(),
        },
    );

    let formatting_result = formatting(
        &state,
        tower_lsp::lsp_types::DocumentFormattingParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            options: tower_lsp::lsp_types::FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        },
    );
    let range_formatting_result = range_formatting(
        &state,
        tower_lsp::lsp_types::DocumentRangeFormattingParams {
            text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                uri: main_uri.clone(),
            },
            range: tower_lsp::lsp_types::Range {
                start: position_at(source_main, "PROGRAM Main"),
                end: position_at(source_main, "END_PROGRAM"),
            },
            options: tower_lsp::lsp_types::FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        },
    );
    let on_type_formatting_result = on_type_formatting(
        &state,
        tower_lsp::lsp_types::DocumentOnTypeFormattingParams {
            text_document_position: tower_lsp::lsp_types::TextDocumentPositionParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                    uri: main_uri.clone(),
                },
                position: position_at(source_main, "x := Lib.Foo"),
            },
            ch: ";".to_string(),
            options: tower_lsp::lsp_types::FormattingOptions {
                tab_size: 4,
                insert_spaces: true,
                ..Default::default()
            },
        },
    );

    let will_rename_result = will_rename_files(
        &state,
        tower_lsp::lsp_types::RenameFilesParams {
            files: vec![tower_lsp::lsp_types::FileRename {
                old_uri: main_uri.to_string(),
                new_uri: "file:///workspace/golden/alpha/MainRenamed.st".to_string(),
            }],
        },
    );

    let execute_command_result = runtime.block_on(execute_command(
        &client,
        &state,
        tower_lsp::lsp_types::ExecuteCommandParams {
            command: PROJECT_INFO_COMMAND.to_string(),
            arguments: vec![json!({ "root_uri": root_one_uri })],
            work_done_progress_params: Default::default(),
        },
    ));

    let notify_summary = {
        let notify_state = Arc::new(ServerState::new());
        let notify_source = r#"
PROGRAM Notify
VAR
    x : INT;
END_VAR
x := 1;
END_PROGRAM
"#;
        let notify_uri =
            tower_lsp::lsp_types::Url::parse("file:///workspace/golden/notify/Notify.st").unwrap();
        let watch_dir = temp_dir("lsp-watch");
        let watch_path = watch_dir.join("Watch.st");
        let watch_source = "PROGRAM Watch\nEND_PROGRAM\n";
        std::fs::write(&watch_path, watch_source).expect("write watch source");
        let watch_uri = tower_lsp::lsp_types::Url::from_file_path(&watch_path).unwrap();

        runtime.block_on(async {
            did_open(
                &client,
                &notify_state,
                tower_lsp::lsp_types::DidOpenTextDocumentParams {
                    text_document: tower_lsp::lsp_types::TextDocumentItem {
                        uri: notify_uri.clone(),
                        language_id: "st".to_string(),
                        version: 1,
                        text: notify_source.to_string(),
                    },
                },
            )
            .await;
            let after_open = document_snapshot(&notify_state, &notify_uri);

            let change_pos = position_at(notify_source, "1;");
            did_change(
                &client,
                &notify_state,
                tower_lsp::lsp_types::DidChangeTextDocumentParams {
                    text_document: tower_lsp::lsp_types::VersionedTextDocumentIdentifier {
                        uri: notify_uri.clone(),
                        version: 2,
                    },
                    content_changes: vec![tower_lsp::lsp_types::TextDocumentContentChangeEvent {
                        range: Some(tower_lsp::lsp_types::Range {
                            start: change_pos,
                            end: tower_lsp::lsp_types::Position::new(
                                change_pos.line,
                                change_pos.character + 1,
                            ),
                        }),
                        range_length: None,
                        text: "2".to_string(),
                    }],
                },
            )
            .await;
            let after_change = document_snapshot(&notify_state, &notify_uri);

            did_save(
                &client,
                &notify_state,
                tower_lsp::lsp_types::DidSaveTextDocumentParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                        uri: notify_uri.clone(),
                    },
                    text: None,
                },
            )
            .await;
            let after_save = document_snapshot(&notify_state, &notify_uri);

            did_close(
                &client,
                &notify_state,
                tower_lsp::lsp_types::DidCloseTextDocumentParams {
                    text_document: tower_lsp::lsp_types::TextDocumentIdentifier {
                        uri: notify_uri.clone(),
                    },
                },
            )
            .await;
            let after_close = document_snapshot(&notify_state, &notify_uri);

            let config_value = json!({
                "trust-lsp": {
                    "formatting": { "indent_width": 2 },
                    "diagnostics": { "showIecReferences": true }
                }
            });
            did_change_configuration(
                &notify_state,
                tower_lsp::lsp_types::DidChangeConfigurationParams {
                    settings: config_value.clone(),
                },
            );

            did_change_watched_files(
                &client,
                &notify_state,
                tower_lsp::lsp_types::DidChangeWatchedFilesParams {
                    changes: vec![tower_lsp::lsp_types::FileEvent {
                        uri: watch_uri.clone(),
                        typ: tower_lsp::lsp_types::FileChangeType::CREATED,
                    }],
                },
            )
            .await;
            let after_watch_create = document_snapshot(&notify_state, &watch_uri);

            did_change_watched_files(
                &client,
                &notify_state,
                tower_lsp::lsp_types::DidChangeWatchedFilesParams {
                    changes: vec![tower_lsp::lsp_types::FileEvent {
                        uri: watch_uri.clone(),
                        typ: tower_lsp::lsp_types::FileChangeType::DELETED,
                    }],
                },
            )
            .await;
            let after_watch_delete = document_snapshot(&notify_state, &watch_uri);

            json!({
                "didOpen": after_open,
                "didChange": after_change,
                "didSave": after_save,
                "didClose": after_close,
                "didChangeConfiguration": notify_state.config(),
                "didChangeWatchedFiles": {
                    "afterCreate": after_watch_create,
                    "afterDelete": after_watch_delete,
                }
            })
        })
    };

    let mut output = serde_json::Map::new();
    output.insert(
        "hover".to_string(),
        serde_json::to_value(&hover_result).unwrap(),
    );
    output.insert(
        "completion".to_string(),
        serde_json::to_value(&completion_result).unwrap(),
    );
    output.insert(
        "completionResolve".to_string(),
        serde_json::to_value(&completion_resolve_result).unwrap(),
    );
    output.insert(
        "signatureHelp".to_string(),
        serde_json::to_value(&signature_result).unwrap(),
    );
    output.insert(
        "definition".to_string(),
        serde_json::to_value(&def_result).unwrap(),
    );
    output.insert(
        "declaration".to_string(),
        serde_json::to_value(&decl_result).unwrap(),
    );
    output.insert(
        "typeDefinition".to_string(),
        serde_json::to_value(&type_def_result).unwrap(),
    );
    output.insert(
        "implementation".to_string(),
        serde_json::to_value(&impl_result).unwrap(),
    );
    output.insert(
        "references".to_string(),
        serde_json::to_value(&ref_result).unwrap(),
    );
    output.insert(
        "documentHighlight".to_string(),
        serde_json::to_value(&highlight_result).unwrap(),
    );
    output.insert(
        "documentSymbol".to_string(),
        serde_json::to_value(&doc_symbol_result).unwrap(),
    );
    output.insert(
        "workspaceSymbolEmpty".to_string(),
        serde_json::to_value(&workspace_symbol_empty).unwrap(),
    );
    output.insert(
        "workspaceSymbolAux".to_string(),
        serde_json::to_value(&workspace_symbol_aux).unwrap(),
    );
    output.insert(
        "documentDiagnostic".to_string(),
        serde_json::to_value(&diagnostic_result).unwrap(),
    );
    output.insert(
        "workspaceDiagnostic".to_string(),
        serde_json::to_value(&workspace_diag_result).unwrap(),
    );
    output.insert(
        "codeAction".to_string(),
        serde_json::to_value(&code_action_result).unwrap(),
    );
    output.insert(
        "codeLens".to_string(),
        serde_json::to_value(&code_lens_result).unwrap(),
    );
    output.insert(
        "callHierarchyItems".to_string(),
        serde_json::to_value(&call_hierarchy_items).unwrap(),
    );
    output.insert(
        "callHierarchyIncoming".to_string(),
        serde_json::to_value(&call_hierarchy_incoming).unwrap(),
    );
    output.insert(
        "callHierarchyOutgoing".to_string(),
        serde_json::to_value(&call_hierarchy_outgoing).unwrap(),
    );
    output.insert(
        "typeHierarchyItems".to_string(),
        serde_json::to_value(&type_hierarchy_items).unwrap(),
    );
    output.insert(
        "typeHierarchySupertypes".to_string(),
        serde_json::to_value(&type_hierarchy_supertypes).unwrap(),
    );
    output.insert(
        "typeHierarchySubtypes".to_string(),
        serde_json::to_value(&type_hierarchy_subtypes).unwrap(),
    );
    output.insert(
        "rename".to_string(),
        serde_json::to_value(&rename_result).unwrap(),
    );
    output.insert(
        "prepareRename".to_string(),
        serde_json::to_value(&prepare_rename_result).unwrap(),
    );
    output.insert(
        "semanticTokensFull".to_string(),
        serde_json::to_value(&semantic_full).unwrap(),
    );
    output.insert(
        "semanticTokensDelta".to_string(),
        serde_json::to_value(&semantic_delta).unwrap(),
    );
    output.insert(
        "semanticTokensRange".to_string(),
        serde_json::to_value(&semantic_range).unwrap(),
    );
    output.insert(
        "foldingRange".to_string(),
        serde_json::to_value(&folding_result).unwrap(),
    );
    output.insert(
        "selectionRange".to_string(),
        serde_json::to_value(&selection_result).unwrap(),
    );
    output.insert(
        "linkedEditingRange".to_string(),
        serde_json::to_value(&linked_editing_result).unwrap(),
    );
    output.insert(
        "documentLinkSt".to_string(),
        serde_json::to_value(&document_link_st).unwrap(),
    );
    output.insert(
        "documentLinkConfig".to_string(),
        serde_json::to_value(&document_link_config).unwrap(),
    );
    output.insert(
        "inlayHint".to_string(),
        serde_json::to_value(&inlay_result).unwrap(),
    );
    output.insert(
        "inlineValue".to_string(),
        serde_json::to_value(&inline_value_result).unwrap(),
    );
    output.insert(
        "formatting".to_string(),
        serde_json::to_value(&formatting_result).unwrap(),
    );
    output.insert(
        "rangeFormatting".to_string(),
        serde_json::to_value(&range_formatting_result).unwrap(),
    );
    output.insert(
        "onTypeFormatting".to_string(),
        serde_json::to_value(&on_type_formatting_result).unwrap(),
    );
    output.insert(
        "willRenameFiles".to_string(),
        serde_json::to_value(&will_rename_result).unwrap(),
    );
    output.insert(
        "executeCommandProjectInfo".to_string(),
        serde_json::to_value(&execute_command_result).unwrap(),
    );
    output.insert("notifyWorkflows".to_string(), notify_summary);

    let output = Value::Object(output);
    let output = serde_json::to_string_pretty(&output).expect("serialize snapshot");
    assert_snapshot!(output);
}

#[test]
fn lsp_code_action_namespace_disambiguation_non_call() {
    let source = r#"
NAMESPACE LibA
TYPE Foo : INT;
END_TYPE
END_NAMESPACE

NAMESPACE LibB
TYPE Foo : INT;
END_TYPE
END_NAMESPACE

PROGRAM Main
    USING LibA;
    USING LibB;
    VAR
        x : Foo;
    END_VAR
    x := 1;
END_PROGRAM
"#;
    let state = ServerState::new();
    let uri = tower_lsp::lsp_types::Url::parse("file:///test.st").unwrap();
    state.open_document(uri.clone(), 1, source.to_string());

    let type_offset = source.find("x : Foo;").expect("type reference");
    let foo_start = type_offset + "x : ".len();
    let foo_end = foo_start + "Foo".len();
    let start = super::lsp_utils::offset_to_position(source, foo_start as u32);
    let end = super::lsp_utils::offset_to_position(source, foo_end as u32);

    let diagnostic = tower_lsp::lsp_types::Diagnostic {
        range: tower_lsp::lsp_types::Range { start, end },
        severity: Some(tower_lsp::lsp_types::DiagnosticSeverity::ERROR),
        code: Some(tower_lsp::lsp_types::NumberOrString::String(
            "E105".to_string(),
        )),
        source: Some("trust-lsp".to_string()),
        message: "ambiguous reference to 'Foo'; qualify the name".to_string(),
        ..Default::default()
    };

    let params = tower_lsp::lsp_types::CodeActionParams {
        text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: uri.clone() },
        range: diagnostic.range,
        context: tower_lsp::lsp_types::CodeActionContext {
            diagnostics: vec![diagnostic],
            only: None,
            trigger_kind: None,
        },
        work_done_progress_params: Default::default(),
        partial_result_params: Default::default(),
    };

    let actions = code_action(&state, params).expect("code actions");
    let mut titles = actions
        .iter()
        .filter_map(|action| match action {
            tower_lsp::lsp_types::CodeActionOrCommand::CodeAction(code_action) => {
                Some(code_action.title.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    titles.sort();
    assert!(
        titles.iter().any(|title| title.contains("LibA.Foo")),
        "expected LibA qualification quick fix"
    );
    assert!(
        titles.iter().any(|title| title.contains("LibB.Foo")),
        "expected LibB qualification quick fix"
    );
}
