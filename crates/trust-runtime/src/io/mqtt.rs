//! MQTT I/O driver (protocol ecosystem expansion baseline).

#![allow(missing_docs)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant};

use rumqttc::{Client, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use smol_str::SmolStr;

use crate::error::RuntimeError;
use crate::io::{IoDriver, IoDriverHealth};

#[derive(Debug, Clone)]
struct BrokerEndpoint {
    host: SmolStr,
    port: u16,
}

#[derive(Debug, Clone)]
struct MqttIoConfig {
    endpoint: BrokerEndpoint,
    client_id: SmolStr,
    topic_in: SmolStr,
    topic_out: SmolStr,
    username: Option<SmolStr>,
    password: Option<SmolStr>,
    reconnect: StdDuration,
}

#[derive(Debug, Deserialize)]
struct MqttToml {
    broker: String,
    client_id: Option<String>,
    topic_in: Option<String>,
    topic_out: Option<String>,
    username: Option<String>,
    password: Option<String>,
    reconnect_ms: Option<u64>,
    keep_alive_s: Option<u64>,
    tls: Option<bool>,
    allow_insecure_remote: Option<bool>,
}

impl MqttIoConfig {
    fn from_params(value: &toml::Value) -> Result<Self, RuntimeError> {
        let params: MqttToml = value
            .clone()
            .try_into()
            .map_err(|err| RuntimeError::InvalidConfig(format!("io.params: {err}").into()))?;
        let endpoint = parse_broker_endpoint(&params.broker)?;
        let tls = params.tls.unwrap_or(false);
        if tls {
            return Err(RuntimeError::InvalidConfig(
                "mqtt tls=true is not yet supported (set tls=false for now)".into(),
            ));
        }
        let allow_insecure_remote = params.allow_insecure_remote.unwrap_or(false);
        if !allow_insecure_remote && !is_local_host(endpoint.host.as_str()) {
            return Err(RuntimeError::InvalidConfig(
                format!(
                    "mqtt insecure remote broker '{}' requires allow_insecure_remote=true",
                    endpoint.host
                )
                .into(),
            ));
        }
        let username = params.username.map(SmolStr::new);
        let password = params.password.map(SmolStr::new);
        if username.is_some() ^ password.is_some() {
            return Err(RuntimeError::InvalidConfig(
                "mqtt username/password must be set together".into(),
            ));
        }
        let client_id = params
            .client_id
            .map(SmolStr::new)
            .unwrap_or_else(|| SmolStr::new(format!("trust-runtime-{}", std::process::id())));
        let topic_in = params
            .topic_in
            .map(SmolStr::new)
            .unwrap_or_else(|| SmolStr::new("trust/io/in"));
        let topic_out = params
            .topic_out
            .map(SmolStr::new)
            .unwrap_or_else(|| SmolStr::new("trust/io/out"));
        let reconnect = StdDuration::from_millis(params.reconnect_ms.unwrap_or(500).max(1));
        let keep_alive_s = params.keep_alive_s.unwrap_or(5).max(1);
        if keep_alive_s > u16::MAX.into() {
            return Err(RuntimeError::InvalidConfig(
                "mqtt keep_alive_s must be <= 65535".into(),
            ));
        }

        Ok(Self {
            endpoint,
            client_id,
            topic_in,
            topic_out,
            username,
            password,
            reconnect,
        })
    }
}

trait MqttSession: Send {
    fn is_connected(&self) -> bool;
    fn take_payload(&mut self) -> Option<Vec<u8>>;
    fn publish(&mut self, topic: &str, payload: &[u8]) -> Result<(), RuntimeError>;
    fn last_error(&self) -> Option<SmolStr>;
}

trait MqttSessionFactory: Send + Sync {
    fn connect(&self, config: &MqttIoConfig) -> Result<Box<dyn MqttSession>, RuntimeError>;
}

#[derive(Debug, Default)]
struct RumqttSessionFactory;

struct RumqttSession {
    client: Client,
    incoming: Arc<Mutex<Option<Vec<u8>>>>,
    connected: Arc<AtomicBool>,
    last_error: Arc<Mutex<Option<SmolStr>>>,
    _worker: thread::JoinHandle<()>,
}

impl MqttSessionFactory for RumqttSessionFactory {
    fn connect(&self, config: &MqttIoConfig) -> Result<Box<dyn MqttSession>, RuntimeError> {
        let mut options = MqttOptions::new(
            config.client_id.as_str(),
            config.endpoint.host.as_str(),
            config.endpoint.port,
        );
        options.set_keep_alive(StdDuration::from_secs(5));
        if let (Some(username), Some(password)) = (&config.username, &config.password) {
            options.set_credentials(username.as_str(), password.as_str());
        }
        let (client, mut connection) = Client::new(options, 64);
        client
            .subscribe(config.topic_in.as_str(), QoS::AtMostOnce)
            .map_err(|err| RuntimeError::IoDriver(format!("mqtt subscribe: {err}").into()))?;

        let incoming = Arc::new(Mutex::new(None));
        let connected = Arc::new(AtomicBool::new(false));
        let last_error = Arc::new(Mutex::new(None));
        let incoming_ref = Arc::clone(&incoming);
        let connected_ref = Arc::clone(&connected);
        let last_error_ref = Arc::clone(&last_error);
        let topic_in = config.topic_in.clone();
        let worker = thread::spawn(move || {
            for event in connection.iter() {
                match event {
                    Ok(Event::Incoming(Packet::ConnAck(_)))
                    | Ok(Event::Incoming(Packet::SubAck(_)))
                    | Ok(Event::Outgoing(_)) => {
                        connected_ref.store(true, Ordering::SeqCst);
                    }
                    Ok(Event::Incoming(Packet::Publish(publish))) => {
                        connected_ref.store(true, Ordering::SeqCst);
                        if publish.topic == topic_in {
                            let mut guard = incoming_ref.lock().unwrap_or_else(|e| e.into_inner());
                            *guard = Some(publish.payload.to_vec());
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        connected_ref.store(false, Ordering::SeqCst);
                        let mut guard = last_error_ref.lock().unwrap_or_else(|e| e.into_inner());
                        *guard = Some(SmolStr::new(format!("mqtt event loop: {err}")));
                        break;
                    }
                }
            }
            connected_ref.store(false, Ordering::SeqCst);
            let mut guard = last_error_ref.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_none() {
                *guard = Some(SmolStr::new("mqtt connection closed"));
            }
        });

        Ok(Box::new(RumqttSession {
            client,
            incoming,
            connected,
            last_error,
            _worker: worker,
        }))
    }
}

impl MqttSession for RumqttSession {
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn take_payload(&mut self) -> Option<Vec<u8>> {
        let mut guard = self.incoming.lock().unwrap_or_else(|e| e.into_inner());
        guard.take()
    }

    fn publish(&mut self, topic: &str, payload: &[u8]) -> Result<(), RuntimeError> {
        self.client
            .publish(topic, QoS::AtMostOnce, false, payload.to_vec())
            .map_err(|err| RuntimeError::IoDriver(format!("mqtt publish: {err}").into()))
    }

    fn last_error(&self) -> Option<SmolStr> {
        let guard = self.last_error.lock().unwrap_or_else(|e| e.into_inner());
        guard.clone()
    }
}

pub struct MqttIoDriver {
    config: MqttIoConfig,
    factory: Arc<dyn MqttSessionFactory>,
    session: Option<Box<dyn MqttSession>>,
    health: IoDriverHealth,
    next_reconnect: Instant,
}

impl MqttIoDriver {
    pub fn from_params(value: &toml::Value) -> Result<Self, RuntimeError> {
        Self::from_params_with_factory(value, Arc::new(RumqttSessionFactory))
    }

    fn from_params_with_factory(
        value: &toml::Value,
        factory: Arc<dyn MqttSessionFactory>,
    ) -> Result<Self, RuntimeError> {
        let config = MqttIoConfig::from_params(value)?;
        Ok(Self {
            config,
            factory,
            session: None,
            health: IoDriverHealth::Degraded {
                error: SmolStr::new("mqtt initializing"),
            },
            next_reconnect: Instant::now(),
        })
    }

    pub fn validate_params(value: &toml::Value) -> Result<(), RuntimeError> {
        let _ = MqttIoConfig::from_params(value)?;
        Ok(())
    }

    fn set_degraded(&mut self, message: impl AsRef<str>) {
        self.health = IoDriverHealth::Degraded {
            error: SmolStr::new(message.as_ref()),
        };
    }

    fn ensure_session(&mut self) {
        let now = Instant::now();
        if let Some(session) = self.session.as_mut() {
            if session.is_connected() {
                self.health = IoDriverHealth::Ok;
                return;
            }
            if let Some(error) = session.last_error() {
                self.set_degraded(format!("mqtt disconnected: {error}"));
                if now < self.next_reconnect {
                    return;
                }
                self.session = None;
            } else {
                self.set_degraded("mqtt connecting");
                return;
            }
        }

        if now < self.next_reconnect {
            return;
        }
        match self.factory.connect(&self.config) {
            Ok(session) => {
                self.session = Some(session);
                self.set_degraded("mqtt connecting");
            }
            Err(err) => {
                self.session = None;
                self.set_degraded(format!("mqtt connect failed: {err}"));
                self.next_reconnect = now + self.config.reconnect;
            }
        }
    }
}

impl IoDriver for MqttIoDriver {
    fn read_inputs(&mut self, inputs: &mut [u8]) -> Result<(), RuntimeError> {
        self.ensure_session();
        if let Some(session) = self.session.as_mut() {
            if let Some(payload) = session.take_payload() {
                inputs.fill(0);
                for (dst, src) in inputs.iter_mut().zip(payload.iter()) {
                    *dst = *src;
                }
            }
            if session.is_connected() {
                self.health = IoDriverHealth::Ok;
            }
        }
        Ok(())
    }

    fn write_outputs(&mut self, outputs: &[u8]) -> Result<(), RuntimeError> {
        self.ensure_session();
        if let Some(session) = self.session.as_mut() {
            if let Err(err) = session.publish(self.config.topic_out.as_str(), outputs) {
                self.set_degraded(err.to_string());
                self.session = None;
                self.next_reconnect = Instant::now() + self.config.reconnect;
            } else if session.is_connected() {
                self.health = IoDriverHealth::Ok;
            }
        }
        Ok(())
    }

    fn health(&self) -> IoDriverHealth {
        self.health.clone()
    }
}

fn parse_broker_endpoint(text: &str) -> Result<BrokerEndpoint, RuntimeError> {
    let trimmed = text.trim();
    let stripped = trimmed
        .strip_prefix("tcp://")
        .or_else(|| trimmed.strip_prefix("mqtt://"))
        .unwrap_or(trimmed);
    if let Some(rest) = stripped.strip_prefix('[') {
        let (host, port) = rest.split_once("]:").ok_or_else(|| {
            RuntimeError::InvalidConfig(
                format!("io.params.broker '{text}' must be host:port").into(),
            )
        })?;
        return Ok(BrokerEndpoint {
            host: SmolStr::new(host),
            port: parse_port(port, text)?,
        });
    }
    let (host, port) = stripped.rsplit_once(':').ok_or_else(|| {
        RuntimeError::InvalidConfig(format!("io.params.broker '{text}' must be host:port").into())
    })?;
    if host.trim().is_empty() {
        return Err(RuntimeError::InvalidConfig(
            format!("io.params.broker '{text}' has empty host").into(),
        ));
    }
    Ok(BrokerEndpoint {
        host: SmolStr::new(host.trim()),
        port: parse_port(port, text)?,
    })
}

fn parse_port(port: &str, full: &str) -> Result<u16, RuntimeError> {
    let port = port.trim().parse::<u16>().map_err(|err| {
        RuntimeError::InvalidConfig(
            format!("io.params.broker '{full}': invalid port: {err}").into(),
        )
    })?;
    if port == 0 {
        return Err(RuntimeError::InvalidConfig(
            format!("io.params.broker '{full}': port must be > 0").into(),
        ));
    }
    Ok(port)
}

fn is_local_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct MockState {
        connected: bool,
        last_error: Option<SmolStr>,
        payloads: VecDeque<Vec<u8>>,
        published: Vec<Vec<u8>>,
        fail_publish_once: bool,
    }

    struct MockSession {
        state: Arc<Mutex<MockState>>,
    }

    impl MqttSession for MockSession {
        fn is_connected(&self) -> bool {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.connected
        }

        fn take_payload(&mut self) -> Option<Vec<u8>> {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.payloads.pop_front()
        }

        fn publish(&mut self, _topic: &str, payload: &[u8]) -> Result<(), RuntimeError> {
            let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.fail_publish_once {
                guard.fail_publish_once = false;
                guard.last_error = Some(SmolStr::new("publish failed"));
                return Err(RuntimeError::IoDriver("publish failed".into()));
            }
            guard.published.push(payload.to_vec());
            Ok(())
        }

        fn last_error(&self) -> Option<SmolStr> {
            let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
            guard.last_error.clone()
        }
    }

    struct MockFactory {
        state: Arc<Mutex<MockState>>,
        attempts: Arc<AtomicUsize>,
        fail_first: bool,
        always_fail: bool,
    }

    impl MqttSessionFactory for MockFactory {
        fn connect(&self, _config: &MqttIoConfig) -> Result<Box<dyn MqttSession>, RuntimeError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.always_fail || (self.fail_first && attempt == 0) {
                return Err(RuntimeError::IoDriver("connect failed".into()));
            }
            Ok(Box::new(MockSession {
                state: Arc::clone(&self.state),
            }))
        }
    }

    fn params(text: &str) -> toml::Value {
        toml::from_str(text).expect("parse toml params")
    }

    #[test]
    fn contract_test_reads_and_writes_payloads() {
        let state = Arc::new(Mutex::new(MockState {
            connected: true,
            payloads: VecDeque::from([vec![1, 0, 1]]),
            ..MockState::default()
        }));
        let attempts = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(MockFactory {
            state: Arc::clone(&state),
            attempts: Arc::clone(&attempts),
            fail_first: false,
            always_fail: false,
        });

        let mut driver = MqttIoDriver::from_params_with_factory(
            &params(
                r#"
broker = "127.0.0.1:1883"
topic_in = "line/in"
topic_out = "line/out"
"#,
            ),
            factory,
        )
        .expect("construct mqtt driver");

        let mut inputs = [0u8; 4];
        driver.read_inputs(&mut inputs).expect("read inputs");
        assert_eq!(&inputs[..3], &[1, 0, 1]);
        driver.write_outputs(&[9, 8, 7]).expect("write outputs");
        assert!(matches!(driver.health(), IoDriverHealth::Ok));

        let guard = state.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(guard.published, vec![vec![9, 8, 7]]);
    }

    #[test]
    fn reconnection_test_retries_after_connect_failure() {
        let state = Arc::new(Mutex::new(MockState {
            connected: true,
            ..MockState::default()
        }));
        let attempts = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(MockFactory {
            state,
            attempts: Arc::clone(&attempts),
            fail_first: true,
            always_fail: false,
        });

        let mut driver = MqttIoDriver::from_params_with_factory(
            &params(
                r#"
broker = "127.0.0.1:1883"
reconnect_ms = 1
"#,
            ),
            factory,
        )
        .expect("construct mqtt driver");

        let mut inputs = [0u8; 1];
        driver.read_inputs(&mut inputs).expect("first read");
        assert!(matches!(driver.health(), IoDriverHealth::Degraded { .. }));
        thread::sleep(StdDuration::from_millis(2));
        driver.read_inputs(&mut inputs).expect("second read");
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "expected at least two connect attempts"
        );
        assert!(matches!(driver.health(), IoDriverHealth::Ok));
    }

    #[test]
    fn security_test_rejects_remote_insecure_broker() {
        let result = MqttIoDriver::from_params(&params(
            r#"
broker = "10.10.0.9:1883"
"#,
        ));
        assert!(result.is_err(), "expected security validation failure");
        let error = match result {
            Ok(_) => panic!("expected insecure remote broker validation failure"),
            Err(err) => err.to_string(),
        };
        assert!(error.contains("allow_insecure_remote"));

        let ok = MqttIoDriver::from_params(&params(
            r#"
broker = "10.10.0.9:1883"
allow_insecure_remote = true
"#,
        ));
        assert!(ok.is_ok(), "explicit insecure override should be allowed");
    }

    #[test]
    fn cycle_impact_test_driver_calls_are_non_blocking_without_session() {
        let state = Arc::new(Mutex::new(MockState::default()));
        let attempts = Arc::new(AtomicUsize::new(0));
        let factory = Arc::new(MockFactory {
            state,
            attempts,
            fail_first: false,
            always_fail: true,
        });
        let mut driver = MqttIoDriver::from_params_with_factory(
            &params(
                r#"
broker = "127.0.0.1:1883"
reconnect_ms = 1
"#,
            ),
            factory,
        )
        .expect("construct mqtt driver");

        let started = Instant::now();
        let mut inputs = [0u8; 8];
        for _ in 0..400 {
            driver.read_inputs(&mut inputs).expect("read");
            driver.write_outputs(&[1, 2, 3, 4]).expect("write");
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < StdDuration::from_millis(250),
            "driver calls should stay non-blocking, elapsed={elapsed:?}"
        );
    }
}
