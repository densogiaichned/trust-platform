//! EtherCAT I/O driver (EtherCAT backend v1).

#![allow(missing_docs)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant};

#[cfg(all(feature = "ethercat-wire", unix))]
use ethercrab::std::{ethercat_now, tx_rx_task};
#[cfg(all(feature = "ethercat-wire", unix))]
use ethercrab::{
    subdevice_group::Op, MainDevice, MainDeviceConfig, PduStorage, SubDeviceGroup, Timeouts,
};
use serde::Deserialize;
use smol_str::SmolStr;
#[cfg(feature = "ethercat-wire")]
use tokio::runtime::Runtime as TokioRuntime;

use crate::error::RuntimeError;
use crate::io::{IoDriver, IoDriverErrorPolicy, IoDriverHealth};

#[derive(Debug, Clone)]
pub struct EthercatConfig {
    pub adapter: SmolStr,
    pub timeout: StdDuration,
    pub cycle_warn: StdDuration,
    pub on_error: IoDriverErrorPolicy,
    pub modules: Vec<EthercatModuleConfig>,
    pub expected_input_bytes: usize,
    pub expected_output_bytes: usize,
    pub mock_inputs: Vec<Vec<u8>>,
    pub mock_latency: StdDuration,
    pub mock_fail_read: bool,
    pub mock_fail_write: bool,
}

#[derive(Debug, Clone)]
pub struct EthercatModuleConfig {
    pub model: SmolStr,
    pub slot: u16,
    pub channels: u16,
}

#[derive(Debug, Deserialize)]
struct EthercatToml {
    adapter: Option<String>,
    timeout_ms: Option<u64>,
    cycle_warn_ms: Option<u64>,
    on_error: Option<String>,
    modules: Option<Vec<EthercatModuleToml>>,
    mock_inputs: Option<Vec<String>>,
    mock_latency_ms: Option<u64>,
    mock_fail_read: Option<bool>,
    mock_fail_write: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct EthercatModuleToml {
    model: String,
    slot: Option<u16>,
    channels: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EthercatModuleKind {
    Coupler,
    DigitalInput,
    DigitalOutput,
}

#[derive(Debug, Clone)]
struct EthercatDiscovery {
    modules: Vec<EthercatModuleConfig>,
    input_bytes: usize,
    output_bytes: usize,
}

trait EthercatBus: Send {
    fn discover(&mut self, config: &EthercatConfig) -> Result<EthercatDiscovery, RuntimeError>;
    fn read_inputs(&mut self, bytes: usize) -> Result<Vec<u8>, RuntimeError>;
    fn write_outputs(&mut self, outputs: &[u8]) -> Result<(), RuntimeError>;
}

#[derive(Debug)]
struct MockEthercatBus {
    modules: Vec<EthercatModuleConfig>,
    input_frames: VecDeque<Vec<u8>>,
    latency: StdDuration,
    fail_read: bool,
    fail_write: bool,
    last_outputs: Vec<u8>,
}

impl MockEthercatBus {
    fn new(config: &EthercatConfig) -> Self {
        Self {
            modules: config.modules.clone(),
            input_frames: VecDeque::from(config.mock_inputs.clone()),
            latency: config.mock_latency,
            fail_read: config.mock_fail_read,
            fail_write: config.mock_fail_write,
            last_outputs: Vec::new(),
        }
    }
}

impl EthercatBus for MockEthercatBus {
    fn discover(&mut self, _config: &EthercatConfig) -> Result<EthercatDiscovery, RuntimeError> {
        let (input_bits, output_bits) =
            self.modules.iter().fold((0usize, 0usize), |acc, module| {
                let (input, output) = module_io_bits(module);
                (acc.0.saturating_add(input), acc.1.saturating_add(output))
            });
        Ok(EthercatDiscovery {
            modules: self.modules.clone(),
            input_bytes: input_bits.div_ceil(8),
            output_bytes: output_bits.div_ceil(8),
        })
    }

    fn read_inputs(&mut self, bytes: usize) -> Result<Vec<u8>, RuntimeError> {
        if self.latency > StdDuration::ZERO {
            std::thread::sleep(self.latency);
        }
        if self.fail_read {
            return Err(RuntimeError::IoDriver("mock ethercat read failure".into()));
        }
        let mut data = self.input_frames.pop_front().unwrap_or_default();
        if !data.is_empty() {
            self.input_frames.push_back(data.clone());
        }
        if data.len() < bytes {
            data.resize(bytes, 0);
        } else if data.len() > bytes {
            data.truncate(bytes);
        }
        Ok(data)
    }

    fn write_outputs(&mut self, outputs: &[u8]) -> Result<(), RuntimeError> {
        if self.latency > StdDuration::ZERO {
            std::thread::sleep(self.latency);
        }
        if self.fail_write {
            return Err(RuntimeError::IoDriver("mock ethercat write failure".into()));
        }
        self.last_outputs.clear();
        self.last_outputs.extend_from_slice(outputs);
        Ok(())
    }
}

#[cfg(all(feature = "ethercat-wire", unix))]
const ETHERCAT_MAX_SUBDEVICES: usize = 64;
#[cfg(all(feature = "ethercat-wire", unix))]
const ETHERCAT_MAX_PDI: usize = 4096;
#[cfg(all(feature = "ethercat-wire", unix))]
const ETHERCAT_MAX_FRAMES: usize = 32;
#[cfg(all(feature = "ethercat-wire", unix))]
const ETHERCAT_MAX_PDU_DATA: usize = PduStorage::element_size(ETHERCAT_MAX_PDI);

#[cfg(all(feature = "ethercat-wire", unix))]
type EthercrabGroup = SubDeviceGroup<ETHERCAT_MAX_SUBDEVICES, ETHERCAT_MAX_PDI, Op>;

#[cfg(all(feature = "ethercat-wire", unix))]
struct EthercrabBus {
    runtime: TokioRuntime,
    maindevice: Arc<MainDevice<'static>>,
    group: EthercrabGroup,
    transport_error: Arc<Mutex<Option<SmolStr>>>,
}

#[cfg(all(feature = "ethercat-wire", unix))]
impl EthercrabBus {
    fn new(config: &EthercatConfig) -> Result<Self, RuntimeError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|err| {
                RuntimeError::IoDriver(format!("ethercat tokio runtime init failed: {err}").into())
            })?;
        let storage = Box::leak(Box::new(PduStorage::<
            ETHERCAT_MAX_FRAMES,
            ETHERCAT_MAX_PDU_DATA,
        >::new()));
        let (tx, rx, pdu_loop) = storage
            .try_split()
            .map_err(|_| RuntimeError::IoDriver("ethercat PDU storage split failed".into()))?;

        let timeouts = Timeouts {
            pdu: config.timeout,
            state_transition: config.timeout.max(StdDuration::from_secs(1)),
            mailbox_response: config.timeout.max(StdDuration::from_millis(250)),
            ..Timeouts::default()
        };

        let maindevice = Arc::new(MainDevice::new(
            pdu_loop,
            timeouts,
            MainDeviceConfig::default(),
        ));
        let transport_error = Arc::new(Mutex::new(None));

        let tx_rx_future = tx_rx_task(config.adapter.as_str(), tx, rx).map_err(|err| {
            RuntimeError::IoDriver(
                format!("ethercat transport '{}' open failed: {err}", config.adapter).into(),
            )
        })?;
        let transport_error_ref = Arc::clone(&transport_error);
        runtime.spawn(async move {
            let message = match tx_rx_future.await {
                Ok(_) => SmolStr::new("ethercat transport loop exited"),
                Err(err) => SmolStr::new(format!("ethercat transport loop failed: {err}")),
            };
            let mut guard = transport_error_ref
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            *guard = Some(message);
        });

        let group = runtime
            .block_on(
                maindevice
                    .init_single_group::<ETHERCAT_MAX_SUBDEVICES, ETHERCAT_MAX_PDI>(ethercat_now),
            )
            .map_err(|err| {
                RuntimeError::IoDriver(
                    format!(
                        "ethercat discovery/init failed on '{}': {err}",
                        config.adapter
                    )
                    .into(),
                )
            })?;
        let group = runtime
            .block_on(group.into_op(maindevice.as_ref()))
            .map_err(|err| {
                RuntimeError::IoDriver(
                    format!(
                        "ethercat PRE-OP -> OP failed on '{}': {err}",
                        config.adapter
                    )
                    .into(),
                )
            })?;

        Ok(Self {
            runtime,
            maindevice,
            group,
            transport_error,
        })
    }

    fn check_transport_error(&self) -> Result<(), RuntimeError> {
        let guard = self
            .transport_error
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(message) = guard.as_ref() {
            return Err(RuntimeError::IoDriver(message.clone()));
        }
        Ok(())
    }

    fn tx_rx(&self) -> Result<(), RuntimeError> {
        self.check_transport_error()?;
        self.runtime
            .block_on(self.group.tx_rx(self.maindevice.as_ref()))
            .map(|_| ())
            .map_err(|err| {
                RuntimeError::IoDriver(format!("ethercat tx/rx failed: {err}").into())
            })?;
        self.check_transport_error()
    }

    fn collect_inputs(&self, bytes: usize) -> Vec<u8> {
        let mut data = Vec::with_capacity(bytes);
        for subdevice in self.group.iter(self.maindevice.as_ref()) {
            let io = subdevice.io_raw();
            data.extend_from_slice(io.inputs());
        }
        if data.len() < bytes {
            data.resize(bytes, 0);
        } else if data.len() > bytes {
            data.truncate(bytes);
        }
        data
    }

    fn write_outputs_to_pdi(&self, outputs: &[u8]) {
        let mut offset = 0usize;
        for subdevice in self.group.iter(self.maindevice.as_ref()) {
            let mut io = subdevice.io_raw_mut();
            let out = io.outputs();
            out.fill(0);
            if offset < outputs.len() {
                let copy_len = out.len().min(outputs.len() - offset);
                out[..copy_len].copy_from_slice(&outputs[offset..offset + copy_len]);
                offset += copy_len;
            }
        }
    }

    fn discovery_snapshot(&self) -> Result<EthercatDiscovery, RuntimeError> {
        let mut modules = Vec::new();
        let mut input_bytes = 0usize;
        let mut output_bytes = 0usize;
        for (slot, subdevice) in self.group.iter(self.maindevice.as_ref()).enumerate() {
            let io = subdevice.io_raw();
            input_bytes = input_bytes.saturating_add(io.inputs().len());
            output_bytes = output_bytes.saturating_add(io.outputs().len());
            let channels = (io.inputs().len().max(io.outputs().len()) * 8).max(1);
            modules.push(EthercatModuleConfig {
                model: SmolStr::new(subdevice.name()),
                slot: slot as u16,
                channels: channels.min(u16::MAX as usize) as u16,
            });
        }
        if modules.is_empty() {
            return Err(RuntimeError::IoDriver(
                "ethercat discovery found no subdevices".into(),
            ));
        }
        Ok(EthercatDiscovery {
            modules,
            input_bytes,
            output_bytes,
        })
    }
}

#[cfg(all(feature = "ethercat-wire", unix))]
impl EthercatBus for EthercrabBus {
    fn discover(&mut self, _config: &EthercatConfig) -> Result<EthercatDiscovery, RuntimeError> {
        self.tx_rx()?;
        self.discovery_snapshot()
    }

    fn read_inputs(&mut self, bytes: usize) -> Result<Vec<u8>, RuntimeError> {
        self.tx_rx()?;
        Ok(self.collect_inputs(bytes))
    }

    fn write_outputs(&mut self, outputs: &[u8]) -> Result<(), RuntimeError> {
        self.write_outputs_to_pdi(outputs);
        self.tx_rx()
    }
}

pub struct EthercatIoDriver {
    config: EthercatConfig,
    bus: Box<dyn EthercatBus>,
    health: IoDriverHealth,
    discovered: bool,
    discovery_message: SmolStr,
}

impl EthercatIoDriver {
    pub fn from_params(value: &toml::Value) -> Result<Self, RuntimeError> {
        let config = EthercatConfig::from_params(value)?;
        let bus = build_bus(&config)?;
        Ok(Self {
            config,
            bus,
            health: IoDriverHealth::Degraded {
                error: SmolStr::new("ethercat discovery pending"),
            },
            discovered: false,
            discovery_message: SmolStr::new("ethercat discovery pending"),
        })
    }

    pub fn validate_params(value: &toml::Value) -> Result<(), RuntimeError> {
        let _ = EthercatConfig::from_params(value)?;
        Ok(())
    }

    fn ensure_discovered(&mut self) -> Result<(), RuntimeError> {
        if self.discovered {
            return Ok(());
        }
        let discovery = self.bus.discover(&self.config)?;
        let module_summary = discovery
            .modules
            .iter()
            .map(|module| format!("{}@{}", module.model, module.slot))
            .collect::<Vec<_>>()
            .join(", ");
        self.discovery_message = SmolStr::new(format!(
            "ethercat discovered [{}] on adapter '{}' (I={}B O={}B)",
            module_summary, self.config.adapter, discovery.input_bytes, discovery.output_bytes
        ));
        self.discovered = true;
        if discovery.input_bytes != self.config.expected_input_bytes
            || discovery.output_bytes != self.config.expected_output_bytes
        {
            self.health = IoDriverHealth::Degraded {
                error: SmolStr::new(format!(
                    "{}; config expects I={}B O={}B",
                    self.discovery_message,
                    self.config.expected_input_bytes,
                    self.config.expected_output_bytes
                )),
            };
        } else {
            self.health = IoDriverHealth::Ok;
        }
        Ok(())
    }

    fn handle_io_error(&mut self, operation: &str, err: RuntimeError) -> Result<(), RuntimeError> {
        let message = SmolStr::new(format!("ethercat {operation}: {err}"));
        match self.config.on_error {
            IoDriverErrorPolicy::Fault => {
                self.health = IoDriverHealth::Faulted {
                    error: message.clone(),
                };
                Err(RuntimeError::IoDriver(message))
            }
            IoDriverErrorPolicy::Warn | IoDriverErrorPolicy::Ignore => {
                self.health = IoDriverHealth::Degraded {
                    error: message.clone(),
                };
                Ok(())
            }
        }
    }

    fn note_cycle_latency(&mut self, operation: &str, elapsed: StdDuration) {
        if elapsed > self.config.cycle_warn {
            self.health = IoDriverHealth::Degraded {
                error: SmolStr::new(format!(
                    "ethercat {operation} cycle {:.3}ms exceeded {:.3}ms",
                    elapsed.as_secs_f64() * 1000.0,
                    self.config.cycle_warn.as_secs_f64() * 1000.0
                )),
            };
        } else if self.discovered {
            self.health = IoDriverHealth::Ok;
        }
    }

    fn enforce_timing(
        &mut self,
        operation: &str,
        elapsed: StdDuration,
    ) -> Result<(), RuntimeError> {
        if elapsed > self.config.timeout {
            let err = RuntimeError::IoDriver(
                format!(
                    "ethercat {operation} timeout {:.3}ms exceeded {:.3}ms",
                    elapsed.as_secs_f64() * 1000.0,
                    self.config.timeout.as_secs_f64() * 1000.0
                )
                .into(),
            );
            return self.handle_io_error(operation, err);
        }
        self.note_cycle_latency(operation, elapsed);
        Ok(())
    }
}

impl IoDriver for EthercatIoDriver {
    fn read_inputs(&mut self, inputs: &mut [u8]) -> Result<(), RuntimeError> {
        if let Err(err) = self.ensure_discovered() {
            return self.handle_io_error("discover", err);
        }
        if inputs.len() < self.config.expected_input_bytes {
            let err = RuntimeError::IoDriver(
                format!(
                    "input image too small: got {}B, expected at least {}B",
                    inputs.len(),
                    self.config.expected_input_bytes
                )
                .into(),
            );
            return self.handle_io_error("read", err);
        }
        let start = Instant::now();
        match self.bus.read_inputs(inputs.len()) {
            Ok(data) => {
                let copy_len = inputs.len().min(data.len());
                inputs[..copy_len].copy_from_slice(&data[..copy_len]);
                self.enforce_timing("read", start.elapsed())
            }
            Err(err) => self.handle_io_error("read", err),
        }
    }

    fn write_outputs(&mut self, outputs: &[u8]) -> Result<(), RuntimeError> {
        if let Err(err) = self.ensure_discovered() {
            return self.handle_io_error("discover", err);
        }
        if outputs.len() < self.config.expected_output_bytes {
            let err = RuntimeError::IoDriver(
                format!(
                    "output image too small: got {}B, expected at least {}B",
                    outputs.len(),
                    self.config.expected_output_bytes
                )
                .into(),
            );
            return self.handle_io_error("write", err);
        }
        let start = Instant::now();
        match self.bus.write_outputs(outputs) {
            Ok(()) => self.enforce_timing("write", start.elapsed()),
            Err(err) => self.handle_io_error("write", err),
        }
    }

    fn health(&self) -> IoDriverHealth {
        if self.discovered {
            self.health.clone()
        } else {
            IoDriverHealth::Degraded {
                error: self.discovery_message.clone(),
            }
        }
    }
}

fn build_bus(config: &EthercatConfig) -> Result<Box<dyn EthercatBus>, RuntimeError> {
    if config.adapter.eq_ignore_ascii_case("mock") {
        return Ok(Box::new(MockEthercatBus::new(config)));
    }

    #[cfg(all(feature = "ethercat-wire", unix))]
    {
        Ok(Box::new(EthercrabBus::new(config)?))
    }

    #[cfg(all(feature = "ethercat-wire", not(unix)))]
    {
        let _ = config;
        Err(RuntimeError::InvalidConfig(
            "ethercat hardware transport is only supported on unix targets in this build".into(),
        ))
    }

    #[cfg(not(feature = "ethercat-wire"))]
    {
        let _ = config;
        Err(RuntimeError::InvalidConfig(
            "io.params.adapter requires feature 'ethercat-wire' for hardware transport".into(),
        ))
    }
}

impl EthercatConfig {
    pub fn from_params(value: &toml::Value) -> Result<Self, RuntimeError> {
        let parsed: EthercatToml = value
            .clone()
            .try_into()
            .map_err(|err| RuntimeError::InvalidConfig(format!("io.params: {err}").into()))?;

        let adapter = parsed
            .adapter
            .unwrap_or_else(|| "mock".to_string())
            .trim()
            .to_string();
        if adapter.is_empty() {
            return Err(RuntimeError::InvalidConfig(
                "io.params.adapter must not be empty".into(),
            ));
        }

        let timeout = StdDuration::from_millis(parsed.timeout_ms.unwrap_or(250).max(1));
        let cycle_warn = StdDuration::from_millis(parsed.cycle_warn_ms.unwrap_or(5).max(1));
        let on_error = parsed
            .on_error
            .as_deref()
            .map(IoDriverErrorPolicy::parse)
            .transpose()?
            .unwrap_or(IoDriverErrorPolicy::Fault);

        let modules = parse_modules(parsed.modules)?;
        let (expected_input_bytes, expected_output_bytes) = expected_image_sizes(&modules);
        let mock_inputs = parse_mock_inputs(parsed.mock_inputs)?;

        Ok(Self {
            adapter: SmolStr::new(adapter),
            timeout,
            cycle_warn,
            on_error,
            modules,
            expected_input_bytes,
            expected_output_bytes,
            mock_inputs,
            mock_latency: StdDuration::from_millis(parsed.mock_latency_ms.unwrap_or(0)),
            mock_fail_read: parsed.mock_fail_read.unwrap_or(false),
            mock_fail_write: parsed.mock_fail_write.unwrap_or(false),
        })
    }
}

fn parse_modules(
    modules: Option<Vec<EthercatModuleToml>>,
) -> Result<Vec<EthercatModuleConfig>, RuntimeError> {
    let modules = modules.unwrap_or_else(default_modules);
    if modules.is_empty() {
        return Err(RuntimeError::InvalidConfig(
            "io.params.modules must contain at least one module".into(),
        ));
    }
    let mut normalized = modules
        .into_iter()
        .enumerate()
        .map(|(idx, module)| {
            let model = module.model.trim().to_ascii_uppercase();
            if model.is_empty() {
                return Err(RuntimeError::InvalidConfig(
                    format!("io.params.modules[{idx}].model must not be empty").into(),
                ));
            }
            let kind = module_kind(&model).ok_or_else(|| {
                RuntimeError::InvalidConfig(
                    format!(
                        "io.params.modules[{idx}].model '{model}' is unsupported in ethercat v1"
                    )
                    .into(),
                )
            })?;
            let slot = module.slot.unwrap_or(idx as u16);
            let channels = module
                .channels
                .unwrap_or_else(|| default_channels_for_kind(kind))
                .max(1);
            if matches!(kind, EthercatModuleKind::Coupler) && channels != 1 {
                return Err(RuntimeError::InvalidConfig(
                    format!(
                        "io.params.modules[{idx}] coupler '{}' must use channels = 1",
                        model
                    )
                    .into(),
                ));
            }
            Ok(EthercatModuleConfig {
                model: SmolStr::new(model),
                slot,
                channels,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    normalized.sort_by_key(|module| module.slot);
    Ok(normalized)
}

fn expected_image_sizes(modules: &[EthercatModuleConfig]) -> (usize, usize) {
    let (input_bits, output_bits) = modules.iter().fold((0usize, 0usize), |acc, module| {
        let (input, output) = module_io_bits(module);
        (acc.0.saturating_add(input), acc.1.saturating_add(output))
    });
    (input_bits.div_ceil(8), output_bits.div_ceil(8))
}

fn module_io_bits(module: &EthercatModuleConfig) -> (usize, usize) {
    match module_kind(module.model.as_str()) {
        Some(EthercatModuleKind::Coupler) | None => (0, 0),
        Some(EthercatModuleKind::DigitalInput) => (module.channels as usize, 0),
        Some(EthercatModuleKind::DigitalOutput) => (0, module.channels as usize),
    }
}

fn parse_mock_inputs(inputs: Option<Vec<String>>) -> Result<Vec<Vec<u8>>, RuntimeError> {
    let Some(inputs) = inputs else {
        return Ok(Vec::new());
    };
    inputs
        .into_iter()
        .enumerate()
        .map(|(idx, text)| {
            parse_hex_bytes(&text).map_err(|err| {
                RuntimeError::InvalidConfig(
                    format!("io.params.mock_inputs[{idx}] invalid hex payload: {err}").into(),
                )
            })
        })
        .collect()
}

fn parse_hex_bytes(text: &str) -> Result<Vec<u8>, &'static str> {
    let compact = text
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>();
    if compact.is_empty() {
        return Ok(Vec::new());
    }
    if compact.len() % 2 != 0 {
        return Err("expected even number of hex characters");
    }
    let mut bytes = Vec::with_capacity(compact.len() / 2);
    for idx in (0..compact.len()).step_by(2) {
        let value =
            u8::from_str_radix(&compact[idx..idx + 2], 16).map_err(|_| "invalid hex digit")?;
        bytes.push(value);
    }
    Ok(bytes)
}

fn module_kind(model: &str) -> Option<EthercatModuleKind> {
    if model.eq_ignore_ascii_case("EK1100") {
        return Some(EthercatModuleKind::Coupler);
    }
    if model.starts_with("EL1") {
        return Some(EthercatModuleKind::DigitalInput);
    }
    if model.starts_with("EL2") {
        return Some(EthercatModuleKind::DigitalOutput);
    }
    None
}

fn default_channels_for_kind(kind: EthercatModuleKind) -> u16 {
    match kind {
        EthercatModuleKind::Coupler => 1,
        EthercatModuleKind::DigitalInput | EthercatModuleKind::DigitalOutput => 8,
    }
}

fn default_modules() -> Vec<EthercatModuleToml> {
    vec![
        EthercatModuleToml {
            model: "EK1100".to_string(),
            slot: Some(0),
            channels: Some(1),
        },
        EthercatModuleToml {
            model: "EL1008".to_string(),
            slot: Some(1),
            channels: Some(8),
        },
        EthercatModuleToml {
            model: "EL2008".to_string(),
            slot: Some(2),
            channels: Some(8),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ethercat_config_defaults_cover_ek1100_elx008() {
        let config = EthercatConfig::from_params(&toml::Value::Table(toml::map::Map::new()))
            .expect("default config");
        assert_eq!(config.adapter.as_str(), "mock");
        assert_eq!(config.expected_input_bytes, 1);
        assert_eq!(config.expected_output_bytes, 1);
        assert!(config
            .modules
            .iter()
            .any(|module| module.model.as_str() == "EK1100"));
    }

    #[test]
    fn ethercat_config_accepts_hardware_adapter_name() {
        let params: toml::Value = toml::from_str("adapter = 'eth0'").expect("parse params");
        let config = EthercatConfig::from_params(&params).expect("hardware adapter should parse");
        assert_eq!(config.adapter.as_str(), "eth0");
    }

    #[test]
    fn ethercat_driver_mock_reads_and_writes_images() {
        let params: toml::Value = toml::from_str(
            r#"
adapter = "mock"
mock_inputs = ["01", "00"]
[[modules]]
model = "EK1100"
slot = 0
[[modules]]
model = "EL1008"
slot = 1
channels = 8
[[modules]]
model = "EL2008"
slot = 2
channels = 8
"#,
        )
        .expect("parse params");
        let mut driver = EthercatIoDriver::from_params(&params).expect("driver");
        let mut inputs = [0u8; 1];
        driver.read_inputs(&mut inputs).expect("read");
        assert_eq!(inputs, [0x01]);
        driver.write_outputs(&[0xAA]).expect("write");
        assert!(matches!(driver.health(), IoDriverHealth::Ok));
    }

    #[test]
    fn ethercat_driver_fault_policy_propagates_driver_failure() {
        let params: toml::Value = toml::from_str(
            r#"
adapter = "mock"
mock_fail_read = true
on_error = "fault"
[[modules]]
model = "EK1100"
slot = 0
[[modules]]
model = "EL1008"
slot = 1
[[modules]]
model = "EL2008"
slot = 2
"#,
        )
        .expect("parse params");
        let mut driver = EthercatIoDriver::from_params(&params).expect("driver");
        let mut inputs = [0u8; 1];
        let err = driver
            .read_inputs(&mut inputs)
            .expect_err("fault policy should fail cycle");
        assert!(err.to_string().contains("ethercat read"));
        assert!(matches!(driver.health(), IoDriverHealth::Faulted { .. }));
    }

    #[test]
    fn ethercat_driver_warn_policy_degrades_without_failing() {
        let params: toml::Value = toml::from_str(
            r#"
adapter = "mock"
mock_fail_write = true
on_error = "warn"
[[modules]]
model = "EK1100"
slot = 0
[[modules]]
model = "EL1008"
slot = 1
[[modules]]
model = "EL2008"
slot = 2
"#,
        )
        .expect("parse params");
        let mut driver = EthercatIoDriver::from_params(&params).expect("driver");
        let mut inputs = [0u8; 1];
        driver.read_inputs(&mut inputs).expect("read");
        driver
            .write_outputs(&[0x01])
            .expect("warn policy should keep cycle running");
        assert!(matches!(driver.health(), IoDriverHealth::Degraded { .. }));
    }
}
