# PLCopen XML ST-Complete Example

This example shows end-to-end PLCopen XML import/export for an ST-complete
project (POUs + `TYPE` graph + configuration/resource/task/program-instance
model).

## What Is Included

- Native ST project sources under `sources/`:
  - `main.st`: `PROGRAM` + `FUNCTION_BLOCK`
  - `types.st`: enum/struct/subrange/array `TYPE` declarations
  - `configuration.st`: `CONFIGURATION` + `RESOURCE` + `TASK` + `PROGRAM` binding
- A CODESYS-style PLCopen XML import sample:
  - `interop/codesys-small.xml`

## Preconditions

- `trust-runtime` is built and available in your shell.
- Run commands from repo root (`trust-platform/`).

## Flow A: Export ST Project -> PLCopen XML

```bash
trust-runtime plcopen export \
  --project examples/plcopen_xml_st_complete \
  --output examples/plcopen_xml_st_complete/interop/exported.xml --json
```

Expected outputs:

- `examples/plcopen_xml_st_complete/interop/exported.xml`
- `examples/plcopen_xml_st_complete/interop/exported.source-map.json`
- JSON report containing at least:
  - `pou_count`
  - `data_type_count`
  - `configuration_count`
  - `resource_count`
  - `task_count`
  - `program_instance_count`

## Flow B: Import PLCopen XML -> ST Project

```bash
mkdir -p /tmp/trust-plcopen-import
trust-runtime plcopen import \
  --input examples/plcopen_xml_st_complete/interop/codesys-small.xml \
  --project /tmp/trust-plcopen-import --json
```

Expected outputs:

- Imported ST files under `/tmp/trust-plcopen-import/sources/`
  - POU sources
  - generated `TYPE` source (`plcopen_data_types*.st`)
  - generated configuration source (`plcopen_configuration_*.st`)
- Migration report:
  - `/tmp/trust-plcopen-import/interop/plcopen-migration-report.json`

## Flow C: Deterministic Round-Trip Check

Export after import, re-import, and export again. For supported ST structures,
POU/type/configuration signatures should be stable.

```bash
trust-runtime plcopen export \
  --project /tmp/trust-plcopen-import \
  --output /tmp/trust-plcopen-import/interop/roundtrip-a.xml

mkdir -p /tmp/trust-plcopen-roundtrip
trust-runtime plcopen import \
  --input /tmp/trust-plcopen-import/interop/roundtrip-a.xml \
  --project /tmp/trust-plcopen-roundtrip

trust-runtime plcopen export \
  --project /tmp/trust-plcopen-roundtrip \
  --output /tmp/trust-plcopen-roundtrip/interop/roundtrip-b.xml
```

The CI regression gate for this is:

- `crates/trust-runtime/tests/plcopen_st_complete_parity.rs`

and uses fixture packs in:

- `crates/trust-runtime/tests/fixtures/plcopen/codesys_st_complete/`

## Out Of Scope (By Design)

- FBD/LD/SFC graphical network semantics.
- Vendor-specific deployment/safety metadata execution.
- Full vendor library semantic equivalence beyond documented shim mappings.
