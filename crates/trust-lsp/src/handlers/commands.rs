//! LSP workspace/executeCommand handlers.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use tower_lsp::lsp_types::{
    CreateFile, CreateFileOptions, DeleteFile, DeleteFileOptions, DocumentChangeOperation,
    DocumentChanges, ExecuteCommandParams, OptionalVersionedTextDocumentIdentifier, Position,
    Range, ResourceOp, TextDocumentEdit, TextDocumentIdentifier, TextEdit, Url, WorkspaceEdit,
};
use tower_lsp::Client;

use text_size::{TextRange, TextSize};
use trust_ide::refactor::parse_namespace_path;
use trust_ide::rename::{RenameResult, TextEdit as IdeTextEdit};
use trust_syntax::parser::parse;
use trust_syntax::syntax::{SyntaxKind, SyntaxNode};

use crate::handlers::context::ServerContext;
use crate::handlers::lsp_utils::{offset_to_position, position_to_offset};
use crate::library_graph::build_library_graph;
use crate::state::{path_to_uri, uri_to_path, ServerState};

pub const MOVE_NAMESPACE_COMMAND: &str = "trust-lsp.moveNamespace";
pub const PROJECT_INFO_COMMAND: &str = "trust-lsp.projectInfo";

#[derive(Debug, Deserialize)]
pub struct MoveNamespaceCommandArgs {
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
    pub new_path: String,
    #[serde(default)]
    pub target_uri: Option<Url>,
}

#[derive(Debug, Deserialize)]
struct ProjectInfoCommandArgs {
    #[serde(default)]
    root_uri: Option<Url>,
    #[serde(default)]
    text_document: Option<TextDocumentIdentifier>,
}

pub async fn execute_command(
    client: &Client,
    state: &ServerState,
    params: ExecuteCommandParams,
) -> Option<Value> {
    match params.command.as_str() {
        MOVE_NAMESPACE_COMMAND => {
            let args = parse_move_namespace_args(params.arguments)?;
            let edit = namespace_move_workspace_edit(state, args)?;
            let response = client.apply_edit(edit).await.ok()?;
            Some(json!(response.applied))
        }
        PROJECT_INFO_COMMAND => project_info_value(state, params.arguments),
        _ => None,
    }
}

fn parse_move_namespace_args(args: Vec<Value>) -> Option<MoveNamespaceCommandArgs> {
    if args.len() != 1 {
        return None;
    }
    serde_json::from_value(args.into_iter().next()?).ok()
}

pub(crate) fn project_info_value(state: &ServerState, args: Vec<Value>) -> Option<Value> {
    project_info_value_with_context(state, args)
}

fn project_info_value_with_context<C: ServerContext>(
    context: &C,
    args: Vec<Value>,
) -> Option<Value> {
    let mut configs = context.workspace_configs();
    if args.len() == 1 {
        if let Ok(parsed) = serde_json::from_value::<ProjectInfoCommandArgs>(
            args.into_iter().next().unwrap_or(Value::Null),
        ) {
            if let Some(root_uri) = parsed.root_uri {
                configs.retain(|(root, _)| root == &root_uri);
            } else if let Some(text_document) = parsed.text_document {
                if let Some(config) = context.workspace_config_for_uri(&text_document.uri) {
                    let root_uri = path_to_uri(&config.root).unwrap_or(text_document.uri.clone());
                    configs = vec![(root_uri, config)];
                }
            }
        }
    }

    let projects: Vec<Value> = configs
        .into_iter()
        .map(|(root, config)| project_info_for_config(&root, &config))
        .collect();

    Some(json!({ "projects": projects }))
}

fn project_info_for_config(root: &Url, config: &crate::config::ProjectConfig) -> Value {
    let graph = build_library_graph(config);
    let libraries: Vec<Value> = graph
        .nodes
        .into_iter()
        .map(|node| {
            let dependencies: Vec<Value> = node
                .dependencies
                .into_iter()
                .map(|dep| {
                    json!({
                        "name": dep.name,
                        "version": dep.version,
                    })
                })
                .collect();
            json!({
                "name": node.name,
                "version": node.version,
                "path": node.path.display().to_string(),
                "dependencies": dependencies,
            })
        })
        .collect();

    let targets: Vec<Value> = config
        .targets
        .iter()
        .map(|target| {
            json!({
                "name": target.name,
                "profile": target.profile,
                "flags": target.flags,
                "defines": target.defines,
            })
        })
        .collect();

    json!({
        "root": root.to_string(),
        "configPath": config.config_path.as_ref().map(|path| path.display().to_string()),
        "build": {
            "target": config.build.target,
            "profile": config.build.profile,
            "flags": config.build.flags,
            "defines": config.build.defines,
        },
        "targets": targets,
        "libraries": libraries,
    })
}

pub(crate) fn namespace_move_workspace_edit(
    state: &ServerState,
    args: MoveNamespaceCommandArgs,
) -> Option<WorkspaceEdit> {
    namespace_move_workspace_edit_with_context(state, args)
}

fn namespace_move_workspace_edit_with_context<C: ServerContext>(
    context: &C,
    args: MoveNamespaceCommandArgs,
) -> Option<WorkspaceEdit> {
    let uri = &args.text_document.uri;
    let doc = context.get_document(uri)?;
    let offset = position_to_offset(&doc.content, args.position)?;
    let parsed = parse(&doc.content);
    let root = parsed.syntax();
    let range = TextRange::new(TextSize::from(offset), TextSize::from(offset));
    let namespace_node = find_enclosing_node_of_kind(&root, range, SyntaxKind::Namespace)?;
    let namespace_range = namespace_node.text_range();
    let removal_range = expand_to_full_lines(&doc.content, namespace_range);
    let namespace_text =
        doc.content[namespace_range.start().into()..namespace_range.end().into()].to_string();
    let new_path_parts = parse_namespace_path(&args.new_path)?;
    let target_uri = match args.target_uri {
        Some(uri) => uri,
        None => derive_target_uri(context, &new_path_parts)?,
    };

    let mut rename_result = context.rename(doc.file_id, TextSize::from(offset), &args.new_path)?;

    let relocating = uri != &target_uri;
    let mut updated_namespace_text = namespace_text;
    if relocating {
        if let Some(source_edits) = rename_result.edits.remove(&doc.file_id) {
            let (inside, outside): (Vec<IdeTextEdit>, Vec<IdeTextEdit>) = source_edits
                .into_iter()
                .partition(|edit| ranges_overlap(edit.range, namespace_range));
            if !inside.is_empty() {
                let base = namespace_range.start();
                let adjusted = inside
                    .into_iter()
                    .filter_map(|edit| shift_edit_range(edit, base))
                    .collect::<Vec<_>>();
                updated_namespace_text = apply_text_edits(&updated_namespace_text, &adjusted);
            }
            if !outside.is_empty() {
                rename_result.edits.insert(doc.file_id, outside);
            }
        }
    }

    let mut text_changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
    add_rename_edits_to_changes(context, rename_result, &mut text_changes);

    let mut delete_source = false;
    if relocating {
        let mut remaining = String::new();
        remaining.push_str(&doc.content[0..usize::from(removal_range.start())]);
        remaining.push_str(&doc.content[usize::from(removal_range.end())..]);
        if remaining.trim().is_empty() {
            delete_source = true;
        } else {
            let edit = TextEdit {
                range: Range {
                    start: offset_to_position(&doc.content, removal_range.start().into()),
                    end: offset_to_position(&doc.content, removal_range.end().into()),
                },
                new_text: String::new(),
            };
            text_changes.entry(uri.clone()).or_default().push(edit);
        }
    }

    if delete_source {
        text_changes.remove(uri);
    }

    let target_content = load_document_content(context, &target_uri).unwrap_or_default();
    if relocating {
        let insert_offset = target_content.len() as u32;
        let insert_pos = offset_to_position(&target_content, insert_offset);
        let insert_text = build_namespace_insert_text(&target_content, &updated_namespace_text);
        let edit = TextEdit {
            range: Range {
                start: insert_pos,
                end: insert_pos,
            },
            new_text: insert_text,
        };
        text_changes
            .entry(target_uri.clone())
            .or_default()
            .push(edit);
    }

    let mut document_changes = Vec::new();
    let create_target = relocating && !uri_exists(context, &target_uri);
    if create_target {
        document_changes.push(DocumentChangeOperation::Op(ResourceOp::Create(
            CreateFile {
                uri: target_uri.clone(),
                options: Some(CreateFileOptions {
                    overwrite: Some(false),
                    ignore_if_exists: Some(true),
                }),
                annotation_id: None,
            },
        )));
    }

    if let Some(edits) = text_changes.remove(&target_uri) {
        document_changes.push(DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: text_document_identifier_for_context(context, &target_uri),
            edits: edits
                .into_iter()
                .map(tower_lsp::lsp_types::OneOf::Left)
                .collect(),
        }));
    }

    for (uri, edits) in text_changes {
        document_changes.push(DocumentChangeOperation::Edit(TextDocumentEdit {
            text_document: text_document_identifier_for_context(context, &uri),
            edits: edits
                .into_iter()
                .map(tower_lsp::lsp_types::OneOf::Left)
                .collect(),
        }));
    }

    if delete_source {
        document_changes.push(DocumentChangeOperation::Op(ResourceOp::Delete(
            DeleteFile {
                uri: uri.clone(),
                options: Some(DeleteFileOptions {
                    recursive: Some(false),
                    ignore_if_not_exists: Some(true),
                    annotation_id: None,
                }),
            },
        )));
    }

    Some(WorkspaceEdit {
        changes: None,
        document_changes: Some(DocumentChanges::Operations(document_changes)),
        change_annotations: None,
    })
}

fn add_rename_edits_to_changes(
    context: &impl ServerContext,
    rename_result: RenameResult,
    changes: &mut HashMap<Url, Vec<TextEdit>>,
) {
    for (file_id, edits) in rename_result.edits {
        let Some(doc) = context.document_for_file_id(file_id) else {
            continue;
        };
        let uri = doc.uri.clone();
        for edit in edits {
            let lsp_edit = TextEdit {
                range: Range {
                    start: offset_to_position(&doc.content, edit.range.start().into()),
                    end: offset_to_position(&doc.content, edit.range.end().into()),
                },
                new_text: edit.new_text,
            };
            changes.entry(uri.clone()).or_default().push(lsp_edit);
        }
    }
}

fn uri_exists(context: &impl ServerContext, uri: &Url) -> bool {
    if context.get_document(uri).is_some() {
        return true;
    }
    if let Some(path) = uri_to_path(uri) {
        return path.exists();
    }
    false
}

fn load_document_content(context: &impl ServerContext, uri: &Url) -> Option<String> {
    if let Some(doc) = context.get_document(uri) {
        return Some(doc.content);
    }
    let path = uri_to_path(uri)?;
    std::fs::read_to_string(path).ok()
}

fn derive_target_uri(context: &impl ServerContext, parts: &[smol_str::SmolStr]) -> Option<Url> {
    if parts.is_empty() {
        return None;
    }
    let root = context.workspace_folders().into_iter().next()?;
    let mut target = root.clone();
    let file_name = format!("{}.st", parts.last()?.as_str());
    {
        let mut segments = target.path_segments_mut().ok()?;
        segments.pop_if_empty();
        for part in &parts[..parts.len().saturating_sub(1)] {
            segments.push(part.as_str());
        }
        segments.push(&file_name);
    }
    Some(target)
}

fn text_document_identifier_for_context(
    context: &impl ServerContext,
    uri: &Url,
) -> OptionalVersionedTextDocumentIdentifier {
    let version =
        context
            .get_document(uri)
            .and_then(|doc| if doc.is_open { Some(doc.version) } else { None });
    OptionalVersionedTextDocumentIdentifier {
        uri: uri.clone(),
        version,
    }
}

fn build_namespace_insert_text(target_content: &str, namespace_text: &str) -> String {
    let mut text = String::new();
    if !target_content.is_empty() && !target_content.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(namespace_text);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

fn apply_text_edits(source: &str, edits: &[IdeTextEdit]) -> String {
    let mut result = source.to_string();
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|edit| std::cmp::Reverse(edit.range.start()));
    for edit in sorted {
        let start: usize = edit.range.start().into();
        let end: usize = edit.range.end().into();
        if start > result.len() || end > result.len() || start > end {
            continue;
        }
        result.replace_range(start..end, &edit.new_text);
    }
    result
}

fn shift_edit_range(edit: IdeTextEdit, base: TextSize) -> Option<IdeTextEdit> {
    if edit.range.start() < base || edit.range.end() < base {
        return None;
    }
    let start = edit.range.start() - base;
    let end = edit.range.end() - base;
    Some(IdeTextEdit {
        range: TextRange::new(start, end),
        new_text: edit.new_text,
    })
}

fn ranges_overlap(left: TextRange, right: TextRange) -> bool {
    left.start() < right.end() && right.start() < left.end()
}

fn find_enclosing_node_of_kind(
    root: &SyntaxNode,
    range: TextRange,
    kind: SyntaxKind,
) -> Option<SyntaxNode> {
    root.descendants()
        .filter(|node| node.kind() == kind)
        .filter(|node| {
            let node_range = node.text_range();
            node_range.contains(range.start()) && node_range.contains(range.end())
        })
        .min_by_key(|node| node.text_range().len())
}

fn expand_to_full_lines(source: &str, range: TextRange) -> TextRange {
    let start = line_start_offset(source, range.start().into());
    let end = line_end_offset(source, range.end().into());
    TextRange::new(TextSize::from(start as u32), TextSize::from(end as u32))
}

fn line_start_offset(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    match source[..offset].rfind('\n') {
        Some(pos) => pos + 1,
        None => 0,
    }
}

fn line_end_offset(source: &str, offset: usize) -> usize {
    let offset = offset.min(source.len());
    match source[offset..].find('\n') {
        Some(pos) => offset + pos + 1,
        None => source.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        BuildConfig, DiagnosticSettings, IndexingConfig, ProjectConfig, RuntimeConfig,
        StdlibSettings, TelemetryConfig, WorkspaceSettings,
    };
    use crate::state::Document;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use trust_hir::db::FileId;

    #[derive(Clone, Default)]
    struct MockContext {
        workspace_configs: Vec<(Url, ProjectConfig)>,
        workspace_config_by_uri: HashMap<Url, ProjectConfig>,
        workspace_folders: Vec<Url>,
        documents_by_uri: HashMap<Url, Document>,
        documents_by_file_id: HashMap<FileId, Document>,
        rename_result: Option<RenameResult>,
    }

    impl MockContext {
        fn insert_document(&mut self, document: Document) {
            self.documents_by_file_id
                .insert(document.file_id, document.clone());
            self.documents_by_uri.insert(document.uri.clone(), document);
        }
    }

    impl ServerContext for MockContext {
        fn workspace_configs(&self) -> Vec<(Url, ProjectConfig)> {
            self.workspace_configs.clone()
        }

        fn workspace_config_for_uri(&self, uri: &Url) -> Option<ProjectConfig> {
            self.workspace_config_by_uri.get(uri).cloned()
        }

        fn workspace_folders(&self) -> Vec<Url> {
            self.workspace_folders.clone()
        }

        fn get_document(&self, uri: &Url) -> Option<Document> {
            self.documents_by_uri.get(uri).cloned()
        }

        fn document_for_file_id(&self, file_id: FileId) -> Option<Document> {
            self.documents_by_file_id.get(&file_id).cloned()
        }

        fn rename(
            &self,
            _file_id: FileId,
            _offset: TextSize,
            _new_name: &str,
        ) -> Option<RenameResult> {
            self.rename_result.clone()
        }
    }

    fn test_project_config(root: &str, target: &str) -> ProjectConfig {
        ProjectConfig {
            root: PathBuf::from(root),
            config_path: None,
            include_paths: Vec::new(),
            vendor_profile: None,
            stdlib: StdlibSettings::default(),
            libraries: Vec::new(),
            dependencies: Vec::new(),
            dependency_resolution_issues: Vec::new(),
            diagnostic_external_paths: Vec::new(),
            build: BuildConfig {
                target: Some(target.to_string()),
                ..BuildConfig::default()
            },
            targets: Vec::new(),
            indexing: IndexingConfig::default(),
            diagnostics: DiagnosticSettings::default(),
            runtime: RuntimeConfig::default(),
            workspace: WorkspaceSettings::default(),
            telemetry: TelemetryConfig::default(),
        }
    }

    fn test_document(uri: &str, file_id: u32, content: &str) -> Document {
        Document::new(
            Url::parse(uri).expect("test uri"),
            1,
            content.to_string(),
            FileId(file_id),
            true,
            1,
        )
    }

    #[test]
    fn project_info_with_mock_context_uses_uri_mapping() {
        let root_a = Url::parse("file:///workspace/a/").expect("root a");
        let root_b = Url::parse("file:///workspace/b/").expect("root b");
        let config_a = test_project_config("/workspace/a", "x86_64");
        let config_b = test_project_config("/workspace/b", "armv7");
        let main_uri = Url::parse("file:///workspace/a/src/main.st").expect("main uri");

        let mut context = MockContext {
            workspace_configs: vec![
                (root_a.clone(), config_a.clone()),
                (root_b.clone(), config_b.clone()),
            ],
            ..MockContext::default()
        };
        context
            .workspace_config_by_uri
            .insert(main_uri.clone(), config_a);

        let info = project_info_value_with_context(
            &context,
            vec![json!({
                "text_document": {
                    "uri": main_uri,
                }
            })],
        )
        .expect("project info");
        let projects = info
            .get("projects")
            .and_then(Value::as_array)
            .expect("projects");
        assert_eq!(projects.len(), 1);
        assert_eq!(
            projects[0]
                .get("build")
                .and_then(|build| build.get("target"))
                .and_then(Value::as_str),
            Some("x86_64")
        );
    }

    #[test]
    fn namespace_move_with_mock_context_generates_expected_operations() {
        let source = r#"
NAMESPACE LibA
TYPE Foo : INT;
END_TYPE
END_NAMESPACE
"#;
        let main = r#"
PROGRAM Main
VAR
    x : LibA.Foo;
END_VAR
END_PROGRAM
"#;
        let source_uri = Url::parse("file:///workspace/liba.st").expect("source uri");
        let main_uri = Url::parse("file:///workspace/main.st").expect("main uri");
        let target_uri = Url::parse("file:///workspace/Company/LibA.st").expect("target uri");

        let source_doc = test_document(source_uri.as_str(), 1, source);
        let main_doc = test_document(main_uri.as_str(), 2, main);

        let mut rename_result = RenameResult::new();
        let ns_start = source.find("LibA").expect("namespace name start");
        rename_result.add_edit(
            source_doc.file_id,
            IdeTextEdit {
                range: TextRange::new(
                    TextSize::from(ns_start as u32),
                    TextSize::from((ns_start + "LibA".len()) as u32),
                ),
                new_text: "Company.LibA".to_string(),
            },
        );
        let main_ref_start = main.find("LibA").expect("main namespace reference");
        rename_result.add_edit(
            main_doc.file_id,
            IdeTextEdit {
                range: TextRange::new(
                    TextSize::from(main_ref_start as u32),
                    TextSize::from((main_ref_start + "LibA".len()) as u32),
                ),
                new_text: "Company.LibA".to_string(),
            },
        );

        let mut context = MockContext {
            workspace_folders: vec![Url::parse("file:///workspace/").expect("root uri")],
            rename_result: Some(rename_result),
            ..MockContext::default()
        };
        context.insert_document(source_doc);
        context.insert_document(main_doc);

        let args = MoveNamespaceCommandArgs {
            text_document: TextDocumentIdentifier {
                uri: source_uri.clone(),
            },
            position: offset_to_position(source, source.find("LibA").expect("position") as u32),
            new_path: "Company.LibA".to_string(),
            target_uri: Some(target_uri.clone()),
        };
        let edit = namespace_move_workspace_edit_with_context(&context, args).expect("edit");
        let ops = match edit.document_changes.expect("document changes") {
            DocumentChanges::Operations(ops) => ops,
            DocumentChanges::Edits(_) => panic!("expected operation list"),
        };

        assert!(
            ops.iter().any(|op| matches!(
                op,
                DocumentChangeOperation::Op(ResourceOp::Create(create)) if create.uri == target_uri
            )),
            "expected target file create operation"
        );
        assert!(
            ops.iter().any(|op| matches!(
                op,
                DocumentChangeOperation::Op(ResourceOp::Delete(delete)) if delete.uri == source_uri
            )),
            "expected source file delete operation"
        );

        let target_edit = ops.iter().find_map(|op| match op {
            DocumentChangeOperation::Edit(edit) if edit.text_document.uri == target_uri => {
                Some(edit)
            }
            _ => None,
        });
        let target_edit = target_edit.expect("target edit");
        let target_contains_renamed_namespace = target_edit.edits.iter().any(|edit| match edit {
            tower_lsp::lsp_types::OneOf::Left(edit) => {
                edit.new_text.contains("NAMESPACE Company.LibA")
            }
            tower_lsp::lsp_types::OneOf::Right(_) => false,
        });
        assert!(
            target_contains_renamed_namespace,
            "target insertion should include renamed namespace"
        );

        let main_edit = ops.iter().find_map(|op| match op {
            DocumentChangeOperation::Edit(edit) if edit.text_document.uri == main_uri => Some(edit),
            _ => None,
        });
        let main_edit = main_edit.expect("main edit");
        let main_updated = main_edit.edits.iter().any(|edit| match edit {
            tower_lsp::lsp_types::OneOf::Left(edit) => edit.new_text.contains("Company.LibA"),
            tower_lsp::lsp_types::OneOf::Right(_) => false,
        });
        assert!(main_updated, "main file should include renamed namespace");
    }

    #[test]
    fn project_info_server_state_and_context_paths_match() {
        let state = ServerState::new();
        let root = Url::parse("file:///workspace/").expect("root");
        state.set_workspace_folders(vec![root.clone()]);
        state.set_workspace_config(root, test_project_config("/workspace", "x86_64"));

        let from_wrapper = project_info_value(&state, Vec::new()).expect("wrapper value");
        let from_context =
            project_info_value_with_context(&state, Vec::new()).expect("context value");
        assert_eq!(from_wrapper, from_context);
    }
}
