# truST Platform
[![CI](https://github.com/johannesPettersson80/trust-platform/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/johannesPettersson80/trust-platform/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)

truST Platform is an IEC 61131-3 Structured Text toolchain: a VS Code extension, language server, runtime, and debugger.
Shipped binaries keep stable names: `trust-lsp`, `trust-runtime`, `trust-debug`, and `trust-bundle-gen`.

## Quick Start (VS Code)

1. Install **truST LSP** from the Marketplace.
2. Open a folder containing `.st` or `.pou` files.
3. Start editing — the extension auto-starts the bundled `trust-lsp`/`trust-debug` binaries.
4. (Optional) Add a `trust-lsp.toml` at the workspace root for project settings.

Command line install:

```bash
code --install-extension trust-platform.trust-lsp
```

## Best Features

- IEC 61131-3-aware diagnostics with spec references.
- Semantic tokens, formatting, and smart code actions.
- Refactors like **Move Namespace** and rename that updates file names.
- Go to definition/references, call hierarchy, type hierarchy, and workspace symbols.
- Inline values and I/O panel driven by the runtime control endpoint.
- DAP debugging with breakpoints, stepping, and variables.

## Components

| Component | Binary | Purpose |
|---|---|---|
| Language Server | `trust-lsp` | LSP server powering diagnostics, navigation, refactors, and IDE features |
| Runtime | `trust-runtime` | IEC 61131-3 runtime execution engine + bytecode |
| Debug Adapter | `trust-debug` | DAP adapter for breakpoints, stepping, and variables |
| Bundle Tool | `trust-bundle-gen` | Generates STBC bundles for runtime execution |
| VS Code Extension | (bundles `trust-lsp`/`trust-debug`) | Editor UX, commands, and LM tools |

## Architecture

![truST system architecture](docs/diagrams/generated/system-architecture.svg)

## Runtime + Debugger (Optional)

- Download pre-built binaries from GitHub Releases, or build from source.
- Start the runtime:

```bash
trust-runtime --project /path/to/project
```

- Ensure `trust-debug` is available on your PATH (or set `trust-lsp.debug.adapter.path`).
- In VS Code, run **Structured Text: Start Debugging** or **Attach Debugger**.

## Configuration (trust-lsp.toml)

Put `trust-lsp.toml` at the workspace root to configure indexing and runtime integration.

```toml
[project]
include_paths = ["libs"]
vendor_profile = "codesys"

[runtime]
# Required for inline values via the runtime control endpoint.
control_endpoint = "unix:///tmp/trust-runtime.sock"
# Optional auth token (matches runtime control settings).
control_auth_token = "optional-token"
```

Inline values also work by setting the runtime endpoint from the VS Code **Structured Text Runtime** panel
(gear icon → Runtime Settings) without editing `trust-lsp.toml`.

## Build From Source (Developer)

```bash
git clone https://github.com/johannesPettersson80/trust-platform
cd trust-platform
cargo build --release
```

Binaries are in `target/release/`.

## Documentation

- `docs/README.md` - Documentation index and diagram workflow
- `docs/guides/PLC_QUICK_START.md` - Hands-on quick start
- `docs/specs/README.md` - IEC 61131-3 specs and tooling references

## Status

- VS Code Marketplace: live
- Runtime and debugger: experimental, integrated in the platform workflow

## Getting Help

- GitHub Issues: https://github.com/johannesPettersson80/trust-platform/issues
- Email: johannes_salomon@hotmail.com
- LinkedIn: https://linkedin.com/in/johannes-pettersson

## License

Licensed under MIT or Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
