//! Conformance suite runner command.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::json;
use trust_runtime::harness::TestHarness;
use trust_runtime::value::{Duration, Value};
use trust_runtime::RestartMode;

const PROFILE_NAME: &str = "trust-conformance-v1";
const CATEGORIES: [&str; 6] = [
    "timers",
    "edges",
    "scan_cycle",
    "init_reset",
    "arithmetic",
    "memory_map",
];

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
enum CaseKind {
    #[default]
    Runtime,
    CompileError,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct RestartDirective {
    before_cycle: u32,
    mode: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct CaseManifest {
    id: String,
    category: String,
    description: Option<String>,
    kind: CaseKind,
    cycles: u32,
    sources: Vec<String>,
    watch_globals: Vec<String>,
    watch_direct: Vec<String>,
    advance_ms: Vec<i64>,
    input_series: BTreeMap<String, Vec<String>>,
    direct_input_series: BTreeMap<String, Vec<String>>,
    restarts: Vec<RestartDirective>,
}

#[derive(Debug, Clone)]
struct CaseDefinition {
    id: String,
    category: String,
    dir: PathBuf,
    manifest: CaseManifest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaseStatus {
    Passed,
    Failed,
    Error,
    Skipped,
}

impl CaseStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct SummaryReason {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct SummaryCaseResult {
    case_id: String,
    category: String,
    status: String,
    expected_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    actual_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cycles: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<SummaryReason>,
}

#[derive(Debug, Clone, Serialize)]
struct SummaryTotals {
    total: usize,
    passed: usize,
    failed: usize,
    errors: usize,
    skipped: usize,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeSummaryMeta {
    name: String,
    version: String,
    target: String,
}

#[derive(Debug, Clone, Serialize)]
struct SummaryOutput {
    version: u32,
    profile: String,
    generated_at_utc: String,
    ordering: String,
    runtime: RuntimeSummaryMeta,
    summary: SummaryTotals,
    results: Vec<SummaryCaseResult>,
}

#[derive(Debug, Clone)]
struct CaseArtifact {
    payload: serde_json::Value,
    cycles: Option<u64>,
}

pub fn run_conformance(
    suite_root: Option<PathBuf>,
    output: Option<PathBuf>,
    update_expected: bool,
    filter: Option<String>,
) -> anyhow::Result<()> {
    let suite_root = resolve_suite_root(suite_root)?;
    let mut cases = discover_cases(&suite_root)?;
    if let Some(filter) = filter.as_deref() {
        let needle = filter.to_ascii_lowercase();
        cases.retain(|case| case.id.to_ascii_lowercase().contains(&needle));
    }
    if cases.is_empty() {
        bail!(
            "no conformance cases discovered under {}",
            suite_root.display()
        );
    }
    cases.sort_by(|left, right| left.id.cmp(&right.id));

    let reports_root = suite_root.join("reports");
    fs::create_dir_all(&reports_root)
        .with_context(|| format!("create reports directory '{}'", reports_root.display()))?;
    let actual_root = reports_root.join("actual");
    fs::create_dir_all(&actual_root)
        .with_context(|| format!("create actual report directory '{}'", actual_root.display()))?;

    let timestamp = now_utc_parts();
    let output_path = output.unwrap_or_else(|| {
        reports_root.join(format!("{}_trust-runtime_summary.json", timestamp.compact))
    });
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create summary output parent '{}'", parent.display()))?;
    }

    let mut results = Vec::with_capacity(cases.len());
    let mut passed = 0_usize;
    let mut failed = 0_usize;
    let mut errors = 0_usize;
    let mut skipped = 0_usize;

    for case in &cases {
        let started = Instant::now();
        let expected_ref = format!("expected/{}/{}.json", case.category, case.id);
        let expected_path = suite_root.join(&expected_ref);
        let actual_ref = format!("reports/actual/{}.json", case.id);
        let actual_path = suite_root.join(&actual_ref);

        let mut summary_result = SummaryCaseResult {
            case_id: case.id.clone(),
            category: case.category.clone(),
            status: CaseStatus::Error.as_str().to_string(),
            expected_ref,
            actual_ref: None,
            duration_ms: None,
            cycles: None,
            reason: None,
        };

        match execute_case(case) {
            Ok(artifact) => {
                summary_result.cycles = artifact.cycles;
                if update_expected {
                    if let Err(err) = write_json_pretty(&expected_path, &artifact.payload) {
                        summary_result.status = CaseStatus::Error.as_str().to_string();
                        summary_result.reason = Some(reason(
                            "expected_write_error",
                            "failed writing expected artifact",
                            Some(err.to_string()),
                        ));
                        errors += 1;
                    } else {
                        summary_result.status = CaseStatus::Passed.as_str().to_string();
                        passed += 1;
                    }
                } else if !expected_path.is_file() {
                    summary_result.status = CaseStatus::Error.as_str().to_string();
                    summary_result.actual_ref = Some(actual_ref.clone());
                    summary_result.reason = Some(reason(
                        "expected_missing",
                        "expected artifact is missing",
                        Some(expected_path.display().to_string()),
                    ));
                    let _ = write_json_pretty(&actual_path, &artifact.payload);
                    errors += 1;
                } else {
                    match read_json_value(&expected_path) {
                        Ok(expected) if expected == artifact.payload => {
                            summary_result.status = CaseStatus::Passed.as_str().to_string();
                            passed += 1;
                        }
                        Ok(_) => {
                            summary_result.status = CaseStatus::Failed.as_str().to_string();
                            summary_result.actual_ref = Some(actual_ref.clone());
                            summary_result.reason = Some(reason(
                                "expected_mismatch",
                                "actual artifact differs from expected",
                                None,
                            ));
                            let _ = write_json_pretty(&actual_path, &artifact.payload);
                            failed += 1;
                        }
                        Err(err) => {
                            summary_result.status = CaseStatus::Error.as_str().to_string();
                            summary_result.actual_ref = Some(actual_ref.clone());
                            summary_result.reason = Some(reason(
                                "expected_read_error",
                                "failed reading expected artifact",
                                Some(err.to_string()),
                            ));
                            let _ = write_json_pretty(&actual_path, &artifact.payload);
                            errors += 1;
                        }
                    }
                }
            }
            Err(err) => {
                summary_result.status = CaseStatus::Error.as_str().to_string();
                summary_result.reason = Some(reason(
                    "case_execution_error",
                    "case execution failed",
                    Some(err.to_string()),
                ));
                errors += 1;
            }
        }
        summary_result.duration_ms = Some(elapsed_ms(started.elapsed()));
        if summary_result.status == CaseStatus::Skipped.as_str() {
            skipped += 1;
        }
        results.push(summary_result);
    }

    let summary = SummaryOutput {
        version: 1,
        profile: PROFILE_NAME.to_string(),
        generated_at_utc: timestamp.rfc3339,
        ordering: "case_id_asc".to_string(),
        runtime: RuntimeSummaryMeta {
            name: "trust-runtime".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            target: format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        },
        summary: SummaryTotals {
            total: results.len(),
            passed,
            failed,
            errors,
            skipped,
        },
        results,
    };

    let rendered =
        serde_json::to_string_pretty(&summary).context("serialize conformance summary")?;
    println!("{rendered}");
    fs::write(&output_path, format!("{rendered}\n")).with_context(|| {
        format!(
            "write conformance summary output '{}'",
            output_path.display()
        )
    })?;

    if summary.summary.failed > 0 || summary.summary.errors > 0 {
        bail!(
            "conformance failed: {} failed, {} errors",
            summary.summary.failed,
            summary.summary.errors
        );
    }
    Ok(())
}

fn resolve_suite_root(suite_root: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let root = match suite_root {
        Some(path) => path,
        None => {
            let cwd = std::env::current_dir().context("resolve current directory")?;
            cwd.join("conformance")
        }
    };
    if !root.is_dir() {
        bail!(
            "conformance suite root '{}' does not exist or is not a directory",
            root.display()
        );
    }
    Ok(root)
}

fn discover_cases(suite_root: &Path) -> anyhow::Result<Vec<CaseDefinition>> {
    let mut cases = Vec::new();
    for category in CATEGORIES {
        let category_root = suite_root.join("cases").join(category);
        if !category_root.is_dir() {
            continue;
        }
        let mut entries = fs::read_dir(&category_root)
            .with_context(|| format!("read case category '{}'", category_root.display()))?
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("list case category '{}'", category_root.display()))?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let case_id = entry.file_name().to_string_lossy().to_string();
            let manifest_path = path.join("manifest.toml");
            if !manifest_path.is_file() {
                continue;
            }
            let manifest = parse_manifest(&manifest_path)?;
            if manifest.id != case_id {
                bail!(
                    "manifest id '{}' does not match case directory '{}'",
                    manifest.id,
                    case_id
                );
            }
            if manifest.category != category {
                bail!(
                    "manifest category '{}' does not match directory category '{}'",
                    manifest.category,
                    category
                );
            }
            if !is_valid_case_id(&manifest.id, category) {
                bail!(
                    "case id '{}' violates conformance naming rules for category '{}'",
                    manifest.id,
                    category
                );
            }
            cases.push(CaseDefinition {
                id: case_id,
                category: category.to_string(),
                dir: path,
                manifest,
            });
        }
    }
    Ok(cases)
}

fn parse_manifest(path: &Path) -> anyhow::Result<CaseManifest> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("read case manifest '{}'", path.display()))?;
    let mut manifest: CaseManifest =
        toml::from_str(&text).with_context(|| format!("parse manifest '{}'", path.display()))?;
    if manifest.sources.is_empty() {
        manifest.sources = vec!["program.st".to_string()];
    }
    if manifest.id.trim().is_empty() {
        bail!("manifest '{}' is missing non-empty `id`", path.display());
    }
    if manifest.category.trim().is_empty() {
        bail!(
            "manifest '{}' is missing non-empty `category`",
            path.display()
        );
    }
    Ok(manifest)
}

fn execute_case(case: &CaseDefinition) -> anyhow::Result<CaseArtifact> {
    let sources = load_case_sources(case)?;
    match case.manifest.kind {
        CaseKind::Runtime => execute_runtime_case(case, &sources),
        CaseKind::CompileError => execute_compile_error_case(case, &sources),
    }
}

fn load_case_sources(case: &CaseDefinition) -> anyhow::Result<Vec<String>> {
    let mut sources = Vec::with_capacity(case.manifest.sources.len());
    for file in &case.manifest.sources {
        let path = case.dir.join(file);
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read case source '{}'", path.display()))?;
        sources.push(text);
    }
    Ok(sources)
}

fn execute_runtime_case(case: &CaseDefinition, sources: &[String]) -> anyhow::Result<CaseArtifact> {
    let cycles = case.manifest.cycles;
    if cycles == 0 {
        bail!("runtime case '{}' must declare cycles > 0", case.id);
    }
    validate_series_lengths(case, cycles)?;

    let source_refs = sources.iter().map(String::as_str).collect::<Vec<_>>();
    let mut harness =
        TestHarness::from_sources(&source_refs).map_err(|err| anyhow!(err.to_string()))?;

    let mut trace = Vec::with_capacity(cycles as usize);
    for cycle_idx in 0..(cycles as usize) {
        let cycle_number = u32::try_from(cycle_idx + 1).unwrap_or(u32::MAX);
        for restart in case
            .manifest
            .restarts
            .iter()
            .filter(|entry| entry.before_cycle == cycle_number)
        {
            let mode = parse_restart_mode(&restart.mode)?;
            harness
                .restart(mode)
                .map_err(|err| anyhow!("restart before cycle {cycle_number} failed: {err}"))?;
        }

        if !case.manifest.advance_ms.is_empty() {
            let advance = case.manifest.advance_ms[cycle_idx];
            harness.advance_time(Duration::from_millis(advance));
        }

        for (name, series) in &case.manifest.input_series {
            let raw = &series[cycle_idx];
            if should_skip_step_value(raw) {
                continue;
            }
            let value = parse_typed_value(raw)
                .with_context(|| format!("parse input series value for '{name}'"))?;
            harness.set_input(name, value);
        }

        for (address, series) in &case.manifest.direct_input_series {
            let raw = &series[cycle_idx];
            if should_skip_step_value(raw) {
                continue;
            }
            let value = parse_typed_value(raw)
                .with_context(|| format!("parse direct input value for '{address}'"))?;
            harness
                .set_direct_input(address, value)
                .with_context(|| format!("set direct input '{address}'"))?;
        }

        let cycle_result = harness.cycle();
        let mut globals = BTreeMap::new();
        for name in &case.manifest.watch_globals {
            let value = harness
                .get_output(name)
                .ok_or_else(|| anyhow!("watch global '{name}' is missing"))?;
            globals.insert(name.clone(), encode_value(&value));
        }

        let mut direct = BTreeMap::new();
        for address in &case.manifest.watch_direct {
            let value = harness
                .get_direct_output(address)
                .with_context(|| format!("read direct output '{address}'"))?;
            direct.insert(address.clone(), encode_value(&value));
        }

        let errors = cycle_result
            .errors
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>();

        trace.push(json!({
            "cycle": cycle_result.cycle_number,
            "runtime_time_nanos": cycle_result.elapsed_time.as_nanos(),
            "globals": globals,
            "direct": direct,
            "errors": errors
        }));
    }

    Ok(CaseArtifact {
        payload: json!({
            "version": 1,
            "case_id": case.id,
            "category": case.category,
            "kind": "runtime",
            "description": case.manifest.description,
            "cycles": cycles,
            "trace": trace
        }),
        cycles: Some(u64::from(cycles)),
    })
}

fn execute_compile_error_case(
    case: &CaseDefinition,
    sources: &[String],
) -> anyhow::Result<CaseArtifact> {
    let source_refs = sources.iter().map(String::as_str).collect::<Vec<_>>();
    let compile_result = TestHarness::from_sources(&source_refs);
    let (compiled, error) = match compile_result {
        Ok(_) => (true, None),
        Err(err) => (false, Some(err.to_string())),
    };
    Ok(CaseArtifact {
        payload: json!({
            "version": 1,
            "case_id": case.id,
            "category": case.category,
            "kind": "compile_error",
            "description": case.manifest.description,
            "compiled": compiled,
            "error": error
        }),
        cycles: None,
    })
}

fn validate_series_lengths(case: &CaseDefinition, cycles: u32) -> anyhow::Result<()> {
    let expected_len = usize::try_from(cycles).unwrap_or(usize::MAX);
    if !case.manifest.advance_ms.is_empty() && case.manifest.advance_ms.len() != expected_len {
        bail!(
            "case '{}' advance_ms length {} must equal cycles {}",
            case.id,
            case.manifest.advance_ms.len(),
            cycles
        );
    }
    for (name, series) in &case.manifest.input_series {
        if series.len() != expected_len {
            bail!(
                "case '{}' input series '{}' length {} must equal cycles {}",
                case.id,
                name,
                series.len(),
                cycles
            );
        }
    }
    for (address, series) in &case.manifest.direct_input_series {
        if series.len() != expected_len {
            bail!(
                "case '{}' direct input series '{}' length {} must equal cycles {}",
                case.id,
                address,
                series.len(),
                cycles
            );
        }
    }
    for restart in &case.manifest.restarts {
        if restart.before_cycle == 0 || restart.before_cycle > cycles {
            bail!(
                "case '{}' restart before_cycle {} must be within 1..={}",
                case.id,
                restart.before_cycle,
                cycles
            );
        }
    }
    Ok(())
}

fn parse_restart_mode(mode: &str) -> anyhow::Result<RestartMode> {
    match mode.to_ascii_lowercase().as_str() {
        "cold" => Ok(RestartMode::Cold),
        "warm" => Ok(RestartMode::Warm),
        _ => bail!("unsupported restart mode '{mode}', expected warm|cold"),
    }
}

fn should_skip_step_value(value: &str) -> bool {
    value.eq_ignore_ascii_case("skip") || value == "_"
}

fn parse_typed_value(raw: &str) -> anyhow::Result<Value> {
    let (kind, payload) = raw
        .split_once(':')
        .ok_or_else(|| anyhow!("typed value must be KIND:VALUE, got '{raw}'"))?;
    let payload = payload.trim();
    let normalized = kind.trim().to_ascii_uppercase();
    let number = |input: &str| -> anyhow::Result<String> { Ok(input.trim().replace('_', "")) };
    Ok(match normalized.as_str() {
        "BOOL" => Value::Bool(parse_bool(payload)?),
        "SINT" => Value::SInt(number(payload)?.parse::<i8>().context("parse SINT")?),
        "INT" => Value::Int(number(payload)?.parse::<i16>().context("parse INT")?),
        "DINT" => Value::DInt(number(payload)?.parse::<i32>().context("parse DINT")?),
        "LINT" => Value::LInt(number(payload)?.parse::<i64>().context("parse LINT")?),
        "USINT" => Value::USInt(number(payload)?.parse::<u8>().context("parse USINT")?),
        "UINT" => Value::UInt(number(payload)?.parse::<u16>().context("parse UINT")?),
        "UDINT" => Value::UDInt(number(payload)?.parse::<u32>().context("parse UDINT")?),
        "ULINT" => Value::ULInt(number(payload)?.parse::<u64>().context("parse ULINT")?),
        "BYTE" => Value::Byte(number(payload)?.parse::<u8>().context("parse BYTE")?),
        "WORD" => Value::Word(number(payload)?.parse::<u16>().context("parse WORD")?),
        "DWORD" => Value::DWord(number(payload)?.parse::<u32>().context("parse DWORD")?),
        "LWORD" => Value::LWord(number(payload)?.parse::<u64>().context("parse LWORD")?),
        "REAL" => Value::Real(number(payload)?.parse::<f32>().context("parse REAL")?),
        "LREAL" => Value::LReal(number(payload)?.parse::<f64>().context("parse LREAL")?),
        "TIME" => Value::Time(parse_duration(payload)?),
        "LTIME" => Value::LTime(parse_duration(payload)?),
        "STRING" => Value::String(payload.to_string().into()),
        _ => bail!("unsupported typed value kind '{normalized}'"),
    })
}

fn parse_bool(raw: &str) -> anyhow::Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => bail!("invalid BOOL literal '{raw}'"),
    }
}

fn parse_duration(raw: &str) -> anyhow::Result<Duration> {
    let text = raw.trim().to_ascii_lowercase().replace('_', "");
    if let Some(millis) = text.strip_suffix("ms") {
        let value = millis.parse::<i64>().context("parse TIME milliseconds")?;
        return Ok(Duration::from_millis(value));
    }
    if let Some(nanos) = text.strip_suffix("ns") {
        let value = nanos.parse::<i64>().context("parse TIME nanoseconds")?;
        return Ok(Duration::from_nanos(value));
    }
    if let Some(seconds) = text.strip_suffix('s') {
        let value = seconds.parse::<i64>().context("parse TIME seconds")?;
        return Ok(Duration::from_secs(value));
    }
    let value = text.parse::<i64>().context("parse TIME as milliseconds")?;
    Ok(Duration::from_millis(value))
}

fn encode_value(value: &Value) -> serde_json::Value {
    match value {
        Value::Bool(v) => json!({"type": "BOOL", "value": v}),
        Value::SInt(v) => json!({"type": "SINT", "value": v}),
        Value::Int(v) => json!({"type": "INT", "value": v}),
        Value::DInt(v) => json!({"type": "DINT", "value": v}),
        Value::LInt(v) => json!({"type": "LINT", "value": v}),
        Value::USInt(v) => json!({"type": "USINT", "value": v}),
        Value::UInt(v) => json!({"type": "UINT", "value": v}),
        Value::UDInt(v) => json!({"type": "UDINT", "value": v}),
        Value::ULInt(v) => json!({"type": "ULINT", "value": v}),
        Value::Real(v) => json!({"type": "REAL", "value": v}),
        Value::LReal(v) => json!({"type": "LREAL", "value": v}),
        Value::Byte(v) => json!({"type": "BYTE", "value": v}),
        Value::Word(v) => json!({"type": "WORD", "value": v}),
        Value::DWord(v) => json!({"type": "DWORD", "value": v}),
        Value::LWord(v) => json!({"type": "LWORD", "value": v}),
        Value::Time(v) => json!({"type": "TIME", "nanos": v.as_nanos()}),
        Value::LTime(v) => json!({"type": "LTIME", "nanos": v.as_nanos()}),
        Value::Date(v) => json!({"type": "DATE", "ticks": v.ticks()}),
        Value::LDate(v) => json!({"type": "LDATE", "nanos": v.nanos()}),
        Value::Tod(v) => json!({"type": "TOD", "ticks": v.ticks()}),
        Value::LTod(v) => json!({"type": "LTOD", "nanos": v.nanos()}),
        Value::Dt(v) => json!({"type": "DT", "ticks": v.ticks()}),
        Value::Ldt(v) => json!({"type": "LDT", "nanos": v.nanos()}),
        Value::String(v) => json!({"type": "STRING", "value": v.to_string()}),
        Value::WString(v) => json!({"type": "WSTRING", "value": v}),
        Value::Char(v) => json!({"type": "CHAR", "value": v}),
        Value::WChar(v) => json!({"type": "WCHAR", "value": v}),
        Value::Array(array) => json!({
            "type": "ARRAY",
            "dimensions": array.dimensions,
            "elements": array.elements.iter().map(encode_value).collect::<Vec<_>>()
        }),
        Value::Struct(value) => {
            let mut fields = BTreeMap::new();
            for (name, field_value) in &value.fields {
                fields.insert(name.to_string(), encode_value(field_value));
            }
            json!({
                "type": "STRUCT",
                "type_name": value.type_name.to_string(),
                "fields": fields
            })
        }
        Value::Enum(value) => json!({
            "type": "ENUM",
            "type_name": value.type_name.to_string(),
            "variant": value.variant_name.to_string(),
            "numeric": value.numeric_value
        }),
        Value::Reference(reference) => json!({
            "type": "REFERENCE",
            "value": reference.as_ref().map(|entry| format!("{entry:?}"))
        }),
        Value::Instance(id) => json!({"type": "INSTANCE", "value": id.0}),
        Value::Null => json!({"type": "NULL"}),
    }
}

fn read_json_value(path: &Path) -> anyhow::Result<serde_json::Value> {
    let text =
        fs::read_to_string(path).with_context(|| format!("read json file '{}'", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parse json file '{}'", path.display()))
}

fn write_json_pretty(path: &Path, value: &serde_json::Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create directory '{}'", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value).context("serialize json payload")?;
    fs::write(path, format!("{text}\n")).with_context(|| format!("write '{}'", path.display()))
}

fn reason(code: &str, message: &str, details: Option<String>) -> SummaryReason {
    SummaryReason {
        code: code.to_string(),
        message: message.to_string(),
        details,
    }
}

fn elapsed_ms(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn is_valid_case_id(id: &str, category: &str) -> bool {
    if !id.starts_with(&format!("cfm_{category}_")) {
        return false;
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return false;
    }
    let Some(last) = id.rsplit('_').next() else {
        return false;
    };
    last.len() == 3 && last.chars().all(|ch| ch.is_ascii_digit())
}

struct UtcParts {
    rfc3339: String,
    compact: String,
}

fn now_utc_parts() -> UtcParts {
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let (year, month, day, hour, minute, second) = split_unix_utc(unix_secs);
    UtcParts {
        rfc3339: format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"),
        compact: format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z"),
    }
}

fn split_unix_utc(unix_secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = unix_secs.div_euclid(86_400);
    let seconds_in_day = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_in_day / 3600;
    let minute = (seconds_in_day % 3600) / 60;
    let second = seconds_in_day % 60;
    (year, month, day, hour, minute, second)
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_id_validation_matches_naming_rules() {
        assert!(is_valid_case_id("cfm_timers_ton_sequence_001", "timers"));
        assert!(is_valid_case_id(
            "cfm_memory_map_sync_word_123",
            "memory_map"
        ));
        assert!(!is_valid_case_id("CFM_timers_ton_sequence_001", "timers"));
        assert!(!is_valid_case_id("cfm_timers_ton_sequence_01", "timers"));
        assert!(!is_valid_case_id("cfm_edges_case_001", "timers"));
    }

    #[test]
    fn parse_typed_values_supports_core_manifest_types() {
        assert_eq!(
            parse_typed_value("BOOL:true").expect("bool"),
            Value::Bool(true)
        );
        assert_eq!(parse_typed_value("INT:-4").expect("int"), Value::Int(-4));
        assert_eq!(parse_typed_value("WORD:41").expect("word"), Value::Word(41));
        assert_eq!(
            parse_typed_value("TIME:10ms").expect("time"),
            Value::Time(Duration::from_millis(10))
        );
    }

    #[test]
    fn unix_split_produces_epoch() {
        let parts = split_unix_utc(0);
        assert_eq!(parts, (1970, 1, 1, 0, 0, 0));
    }
}
