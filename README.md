# truST Platform
[![CI](https://github.com/johannesPettersson80/trust-platform/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/johannesPettersson80/trust-platform/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Rust Version](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org)


truST Platform is an IEC 61131-3 Structured Text tooling suite: a language server, runtime, debugger, and VS Code extension.
The repo is `trust-platform`; shipped binaries keep stable names: `trust-lsp`, `trust-runtime`, `trust-debug`, and `trust-bundle-gen`.

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


## Install

### VS Code (Marketplace)

1. Open VS Code.
2. Go to Extensions.
3. Search for **truST LSP**.
4. Click Install.

You can also install from the command line:

```bash
code --install-extension trust-lsp.trust-lsp
```

### Pre-built binaries (GitHub Releases)

Download platform-specific binaries from the GitHub Releases page for this repo.

## Quick Start

### Build From Source

```bash
# Clone the repository
git clone <repo-url>
cd trust-platform

# Build in release mode
cargo build --release

# Binaries will be in target/release/
# trust-lsp, trust-runtime, trust-debug, trust-bundle-gen
```

### Run the Language Server

```bash
cargo run --release --bin trust-lsp
```

### VS Code Extension

The extension lives in `editors/vscode` (Marketplace publishing in progress). See
`editors/vscode/README.md` for setup and debug instructions.

## Configuration

Most projects use `trust-lsp.toml` at the workspace root. Runtime inline values can also be configured
from the VS Code **Structured Text Runtime** panel (gear icon → Runtime Settings).
See `docs/README.md` for a minimal configuration example.

## Documentation

- `docs/README.md` - Documentation index and diagram workflow
- `docs/guides/PLC_QUICK_START.md` - Hands-on quick start
- `docs/specs/README.md` - IEC 61131-3 specs and tooling references

## Status

- VS Code Marketplace publishing is in progress.
- Runtime and debugger are experimental but integrated in the platform workflow.


## Getting Help

- GitHub Issues: https://github.com/johannesPettersson80/trust-platform/issues
- Email: johannes_salomon@hotmail.com
- LinkedIn: https://linkedin.com/in/johannes-pettersson

## License

Licensed under MIT or Apache-2.0. See `LICENSE-MIT` and `LICENSE-APACHE`.
