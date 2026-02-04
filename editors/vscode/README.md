# Structured Text (truST LSP) VS Code Extension

VS Code client for the truST LSP language server (`trust-lsp`) (IEC 61131-3 Structured Text).

## Quick Start (Users)

1. Install **truST LSP** from the Marketplace.
2. Open a folder with `.st` or `.pou` files.
3. Start editing — the extension auto-starts bundled `trust-lsp`/`trust-debug` binaries.
4. (Optional) Add `trust-lsp.toml` or set the runtime endpoint in the **Structured Text Runtime** panel.

## Best Features

- IEC 61131-3-aware diagnostics with spec references.
- Semantic tokens, formatting, and smart code actions.
- Refactor command **Structured Text: Move Namespace**.
- Rename that updates file names and references.
- Inline values + runtime I/O panel.
- DAP debugging with breakpoints, stepping, and variables.

## Install

### Marketplace

1. Open VS Code.
2. Go to Extensions.
3. Search for **truST LSP**.
4. Click Install.

Or from the command line:

```bash
code --install-extension trust-platform.trust-lsp
```


## Features
- Syntax highlighting via TextMate grammar.
- LSP features: hover, completion, go to definition, references, rename, formatting, folding, semantic tokens.
- Refactor command: `Structured Text: Move Namespace` (prompts for a new namespace path and optional target file).
- Debugger (DAP) integration is available but requires a `trust-debug` adapter executable.
- Diagnostics display IEC references when provided by the server (in Problems/hover) with links to local spec docs.

## Language Model Tools (lm.tools)
The extension exposes structured tool calls for AI assistants that support VS Code language model tools.
Tool-calling models can read/write files, inspect LSP data, and trigger debug helpers.

Notes:
- Line/character positions are zero-based.
- File paths must be absolute or VS Code URIs (e.g., `vscode-notebook-cell:`).
- File read/write/apply tools only allow paths inside the current workspace.
- Code actions currently include quick-fix removal for unused variables (W001) and unused parameters (W002).

### File context and edits
- `trust_file_read` `{ filePath, startLine?, startCharacter?, endLine?, endCharacter? }`
- `trust_read_range` `{ filePath, startLine, startCharacter, endLine, endCharacter }`
- `trust_file_write` `{ filePath, text, save? }`
- `trust_apply_edits` `{ filePath, edits: [{ startLine, startCharacter, endLine, endCharacter, newText }], save? }`

### LSP inspector
- `trust_lsp_request` `{ method, params?, requestTimeoutMs?, captureNotifications?, notificationTimeoutMs?, captureProgress?, capturePartialResults?, workDoneToken?, partialResultToken? }`
- `trust_lsp_notify` `{ method, params? }`

### LSP insights
- `trust_get_hover` `{ filePath, line, character }`
- `trust_get_diagnostics` `{ filePath }`
- `trust_get_definition` `{ filePath, line, character }`
- `trust_get_declaration` `{ filePath, line, character }`
- `trust_get_type_definition` `{ filePath, line, character }`
- `trust_get_implementation` `{ filePath, line, character }`
- `trust_get_references` `{ filePath, line, character, includeDeclaration? }`
- `trust_get_completions` `{ filePath, line, character, triggerCharacter? }`
- `trust_get_signature_help` `{ filePath, line, character }`
- `trust_get_document_symbols` `{ filePath }`
- `trust_get_workspace_symbols` `{ query, limit? }`
- `trust_get_workspace_symbols_timed` `{ query, limit?, pathIncludes? }`
- `trust_get_rename_edits` `{ filePath, line, character, newName }`
- `trust_get_formatting_edits` `{ filePath }`
- `trust_get_on_type_formatting_edits` `{ filePath, line, character, triggerCharacter }`
- `trust_get_code_actions` `{ filePath, startLine, startCharacter, endLine, endCharacter }`
- `trust_get_project_info` `{ arguments? }`
- `trust_get_semantic_tokens_full` `{ filePath }`
- `trust_get_semantic_tokens_delta` `{ filePath, previousResultId }`
- `trust_get_semantic_tokens_range` `{ filePath, startLine, startCharacter, endLine, endCharacter }`
- `trust_get_inlay_hints` `{ filePath, startLine, startCharacter, endLine, endCharacter }`
- `trust_get_linked_editing` `{ filePath, line, character }`
- `trust_get_document_links` `{ filePath, resolve? }`
- `trust_get_code_lens` `{ filePath, resolve? }`
- `trust_get_selection_ranges` `{ filePath, positions: [{ line, character }] }`
- `trust_call_hierarchy_prepare` `{ filePath, line, character }`
- `trust_call_hierarchy_incoming` `{ item }`
- `trust_call_hierarchy_outgoing` `{ item }`
- `trust_type_hierarchy_prepare` `{ filePath, line, character }`
- `trust_type_hierarchy_supertypes` `{ item }`
- `trust_type_hierarchy_subtypes` `{ item }`

### Workspace ops + settings + telemetry
- `trust_workspace_rename_file` `{ oldPath, newPath, overwrite?, useWorkspaceEdit? }`
- `trust_update_settings` `{ key, value, scope?, filePath?, timeoutMs?, forceRefresh? }`
- `trust_read_telemetry` `{ filePath?, limit?, tail? }`

### Debug helpers
- `trust_get_inline_values` `{ frameId, startLine, startCharacter, endLine, endCharacter, context? }`
- `trust_debug_start` `{ filePath? }`
- `trust_debug_attach` `{}`
- `trust_debug_reload` `{}`
- `trust_debug_open_io_panel` `{}`
- `trust_debug_ensure_configuration` `{}`

## Setup
1. Build the server binary:
   `cargo build -p trust-lsp`
2. Point VS Code to the server:
   - Set `trust-lsp.server.path` to the built binary (for example: `target/debug/trust-lsp`).
   - Or leave it empty to use `trust-lsp` from your PATH.
3. Open a workspace containing `.st` files.

Inline values can use a runtime control endpoint set from the **Structured Text Runtime** panel
(gear icon → Runtime Settings). This writes a workspace setting override, so you do not need to
create `trust-lsp.toml` just for inline values.

Optional: set `trust-lsp.trace.server` to `messages` or `verbose` for LSP tracing.
Optional: set `trust-lsp.diagnostics.showIecReferences` to toggle IEC references in diagnostics.

## Smoke tests
- Document links: open a `.st` file with a `USING Foo;` directive and Ctrl/Cmd+click `Foo` to jump to its namespace definition.
- Config links: open `trust-lsp.toml` and Ctrl/Cmd+click paths in `include_paths`, `library_paths`, or `[[libraries]] path`.
- Move namespace: right-click a `NAMESPACE` or `USING` line and choose `Structured Text: Move Namespace` to relocate declarations.
- Move namespace (lightbulb): place the cursor on a `NAMESPACE` or `USING` line and run the `Move Namespace` quick fix.

## Troubleshooting
- IEC spec links in Problems panel: ensure `trust-lsp.diagnostics.showIecReferences` is enabled and that `docs/specs/*.md` is part of the opened workspace (links appear in the diagnostic hover).

## Debugging
1. Provide a `trust-debug` adapter executable on your PATH or set `trust-lsp.debug.adapter.path`.
2. (Optional) Set `trust-lsp.debug.adapter.args` and `trust-lsp.debug.adapter.env` as needed.
3. Run `Structured Text: Start Debugging` from the Command Palette, or use the
   `Debug Structured Text` launch configuration template.
4. To attach to a running runtime, run `Structured Text: Attach Debugger` (auto-reads
   `runtime.toml` for the control endpoint).
4. Open `Structured Text: Open I/O Panel` to view input/output snapshots (refreshes on demand).

## Runtime Panel Development
- The webview script source lives in `editors/vscode/src/ioPanel.webview.js`.
- Run `npm run build:panel` to sync it into `editors/vscode/media/ioPanel.js`.
- `npm run compile` runs the sync automatically before building the extension.
- `npm run watch` warns if the panel script is out of sync while the TypeScript watcher runs.
- `editors/vscode/src/ioPanel.ts` only builds the HTML shell + wiring.

## Run/Debug the extension
1. `cd editors/vscode`
2. `npm install`
3. `npm run compile`
4. In VS Code, open the repo root and run the `Run Extension` launch configuration (F5).
