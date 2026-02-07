//! Runtime historian and observability helpers.

#![allow(missing_docs)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use glob::Pattern;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use tracing::warn;

use crate::debug::DebugSnapshot;
use crate::error::RuntimeError;
use crate::metrics::RuntimeMetricsSnapshot;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    All,
    Allowlist,
}

#[derive(Debug, Clone)]
pub struct AlertRule {
    pub name: SmolStr,
    pub variable: SmolStr,
    pub above: Option<f64>,
    pub below: Option<f64>,
    pub debounce_samples: u32,
    pub hook: Option<SmolStr>,
}

#[derive(Debug, Clone)]
pub struct HistorianConfig {
    pub enabled: bool,
    pub sample_interval_ms: u64,
    pub mode: RecordingMode,
    pub include: Vec<SmolStr>,
    pub history_path: PathBuf,
    pub max_entries: usize,
    pub prometheus_enabled: bool,
    pub prometheus_path: SmolStr,
    pub alerts: Vec<AlertRule>,
}

impl Default for HistorianConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sample_interval_ms: 1_000,
            mode: RecordingMode::All,
            include: Vec::new(),
            history_path: PathBuf::from("history/historian.jsonl"),
            max_entries: 20_000,
            prometheus_enabled: true,
            prometheus_path: SmolStr::new("/metrics"),
            alerts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistorianSample {
    pub timestamp_ms: u128,
    pub source_time_ns: i64,
    pub variable: String,
    pub value: HistorianValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum HistorianValue {
    Bool(bool),
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    String(String),
}

impl HistorianValue {
    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Bool(value) => Some(if *value { 1.0 } else { 0.0 }),
            Self::Integer(value) => Some(*value as f64),
            Self::Unsigned(value) => Some(*value as f64),
            Self::Float(value) => Some(*value),
            Self::String(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertState {
    Triggered,
    Cleared,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HistorianAlertEvent {
    pub timestamp_ms: u128,
    pub rule: String,
    pub variable: String,
    pub state: AlertState,
    pub value: Option<f64>,
    pub threshold: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HistorianPrometheusSnapshot {
    pub samples_total: u64,
    pub series_total: usize,
    pub alerts_total: u64,
}

#[derive(Debug, Clone)]
enum HookTarget {
    Log,
    File(PathBuf),
    Webhook(SmolStr),
}

#[derive(Debug, Clone)]
struct CompiledAlertRule {
    name: SmolStr,
    variable: SmolStr,
    above: Option<f64>,
    below: Option<f64>,
    debounce_samples: u32,
    hook: Option<HookTarget>,
}

#[derive(Debug, Clone, Default)]
struct AlertTracker {
    active: bool,
    consecutive: u32,
}

#[derive(Debug, Default)]
struct HistorianInner {
    samples: VecDeque<HistorianSample>,
    tracked_variables: HashSet<String>,
    samples_total: u64,
    last_capture_ms: Option<u128>,
    alert_trackers: HashMap<SmolStr, AlertTracker>,
    alerts: VecDeque<HistorianAlertEvent>,
    alerts_total: u64,
}

#[derive(Debug)]
pub struct HistorianService {
    config: HistorianConfig,
    include_patterns: Vec<Pattern>,
    alert_rules: Vec<CompiledAlertRule>,
    inner: Mutex<HistorianInner>,
}

impl HistorianService {
    pub fn new(
        config: HistorianConfig,
        bundle_root: Option<&Path>,
    ) -> Result<Arc<Self>, RuntimeError> {
        let history_path = resolve_path(&config.history_path, bundle_root);
        if let Some(parent) = history_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                RuntimeError::ControlError(format!("historian path setup failed: {err}").into())
            })?;
        }
        let include_patterns = compile_patterns(&config.include)?;
        let alert_rules = compile_alert_rules(&config.alerts, bundle_root)?;

        let mut inner = HistorianInner::default();
        load_existing_samples(&history_path, config.max_entries, &mut inner)?;

        let mut runtime_config = config.clone();
        runtime_config.history_path = history_path;

        Ok(Arc::new(Self {
            config: runtime_config,
            include_patterns,
            alert_rules,
            inner: Mutex::new(inner),
        }))
    }

    #[must_use]
    pub fn config(&self) -> &HistorianConfig {
        &self.config
    }

    pub fn start_sampler(self: Arc<Self>, debug: crate::debug::DebugControl) {
        let interval = self.config.sample_interval_ms.max(1);
        let poll_ms = (interval / 2).clamp(10, 1_000);
        thread::spawn(move || loop {
            if let Some(snapshot) = debug.snapshot() {
                let now_ms = unix_ms();
                let _ = self.capture_snapshot_at(&snapshot, now_ms);
            }
            thread::sleep(Duration::from_millis(poll_ms));
        });
    }

    pub fn capture_snapshot_at(
        &self,
        snapshot: &DebugSnapshot,
        timestamp_ms: u128,
    ) -> Result<usize, RuntimeError> {
        let interval_ms = u128::from(self.config.sample_interval_ms.max(1));
        let mut pending_hooks: Vec<(HookTarget, HistorianAlertEvent)> = Vec::new();

        let recorded = {
            let mut inner = self
                .inner
                .lock()
                .map_err(|_| RuntimeError::ControlError("historian unavailable".into()))?;
            if let Some(last) = inner.last_capture_ms {
                if timestamp_ms.saturating_sub(last) < interval_ms {
                    return Ok(0);
                }
            }

            let samples = collect_snapshot_samples(
                snapshot,
                &self.config,
                &self.include_patterns,
                timestamp_ms,
            );
            if samples.is_empty() {
                inner.last_capture_ms = Some(timestamp_ms);
                return Ok(0);
            }

            append_samples(&self.config.history_path, &samples)?;
            for sample in &samples {
                inner.samples.push_back(sample.clone());
                inner.tracked_variables.insert(sample.variable.clone());
                while inner.samples.len() > self.config.max_entries {
                    let _ = inner.samples.pop_front();
                }
            }
            inner.samples_total = inner.samples_total.saturating_add(samples.len() as u64);
            inner.last_capture_ms = Some(timestamp_ms);

            let mut latest_numeric = HashMap::<String, f64>::new();
            for sample in &samples {
                if let Some(value) = sample.value.as_f64() {
                    latest_numeric.insert(sample.variable.clone(), value);
                }
            }
            let alert_events = evaluate_alerts(
                &self.alert_rules,
                &latest_numeric,
                timestamp_ms,
                &mut inner.alert_trackers,
            );
            for (event, hook) in alert_events {
                inner.alerts.push_back(event.clone());
                while inner.alerts.len() > 1_000 {
                    let _ = inner.alerts.pop_front();
                }
                inner.alerts_total = inner.alerts_total.saturating_add(1);
                if let Some(target) = hook {
                    pending_hooks.push((target, event));
                }
            }

            samples.len()
        };

        for (target, event) in pending_hooks {
            dispatch_hook(&target, &event);
        }

        Ok(recorded)
    }

    #[must_use]
    pub fn query(
        &self,
        variable: Option<&str>,
        since_ms: Option<u128>,
        limit: usize,
    ) -> Vec<HistorianSample> {
        let limit = limit.clamp(1, 5_000);
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut items = inner
            .samples
            .iter()
            .rev()
            .filter(|sample| variable.is_none_or(|name| sample.variable.as_str() == name))
            .filter(|sample| since_ms.is_none_or(|value| sample.timestamp_ms >= value))
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        items.reverse();
        items
    }

    #[must_use]
    pub fn alerts(&self, limit: usize) -> Vec<HistorianAlertEvent> {
        let limit = limit.clamp(1, 1_000);
        let Ok(inner) = self.inner.lock() else {
            return Vec::new();
        };
        let mut items = inner
            .alerts
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        items.reverse();
        items
    }

    #[must_use]
    pub fn prometheus_path(&self) -> Option<&str> {
        if self.config.prometheus_enabled {
            Some(self.config.prometheus_path.as_str())
        } else {
            None
        }
    }

    #[must_use]
    pub fn prometheus_snapshot(&self) -> HistorianPrometheusSnapshot {
        let Ok(inner) = self.inner.lock() else {
            return HistorianPrometheusSnapshot::default();
        };
        HistorianPrometheusSnapshot {
            samples_total: inner.samples_total,
            series_total: inner.tracked_variables.len(),
            alerts_total: inner.alerts_total,
        }
    }

    #[must_use]
    pub fn render_prometheus(&self, runtime: &RuntimeMetricsSnapshot) -> String {
        render_prometheus(runtime, Some(self.prometheus_snapshot()))
    }
}

fn compile_patterns(patterns: &[SmolStr]) -> Result<Vec<Pattern>, RuntimeError> {
    patterns
        .iter()
        .map(|pattern| {
            Pattern::new(pattern.as_str()).map_err(|err| {
                RuntimeError::InvalidConfig(
                    format!("runtime.observability.include invalid pattern '{pattern}': {err}")
                        .into(),
                )
            })
        })
        .collect()
}

fn compile_alert_rules(
    rules: &[AlertRule],
    bundle_root: Option<&Path>,
) -> Result<Vec<CompiledAlertRule>, RuntimeError> {
    rules
        .iter()
        .map(|rule| {
            if rule.name.trim().is_empty() {
                return Err(RuntimeError::InvalidConfig(
                    "runtime.observability.alerts[].name must not be empty".into(),
                ));
            }
            if rule.variable.trim().is_empty() {
                return Err(RuntimeError::InvalidConfig(
                    "runtime.observability.alerts[].variable must not be empty".into(),
                ));
            }
            if rule.above.is_none() && rule.below.is_none() {
                return Err(RuntimeError::InvalidConfig(
                    format!(
                        "runtime.observability.alert '{}': set above and/or below threshold",
                        rule.name
                    )
                    .into(),
                ));
            }
            if rule.debounce_samples == 0 {
                return Err(RuntimeError::InvalidConfig(
                    format!(
                        "runtime.observability.alert '{}': debounce_samples must be >= 1",
                        rule.name
                    )
                    .into(),
                ));
            }

            let hook = rule.hook.as_deref().map(|value| {
                if value.eq_ignore_ascii_case("log") {
                    HookTarget::Log
                } else if value.starts_with("http://") || value.starts_with("https://") {
                    HookTarget::Webhook(SmolStr::new(value))
                } else {
                    HookTarget::File(resolve_path(Path::new(value), bundle_root))
                }
            });

            Ok(CompiledAlertRule {
                name: rule.name.clone(),
                variable: rule.variable.clone(),
                above: rule.above,
                below: rule.below,
                debounce_samples: rule.debounce_samples,
                hook,
            })
        })
        .collect()
}

fn load_existing_samples(
    path: &Path,
    max_entries: usize,
    inner: &mut HistorianInner,
) -> Result<(), RuntimeError> {
    if !path.is_file() {
        return Ok(());
    }
    let file = std::fs::File::open(path).map_err(|err| {
        RuntimeError::ControlError(format!("historian open failed: {err}").into())
    })?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => continue,
        };
        let Ok(sample) = serde_json::from_str::<HistorianSample>(&line) else {
            continue;
        };
        inner.tracked_variables.insert(sample.variable.clone());
        inner.samples.push_back(sample);
        while inner.samples.len() > max_entries {
            let _ = inner.samples.pop_front();
        }
        inner.samples_total = inner.samples_total.saturating_add(1);
    }
    Ok(())
}

fn collect_snapshot_samples(
    snapshot: &DebugSnapshot,
    config: &HistorianConfig,
    patterns: &[Pattern],
    timestamp_ms: u128,
) -> Vec<HistorianSample> {
    let mut samples = Vec::<HistorianSample>::new();
    let mut values = Vec::<(String, HistorianValue)>::new();
    for (name, value) in snapshot.storage.globals() {
        flatten_value(
            name.as_str(),
            value,
            &snapshot.storage,
            config,
            patterns,
            &mut values,
        );
    }
    for (name, value) in snapshot.storage.retain() {
        let path = format!("retain.{name}");
        flatten_value(
            path.as_str(),
            value,
            &snapshot.storage,
            config,
            patterns,
            &mut values,
        );
    }
    samples.reserve(values.len());
    for (variable, value) in values {
        samples.push(HistorianSample {
            timestamp_ms,
            source_time_ns: snapshot.now.as_nanos(),
            variable,
            value,
        });
    }
    samples
}

fn flatten_value(
    path: &str,
    value: &Value,
    storage: &crate::memory::VariableStorage,
    config: &HistorianConfig,
    patterns: &[Pattern],
    out: &mut Vec<(String, HistorianValue)>,
) {
    if let Some(hist_value) = to_historian_value(value) {
        if should_record(path, config, patterns) {
            out.push((path.to_string(), hist_value));
        }
        return;
    }
    match value {
        Value::Struct(value) => {
            for (field, field_value) in &value.fields {
                let nested = format!("{path}.{field}");
                flatten_value(nested.as_str(), field_value, storage, config, patterns, out);
            }
        }
        Value::Array(value) => {
            for (idx, element) in value.elements.iter().enumerate() {
                let nested = format!("{path}[{idx}]");
                flatten_value(nested.as_str(), element, storage, config, patterns, out);
            }
        }
        Value::Instance(instance_id) => {
            if let Some(instance) = storage.get_instance(*instance_id) {
                for (field, field_value) in &instance.variables {
                    let nested = format!("{path}.{field}");
                    flatten_value(nested.as_str(), field_value, storage, config, patterns, out);
                }
            }
        }
        _ => {}
    }
}

fn should_record(path: &str, config: &HistorianConfig, patterns: &[Pattern]) -> bool {
    match config.mode {
        RecordingMode::All => true,
        RecordingMode::Allowlist => patterns.iter().any(|pattern| pattern.matches(path)),
    }
}

fn to_historian_value(value: &Value) -> Option<HistorianValue> {
    match value {
        Value::Bool(value) => Some(HistorianValue::Bool(*value)),
        Value::SInt(value) => Some(HistorianValue::Integer((*value).into())),
        Value::Int(value) => Some(HistorianValue::Integer((*value).into())),
        Value::DInt(value) => Some(HistorianValue::Integer((*value).into())),
        Value::LInt(value) => Some(HistorianValue::Integer(*value)),
        Value::USInt(value) => Some(HistorianValue::Unsigned((*value).into())),
        Value::UInt(value) => Some(HistorianValue::Unsigned((*value).into())),
        Value::UDInt(value) => Some(HistorianValue::Unsigned((*value).into())),
        Value::ULInt(value) => Some(HistorianValue::Unsigned(*value)),
        Value::Real(value) => Some(HistorianValue::Float(f64::from(*value))),
        Value::LReal(value) => Some(HistorianValue::Float(*value)),
        Value::Byte(value) => Some(HistorianValue::Unsigned((*value).into())),
        Value::Word(value) => Some(HistorianValue::Unsigned((*value).into())),
        Value::DWord(value) => Some(HistorianValue::Unsigned((*value).into())),
        Value::LWord(value) => Some(HistorianValue::Unsigned(*value)),
        Value::Time(value) | Value::LTime(value) => {
            Some(HistorianValue::Integer(value.as_millis()))
        }
        Value::Date(value) => Some(HistorianValue::Integer(value.ticks())),
        Value::LDate(value) => Some(HistorianValue::Integer(value.nanos())),
        Value::Tod(value) => Some(HistorianValue::Integer(value.ticks())),
        Value::LTod(value) => Some(HistorianValue::Integer(value.nanos())),
        Value::Dt(value) => Some(HistorianValue::Integer(value.ticks())),
        Value::Ldt(value) => Some(HistorianValue::Integer(value.nanos())),
        Value::String(value) => Some(HistorianValue::String(value.to_string())),
        Value::WString(value) => Some(HistorianValue::String(value.clone())),
        Value::Char(value) => Some(HistorianValue::String(char::from(*value).to_string())),
        Value::WChar(value) => {
            char::from_u32(u32::from(*value)).map(|ch| HistorianValue::String(ch.to_string()))
        }
        Value::Enum(value) => Some(HistorianValue::String(value.variant_name.to_string())),
        _ => None,
    }
}

fn append_samples(path: &Path, samples: &[HistorianSample]) -> Result<(), RuntimeError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| {
            RuntimeError::ControlError(format!("historian write failed: {err}").into())
        })?;
    for sample in samples {
        let line = serde_json::to_string(sample).map_err(|err| {
            RuntimeError::ControlError(format!("historian serialization failed: {err}").into())
        })?;
        file.write_all(line.as_bytes()).map_err(|err| {
            RuntimeError::ControlError(format!("historian write failed: {err}").into())
        })?;
        file.write_all(b"\n").map_err(|err| {
            RuntimeError::ControlError(format!("historian write failed: {err}").into())
        })?;
    }
    Ok(())
}

fn evaluate_alerts(
    rules: &[CompiledAlertRule],
    latest_numeric: &HashMap<String, f64>,
    timestamp_ms: u128,
    trackers: &mut HashMap<SmolStr, AlertTracker>,
) -> Vec<(HistorianAlertEvent, Option<HookTarget>)> {
    let mut events = Vec::new();
    for rule in rules {
        let value = latest_numeric.get(rule.variable.as_str()).copied();
        let breached = value.is_some_and(|v| threshold_breached(v, rule.above, rule.below));
        let tracker = trackers.entry(rule.name.clone()).or_default();

        if breached {
            tracker.consecutive = tracker.consecutive.saturating_add(1);
            if !tracker.active && tracker.consecutive >= rule.debounce_samples {
                tracker.active = true;
                events.push((
                    HistorianAlertEvent {
                        timestamp_ms,
                        rule: rule.name.to_string(),
                        variable: rule.variable.to_string(),
                        state: AlertState::Triggered,
                        value,
                        threshold: rule_threshold_text(rule.above, rule.below),
                    },
                    rule.hook.clone(),
                ));
            }
        } else {
            tracker.consecutive = 0;
            if tracker.active {
                tracker.active = false;
                events.push((
                    HistorianAlertEvent {
                        timestamp_ms,
                        rule: rule.name.to_string(),
                        variable: rule.variable.to_string(),
                        state: AlertState::Cleared,
                        value,
                        threshold: rule_threshold_text(rule.above, rule.below),
                    },
                    rule.hook.clone(),
                ));
            }
        }
    }
    events
}

fn threshold_breached(value: f64, above: Option<f64>, below: Option<f64>) -> bool {
    above.is_some_and(|limit| value > limit) || below.is_some_and(|limit| value < limit)
}

fn rule_threshold_text(above: Option<f64>, below: Option<f64>) -> String {
    match (above, below) {
        (Some(_), Some(_)) => "outside_band".to_string(),
        (Some(_), None) => "above".to_string(),
        (None, Some(_)) => "below".to_string(),
        (None, None) => "threshold".to_string(),
    }
}

fn dispatch_hook(target: &HookTarget, event: &HistorianAlertEvent) {
    match target {
        HookTarget::Log => {
            warn!(
                "historian alert {} for {} is {:?}",
                event.rule, event.variable, event.state
            );
        }
        HookTarget::File(path) => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
                if let Ok(line) = serde_json::to_string(event) {
                    let _ = file.write_all(line.as_bytes());
                    let _ = file.write_all(b"\n");
                }
            }
        }
        HookTarget::Webhook(url) => {
            let payload = match serde_json::to_string(event) {
                Ok(payload) => payload,
                Err(_) => return,
            };
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_millis(500))
                .timeout_read(Duration::from_millis(800))
                .build();
            if let Err(err) = agent
                .post(url.as_str())
                .set("Content-Type", "application/json")
                .send_string(payload.as_str())
            {
                warn!(
                    "historian webhook delivery failed for '{}': {err}",
                    event.rule
                );
            }
        }
    }
}

fn resolve_path(path: &Path, bundle_root: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match bundle_root {
        Some(root) => root.join(path),
        None => path.to_path_buf(),
    }
}

fn unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[must_use]
pub fn render_prometheus(
    runtime: &RuntimeMetricsSnapshot,
    historian: Option<HistorianPrometheusSnapshot>,
) -> String {
    let mut body = String::new();
    body.push_str("# HELP trust_runtime_uptime_ms Runtime uptime in milliseconds.\n");
    body.push_str("# TYPE trust_runtime_uptime_ms gauge\n");
    let _ = writeln!(body, "trust_runtime_uptime_ms {}", runtime.uptime_ms);

    body.push_str("# HELP trust_runtime_faults_total Runtime fault count.\n");
    body.push_str("# TYPE trust_runtime_faults_total counter\n");
    let _ = writeln!(body, "trust_runtime_faults_total {}", runtime.faults);

    body.push_str("# HELP trust_runtime_overruns_total Runtime cycle overrun count.\n");
    body.push_str("# TYPE trust_runtime_overruns_total counter\n");
    let _ = writeln!(body, "trust_runtime_overruns_total {}", runtime.overruns);

    body.push_str("# HELP trust_runtime_cycle_last_ms Last cycle duration in milliseconds.\n");
    body.push_str("# TYPE trust_runtime_cycle_last_ms gauge\n");
    let _ = writeln!(
        body,
        "trust_runtime_cycle_last_ms {:.6}",
        runtime.cycle.last_ms
    );

    body.push_str("# HELP trust_runtime_cycle_avg_ms Average cycle duration in milliseconds.\n");
    body.push_str("# TYPE trust_runtime_cycle_avg_ms gauge\n");
    let _ = writeln!(
        body,
        "trust_runtime_cycle_avg_ms {:.6}",
        runtime.cycle.avg_ms
    );

    body.push_str("# HELP trust_runtime_task_last_ms Last task duration in milliseconds.\n");
    body.push_str("# TYPE trust_runtime_task_last_ms gauge\n");
    for task in &runtime.tasks {
        let _ = writeln!(
            body,
            "trust_runtime_task_last_ms{{task=\"{}\"}} {:.6}",
            escape_label(task.name.as_str()),
            task.last_ms
        );
    }

    body.push_str("# HELP trust_runtime_task_overruns_total Task overrun count.\n");
    body.push_str("# TYPE trust_runtime_task_overruns_total counter\n");
    for task in &runtime.tasks {
        let _ = writeln!(
            body,
            "trust_runtime_task_overruns_total{{task=\"{}\"}} {}",
            escape_label(task.name.as_str()),
            task.overruns
        );
    }

    if let Some(historian) = historian {
        body.push_str(
            "# HELP trust_runtime_historian_samples_total Persisted historian samples.\n",
        );
        body.push_str("# TYPE trust_runtime_historian_samples_total counter\n");
        let _ = writeln!(
            body,
            "trust_runtime_historian_samples_total {}",
            historian.samples_total
        );

        body.push_str(
            "# HELP trust_runtime_historian_series_total Distinct historian variables tracked.\n",
        );
        body.push_str("# TYPE trust_runtime_historian_series_total gauge\n");
        let _ = writeln!(
            body,
            "trust_runtime_historian_series_total {}",
            historian.series_total
        );

        body.push_str("# HELP trust_runtime_historian_alerts_total Historian alert transitions.\n");
        body.push_str("# TYPE trust_runtime_historian_alerts_total counter\n");
        let _ = writeln!(
            body,
            "trust_runtime_historian_alerts_total {}",
            historian.alerts_total
        );
    }

    body
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::debug::DebugSnapshot;
    use crate::memory::VariableStorage;
    use crate::value::{Duration as PlcDuration, Value};

    fn temp_path(name: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("trust-historian-{name}-{stamp}.jsonl"))
    }

    fn snapshot_with_values(counter: i16, temp: f64, active: bool) -> DebugSnapshot {
        let mut storage = VariableStorage::default();
        storage.set_global("Counter", Value::Int(counter));
        storage.set_global("Temp", Value::LReal(temp));
        storage.set_global("Active", Value::Bool(active));
        storage.set_global("Label", Value::String(SmolStr::new("Pump-A")));
        DebugSnapshot {
            storage,
            now: PlcDuration::from_millis(1_000),
        }
    }

    fn basic_config(path: PathBuf) -> HistorianConfig {
        HistorianConfig {
            enabled: true,
            sample_interval_ms: 100,
            mode: RecordingMode::All,
            include: Vec::new(),
            history_path: path,
            max_entries: 1_000,
            prometheus_enabled: true,
            prometheus_path: SmolStr::new("/metrics"),
            alerts: Vec::new(),
        }
    }

    #[test]
    fn recording_fidelity_and_sample_interval_are_enforced() {
        let path = temp_path("fidelity");
        let service = HistorianService::new(basic_config(path.clone()), None).expect("service");
        let first = snapshot_with_values(7, 21.5, true);
        let second = snapshot_with_values(8, 25.0, false);

        let captured = service
            .capture_snapshot_at(&first, 1_000)
            .expect("capture first");
        assert!(captured >= 4);
        let skipped = service
            .capture_snapshot_at(&first, 1_050)
            .expect("capture skipped");
        assert_eq!(skipped, 0, "sample interval should suppress early capture");
        let captured_again = service
            .capture_snapshot_at(&second, 1_150)
            .expect("capture second");
        assert!(captured_again >= 4);

        let counter = service.query(Some("Counter"), None, 10);
        assert_eq!(counter.len(), 2);
        assert_eq!(counter[0].value, HistorianValue::Integer(7));
        assert_eq!(counter[1].value, HistorianValue::Integer(8));

        let active = service.query(Some("Active"), None, 10);
        assert_eq!(active[0].value, HistorianValue::Bool(true));
        assert_eq!(active[1].value, HistorianValue::Bool(false));

        let label = service.query(Some("Label"), None, 10);
        assert_eq!(label[0].value, HistorianValue::String("Pump-A".to_string()));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persistent_backend_reloads_across_service_restart() {
        let path = temp_path("durability");
        {
            let service = HistorianService::new(basic_config(path.clone()), None).expect("service");
            let snapshot = snapshot_with_values(42, 10.0, true);
            service
                .capture_snapshot_at(&snapshot, 2_000)
                .expect("capture");
        }
        let restarted = HistorianService::new(basic_config(path.clone()), None).expect("restart");
        let counter = restarted.query(Some("Counter"), None, 10);
        assert_eq!(counter.len(), 1);
        assert_eq!(counter[0].value, HistorianValue::Integer(42));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn alert_threshold_debounce_and_file_hook_contract() {
        let history_path = temp_path("alerts-history");
        let hook_path = temp_path("alerts-hook");
        let mut config = basic_config(history_path.clone());
        config.sample_interval_ms = 1;
        config.alerts = vec![AlertRule {
            name: SmolStr::new("high_temp"),
            variable: SmolStr::new("Temp"),
            above: Some(50.0),
            below: None,
            debounce_samples: 2,
            hook: Some(SmolStr::new(hook_path.to_string_lossy())),
        }];

        let service = HistorianService::new(config, None).expect("service");

        service
            .capture_snapshot_at(&snapshot_with_values(1, 40.0, true), 1_000)
            .expect("below threshold");
        service
            .capture_snapshot_at(&snapshot_with_values(1, 60.0, true), 1_010)
            .expect("first breach");
        service
            .capture_snapshot_at(&snapshot_with_values(1, 61.0, true), 1_020)
            .expect("second breach");
        service
            .capture_snapshot_at(&snapshot_with_values(1, 45.0, true), 1_030)
            .expect("clear");

        let events = service.alerts(10);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].state, AlertState::Triggered);
        assert_eq!(events[1].state, AlertState::Cleared);

        let hook_lines = std::fs::read_to_string(&hook_path).expect("hook file");
        let hook_count = hook_lines.lines().count();
        assert_eq!(hook_count, 2);

        let _ = std::fs::remove_file(history_path);
        let _ = std::fs::remove_file(hook_path);
    }

    #[test]
    fn allowlist_mode_records_matching_paths_only() {
        let path = temp_path("allowlist");
        let mut config = basic_config(path.clone());
        config.mode = RecordingMode::Allowlist;
        config.include = vec![SmolStr::new("Temp"), SmolStr::new("retain.*")];
        let service = HistorianService::new(config, None).expect("service");

        let mut storage = VariableStorage::default();
        storage.set_global("Counter", Value::Int(9));
        storage.set_global("Temp", Value::LReal(5.0));
        storage.set_retain("Persist", Value::Bool(true));
        let snapshot = DebugSnapshot {
            storage,
            now: PlcDuration::from_millis(500),
        };
        service
            .capture_snapshot_at(&snapshot, 3_000)
            .expect("capture");

        assert_eq!(service.query(Some("Counter"), None, 10).len(), 0);
        assert_eq!(service.query(Some("Temp"), None, 10).len(), 1);
        assert_eq!(service.query(Some("retain.Persist"), None, 10).len(), 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn prometheus_render_includes_runtime_and_historian_metrics() {
        let runtime = RuntimeMetricsSnapshot {
            uptime_ms: 1200,
            faults: 1,
            overruns: 2,
            ..RuntimeMetricsSnapshot::default()
        };
        let body = render_prometheus(
            &runtime,
            Some(HistorianPrometheusSnapshot {
                samples_total: 10,
                series_total: 3,
                alerts_total: 4,
            }),
        );
        assert!(body.contains("trust_runtime_uptime_ms 1200"));
        assert!(body.contains("trust_runtime_faults_total 1"));
        assert!(body.contains("trust_runtime_historian_samples_total 10"));
        assert!(body.contains("trust_runtime_historian_alerts_total 4"));
    }
}
