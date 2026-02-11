# PLCopen Interop Compatibility (Deliverable 2)

This document defines the current PLCopen XML interoperability contract for
`trust-runtime plcopen` after Deliverable 2 hardening.

## Scope

- Namespace: `http://www.plcopen.org/xml/tc6_0200`
- Profile: `trust-st-strict-v1`
- Command surface:
  - `trust-runtime plcopen profile [--json]`
  - `trust-runtime plcopen export [--project <dir>] [--output <file>] [--json]`
  - `trust-runtime plcopen import --input <file> [--project <dir>] [--json]`

## Compatibility Matrix

| Capability | Status | Notes |
|---|---|---|
| ST POU import/export (`PROGRAM`, `FUNCTION`, `FUNCTION_BLOCK`) | supported | Includes common aliases (`PRG`, `FC`, `FUN`, `FB`). |
| ST type graph import (`types/dataTypes` subset) | partial | Supported `baseType` subset (`elementary`, `derived`, `array`, `struct`, `enum`, `subrange`) is imported into generated `TYPE` declarations under `sources/`. |
| Source map metadata (`trust.sourceMap`) | supported | Embedded `addData` payload + sidecar `*.source-map.json`. |
| Vendor extension preservation (`addData`) | partial | Preserved/re-injectable, but not semantically interpreted. |
| Vendor ecosystem migration heuristics | partial | Advisory signal only; not semantic equivalence. |
| Vendor library shim normalization | partial | Selected aliases are mapped to IEC FB names during import; each mapping is reported. |
| Graphical bodies (FBD/LD/SFC) | unsupported | Strict subset remains ST-only. |
| Resource/configuration execution model import | unsupported | `<instances>/<configurations>/<resources>` not mapped to runtime scheduling. |
| Vendor AOI/library internal semantics | unsupported | Advanced behavior remains manual migration work beyond symbol-level shims. |

## Migration Report Contract

`plcopen import` writes `interop/plcopen-migration-report.json` with:

- Coverage metrics:
  - `discovered_pous`
  - `imported_pous`
  - `skipped_pous`
  - `source_coverage_percent`
  - `semantic_loss_percent`
  - `compatibility_coverage`:
    - `supported_items`
    - `partial_items`
    - `unsupported_items`
    - `support_percent`
    - `verdict` (`full` | `partial` | `low` | `none`)
- Structured diagnostics (`unsupported_diagnostics`) with:
  - `code`
  - `severity`
  - `node`
  - `message`
  - optional `pou`
  - `action`
- Applied shim summary (`applied_library_shims`) with:
  - `vendor`
  - `source_symbol`
  - `replacement_symbol`
  - `occurrences`
  - `notes`
- Per-POU migration entries (`entries`) with `status` and `reason`.

## Supported Ecosystem Detection (Advisory)

Detected values currently include:

- `codesys`
- `beckhoff-twincat`
- `siemens-tia`
- `rockwell-studio5000`
- `schneider-ecostruxure`
- `mitsubishi-gxworks3`
- fallback: `generic-plcopen`

## Round-Trip Limits

Round-trip means `export -> import -> export` through the strict subset.

Guaranteed:

- ST POU signature-level stability for importable subset inputs.
- Stable source-map sidecar contract.

Not guaranteed:

- Original vendor formatting/layout in XML payloads.
- Preservation of graphical network semantics.
- Import of runtime deployment/safety metadata.
- Exact source file names (imports use sanitized unique names under `sources/`).
- `dataTypes` round-trip parity (import currently generates ST `TYPE` source; export-side `dataTypes` emission is pending).

## Known Gaps

- No semantic import for SFC/LD/FBD bodies.
- No import of PLCopen runtime resources/configurations into task/runtime model.
- `dataTypes` export is not yet implemented; import currently materializes supported `dataTypes` as ST `TYPE` declarations in `sources/`.
- Vendor library shim coverage is intentionally limited to the baseline alias catalog.
- No semantic translation for vendor-specific AOI/FB internals and pragmas.
- Vendor extension nodes are preserved as opaque metadata, not executed.
