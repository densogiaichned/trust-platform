//! Runtime settings snapshot and updates.

#![allow(missing_docs)]

use indexmap::IndexMap;
use smol_str::SmolStr;

use crate::value::Duration;
use crate::watchdog::{FaultPolicy, RetainMode, WatchdogPolicy};

#[derive(Debug, Clone)]
pub struct RuntimeSettings {
    pub log_level: SmolStr,
    pub watchdog: WatchdogPolicy,
    pub fault_policy: FaultPolicy,
    pub retain_mode: RetainMode,
    pub retain_save_interval: Option<Duration>,
    pub web: WebSettings,
    pub discovery: DiscoverySettings,
    pub mesh: MeshSettings,
    pub opcua: OpcUaSettings,
    pub simulation: SimulationSettings,
}

impl RuntimeSettings {
    pub fn new(
        base: BaseSettings,
        web: WebSettings,
        discovery: DiscoverySettings,
        mesh: MeshSettings,
        simulation: SimulationSettings,
    ) -> Self {
        Self {
            log_level: base.log_level,
            watchdog: base.watchdog,
            fault_policy: base.fault_policy,
            retain_mode: base.retain_mode,
            retain_save_interval: base.retain_save_interval,
            web,
            discovery,
            mesh,
            opcua: OpcUaSettings::default(),
            simulation,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BaseSettings {
    pub log_level: SmolStr,
    pub watchdog: WatchdogPolicy,
    pub fault_policy: FaultPolicy,
    pub retain_mode: RetainMode,
    pub retain_save_interval: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct WebSettings {
    pub enabled: bool,
    pub listen: SmolStr,
    pub auth: SmolStr,
    pub tls: bool,
}

#[derive(Debug, Clone)]
pub struct DiscoverySettings {
    pub enabled: bool,
    pub service_name: SmolStr,
    pub advertise: bool,
    pub interfaces: Vec<SmolStr>,
}

#[derive(Debug, Clone)]
pub struct MeshSettings {
    pub enabled: bool,
    pub listen: SmolStr,
    pub tls: bool,
    pub auth_token: Option<SmolStr>,
    pub publish: Vec<SmolStr>,
    pub subscribe: IndexMap<SmolStr, SmolStr>,
}

#[derive(Debug, Clone)]
pub struct OpcUaSettings {
    pub enabled: bool,
    pub listen: SmolStr,
    pub endpoint_path: SmolStr,
    pub namespace_uri: SmolStr,
    pub publish_interval_ms: u64,
    pub max_nodes: usize,
    pub expose: Vec<SmolStr>,
    pub security_policy: SmolStr,
    pub security_mode: SmolStr,
    pub allow_anonymous: bool,
    pub username_set: bool,
}

impl Default for OpcUaSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: SmolStr::new("0.0.0.0:4840"),
            endpoint_path: SmolStr::new("/"),
            namespace_uri: SmolStr::new("urn:trust:runtime"),
            publish_interval_ms: 250,
            max_nodes: 128,
            expose: Vec::new(),
            security_policy: SmolStr::new("basic256sha256"),
            security_mode: SmolStr::new("sign_and_encrypt"),
            allow_anonymous: false,
            username_set: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SimulationSettings {
    pub enabled: bool,
    pub time_scale: u32,
    pub mode_label: SmolStr,
    pub warning: SmolStr,
}
