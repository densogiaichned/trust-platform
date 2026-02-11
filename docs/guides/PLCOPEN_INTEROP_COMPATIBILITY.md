# PLCopen Interop Compatibility (Deliverable 5)

This document defines the current PLCopen XML interoperability contract for
`trust-runtime plcopen` after Deliverable 5 (ST-complete project coverage).

## Scope

- Namespace: `http://www.plcopen.org/xml/tc6_0200`
- Profile: `trust-st-complete-v1`
- Command surface:
  - `trust-runtime plcopen profile [--json]`
  - `trust-runtime plcopen export [--project <dir>] [--output <file>] [--json]`
  - `trust-runtime plcopen import --input <file> [--project <dir>] [--json]`
- Product decision for this phase:
  - ST-only PLCopen project support.
  - FBD/LD/SFC graphical network bodies are out of scope.

## Compatibility Matrix

| Capability | Status | Notes |
|---|---|---|
| ST POU import/export (`PROGRAM`, `FUNCTION`, `FUNCTION_BLOCK`) | supported | Includes common aliases (`PRG`, `FC`, `FUN`, `FB`). |
| ST `types/dataTypes` import (`elementary`, `derived`, `array`, `struct`, `enum`, `subrange`) | supported | Imported into generated ST `TYPE` declarations under `sources/`. |
| ST `TYPE` export to `types/dataTypes` | partial | Supported ST declarations are emitted; unsupported forms are skipped with warnings. |
| `instances/configurations/resources/tasks/program instances` import/export | supported | Deterministic ST mapping with name normalization and structured diagnostics when fallback behavior is applied. |
| Source map metadata (`trust.sourceMap`) | supported | Embedded `addData` payload + sidecar `*.source-map.json`. |
| Vendor extension preservation (`addData`) | partial | Preserved/re-injectable, but not semantically interpreted. |
| Vendor ecosystem migration heuristics | partial | Advisory signal only; not semantic equivalence. |
| Vendor library shim normalization | partial | Selected aliases are mapped to IEC FB names during import; each mapping is reported. |
| Graphical bodies (FBD/LD/SFC) | unsupported | ST-complete contract remains ST-only by product decision. |
| Vendor AOI/library internal semantics | unsupported | Advanced behavior remains manual migration work beyond symbol-level shims. |

## Migration Report Contract

`plcopen import` writes `interop/plcopen-migration-report.json` with:

- Coverage metrics:
  - `discovered_pous`
  - `imported_pous`
  - `skipped_pous`
  - `imported_data_types`
  - `discovered_configurations`
  - `imported_configurations`
  - `imported_resources`
  - `imported_tasks`
  - `imported_program_instances`
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

## CODESYS ST Fixture Pack and Parity Gate

Deliverable 5 includes deterministic CODESYS ST fixture packs for
`small`, `medium`, and `large` project shapes:

- XML fixtures:
  - `crates/trust-runtime/tests/fixtures/plcopen/codesys_st_complete/small.xml`
  - `crates/trust-runtime/tests/fixtures/plcopen/codesys_st_complete/medium.xml`
  - `crates/trust-runtime/tests/fixtures/plcopen/codesys_st_complete/large.xml`
- Expected migration artifacts:
  - `crates/trust-runtime/tests/fixtures/plcopen/codesys_st_complete/*.expected-migration.json`
- CI parity regression gate:
  - `crates/trust-runtime/tests/plcopen_st_complete_parity.rs`

The parity test enforces deterministic import/export signature stability for
supported ST-project structures and fails CI on schema-drift regressions.

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

Round-trip means `import -> export -> import -> export` through the
ST-complete contract.

Guaranteed for supported ST-project structures:

- ST POU signature-level stability.
- Supported `dataTypes` signature stability.
- Supported configuration/resource/task/program-instance wiring intent stability.
- Stable source-map sidecar contract.

Not guaranteed:

- Original vendor formatting/layout in XML payloads.
- Preservation of graphical network semantics.
- Import of runtime deployment/safety metadata.
- Exact source file names (imports use sanitized unique names under `sources/`).

## Known Gaps

- No semantic import/export for SFC/LD/FBD bodies.
- Export-side `dataTypes` remains subset-based for supported ST `TYPE` forms; unsupported ST type syntax is skipped with warnings.
- Vendor library shim coverage is intentionally limited to the baseline alias catalog.
- No semantic translation for vendor-specific AOI/FB internals and pragmas.
- Vendor extension nodes are preserved as opaque metadata, not executed.

## Example Project

A complete import/export walkthrough project is available in:

- `examples/plcopen_xml_st_complete/`
