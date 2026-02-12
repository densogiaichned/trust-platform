# Examples: Guided VS Code Tutorial Tracks

This folder is curated as onboarding-quality tutorials.

Two former directories were intentionally removed from `examples/`:

- `browser_analysis_wasm_spike/` (internal prototype moved under `docs/internal/prototypes/`)
- `openplc_interop_v1/` (OpenPLC notes absorbed into the PLCopen ST-complete tutorial)

## One-Time Setup

1. Build core binaries:

```bash
cargo build -p trust-runtime -p trust-lsp -p trust-debug
```

2. Install extension:

```bash
code --install-extension trust-platform.trust-lsp
```

3. Open repository:

```bash
code /path/to/trust-platform
```

## Tutorial Catalog (Kept + Improved)

| Track | Start Here | What You Learn | Typical Time |
|---|---|---|---|
| ST fundamentals | `examples/tutorials/README.md` | language basics, VS Code tooling, testing workflow | 60-120 min |
| Runtime I/O mental model | `examples/memory_marker_counter/README.md` | `%M/%Q` cycle semantics + debugger confirmation | 20-30 min |
| Multi-file architecture | `examples/plant_demo/README.md` | type/FB/program/config layering + cross-file refactors | 25-40 min |
| Process-control capstone | `examples/filling_line/README.md` | hysteresis control, interface hierarchy, hot reload | 35-55 min |
| EtherCAT bring-up | `examples/ethercat_ek1100_elx008_v1/README.md` | `io.toml`, mock-first validation, hardware handoff | 30-50 min |
| PLCopen XML interop | `examples/plcopen_xml_st_complete/README.md` | VS Code import, post-import exploration, round-trip checks, OpenPLC detection note | 30-50 min |
| Siemens profile | `examples/siemens_scl_v1/README.md` | `#`-prefix behavior, profile comparison, runtime/debug run | 20-30 min |
| Mitsubishi profile | `examples/mitsubishi_gxworks3_v1/README.md` | `DIFU/DIFD` mapping, profile comparison, runtime/debug run | 20-30 min |
| Vendor library stubs | `examples/vendor_library_stubs/README.md` | user-extensible vendor symbol stubs via `[[libraries]]` | 15-25 min |

## Recommended Learning Order

1. `examples/tutorials/README.md`
2. `examples/memory_marker_counter/README.md`
3. `examples/plant_demo/README.md`
4. `examples/filling_line/README.md`
5. Choose specialization:
   - Interop: `examples/plcopen_xml_st_complete/README.md`
   - Vendor profiles: `examples/siemens_scl_v1/README.md`, `examples/mitsubishi_gxworks3_v1/README.md`
   - Fieldbus backend: `examples/ethercat_ek1100_elx008_v1/README.md`

## Validation Commands

```bash
trust-runtime build --project examples/filling_line --sources src
trust-runtime build --project examples/plant_demo --sources src
trust-runtime build --project examples/ethercat_ek1100_elx008_v1 --sources src
trust-runtime build --project examples/siemens_scl_v1 --sources src
trust-runtime build --project examples/mitsubishi_gxworks3_v1 --sources src
trust-runtime build --project examples/vendor_library_stubs --sources .
```

Tutorial regression checks:

```bash
cargo test -p trust-runtime tutorial_examples_parse_typecheck_and_compile_to_bytecode
cargo test -p trust-runtime st_test_cli_command
```
