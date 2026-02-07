//! Setup command handler.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use smol_str::SmolStr;

use crate::prompt;
use crate::style;
use crate::wizard;

mod setup_web;

pub fn run_setup(
    driver: Option<String>,
    backend: Option<String>,
    path: Option<PathBuf>,
    force: bool,
) -> anyhow::Result<()> {
    if driver.is_some() || backend.is_some() || path.is_some() || force {
        return run_system_setup(driver, backend, path, force);
    }
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(
            "setup requires an interactive terminal. Tip: run `trust-runtime setup` in a terminal, or use `sudo trust-runtime setup --force` for system I/O."
        );
    }
    println!(
        "{}",
        style::accent("Welcome to trueST! Let’s set up your first PLC project.")
    );
    println!("Setup options:");
    println!("  1) Open browser setup");
    println!("  2) Start CLI setup");
    println!("  3) Cancel setup");
    let choice = prompt::prompt_string("Select option", "1")?;
    match choice.trim() {
        "1" => run_browser_setup(),
        "2" => run_cli_setup(),
        "3" => {
            println!(
                "{}",
                style::warning("Setup cancelled. Resume any time with: trust-runtime setup")
            );
            Ok(())
        }
        _ => anyhow::bail!(
            "Invalid option. Expected 1, 2, or 3. Tip: run trust-runtime setup again."
        ),
    }
}

pub fn run_setup_default() -> anyhow::Result<()> {
    crate::style::print_logo();
    println!(
        "{}",
        style::accent("Welcome to trueST! Let’s create your first PLC project.")
    );
    println!("If you are on another device, run: trust-runtime setup");
    run_browser_setup_auto()
}

fn run_system_setup(
    driver: Option<String>,
    backend: Option<String>,
    path: Option<PathBuf>,
    force: bool,
) -> anyhow::Result<()> {
    let options = trust_runtime::setup::SetupOptions {
        driver: driver.map(SmolStr::new),
        backend: backend.map(SmolStr::new),
        force,
        path,
    };
    let path = trust_runtime::setup::run_setup(options)?;
    println!(
        "{}",
        style::success(format!("System I/O config written to {}", path.display()))
    );
    Ok(())
}

fn run_browser_setup() -> anyhow::Result<()> {
    println!("Where will you open the browser?");
    println!("  1) On this device (local GUI)");
    println!("  2) From another device (headless/SSH)");
    let choice = prompt::prompt_string("Select option", "2")?;
    let remote = matches!(choice.trim(), "2");
    let token_required = remote;
    let token_ttl = if remote {
        println!("Token expiry:");
        println!("  1) 15 min (default)");
        println!("  2) 30 min");
        println!("  3) 60 min");
        println!("  4) Custom");
        match prompt::prompt_string("Select option", "1")?.as_str() {
            "2" => 30,
            "3" => 60,
            "4" => prompt::prompt_u64("Minutes", 15)? as u64,
            _ => 15,
        }
    } else {
        0
    };
    let advanced = prompt::prompt_yes_no("Advanced settings?", false)?;
    let default_bind = if remote { "0.0.0.0" } else { "127.0.0.1" };
    let default_port = 8080u16;
    let bind = if advanced {
        prompt::prompt_string("Bind address", default_bind)?
    } else {
        default_bind.to_string()
    };
    let port = if advanced {
        prompt::prompt_u64("Port", default_port.into())? as u16
    } else {
        default_port
    };
    println!("Project folder: where runtime.toml, io.toml, sources/, program.stbc live.");
    let default_bundle = default_bundle_path();
    let bundle_path = prompt::prompt_path("Project folder (runtime files)", &default_bundle)?;
    let defaults = SetupDefaults::from_bundle(&bundle_path);
    setup_web::run_setup_web(setup_web::SetupWebOptions {
        bundle_root: bundle_path,
        bind,
        port,
        token_required,
        token_ttl_minutes: token_ttl,
        defaults,
    })
}

fn run_browser_setup_auto() -> anyhow::Result<()> {
    let default_bind = "127.0.0.1".to_string();
    let port = 8080u16;
    println!("Project folder: where runtime.toml, io.toml, sources/, program.stbc live.");
    let default_bundle = default_bundle_path();
    let bundle_path = prompt::prompt_path("Project folder (runtime files)", &default_bundle)?;
    let defaults = SetupDefaults::from_bundle(&bundle_path);
    setup_web::run_setup_web(setup_web::SetupWebOptions {
        bundle_root: bundle_path,
        bind: default_bind,
        port,
        token_required: false,
        token_ttl_minutes: 0,
        defaults,
    })
}

fn run_cli_setup() -> anyhow::Result<()> {
    println!("Setup mode:");
    println!("  1) Guided setup (recommended)");
    println!("  2) Manual setup");
    let choice = prompt::prompt_string("Select option", "1")?;
    match choice.trim() {
        "1" => run_cli_guided(),
        "2" => run_cli_manual(),
        _ => anyhow::bail!("Invalid option. Expected 1 or 2. Tip: run trust-runtime setup again."),
    }
}

fn run_cli_guided() -> anyhow::Result<()> {
    println!("Project folder: where runtime.toml, io.toml, sources/, program.stbc live.");
    let default_bundle = default_bundle_path();
    let bundle_path = prompt::prompt_path("Project folder (runtime files)", &default_bundle)?;
    let defaults = SetupDefaults::from_bundle(&bundle_path);
    wizard::create_bundle_auto(Some(bundle_path.clone()))?;
    println!(
        "{}",
        style::success(format!(
            "Project folder ready at: {}",
            bundle_path.display()
        ))
    );
    let resource_name: String = prompt::prompt_string("PLC name", defaults.resource_name.as_str())?;
    let cycle_ms = prompt::prompt_u64("Cycle time (ms)", defaults.cycle_ms)?;
    let write_system_io =
        prompt::prompt_yes_no("Write system-wide I/O config for this device?", true)?;
    if write_system_io {
        let overwrite = prompt::prompt_yes_no("Overwrite existing system-wide I/O config?", false)?;
        let options = trust_runtime::setup::SetupOptions {
            driver: Some(SmolStr::new(defaults.driver.clone())),
            backend: None,
            force: overwrite,
            path: None,
        };
        trust_runtime::setup::run_setup(options)?;
    }
    let use_system_io =
        prompt::prompt_yes_no("Use system-wide I/O config for this project?", true)?;
    let io_path = bundle_path.join("io.toml");
    if use_system_io {
        wizard::remove_io_toml(&io_path)?;
    } else {
        println!(
            "Choose gpio for Raspberry Pi, loopback/simulated for local runs, modbus-tcp for devices, or mqtt for brokered exchange."
        );
        let driver = prompt::prompt_string(
            "I/O driver (gpio, loopback, simulated, modbus-tcp, mqtt)",
            &defaults.driver,
        )?;
        wizard::write_io_toml_with_driver(&io_path, driver.trim())?;
    }
    let runtime_path = bundle_path.join("runtime.toml");
    wizard::write_runtime_toml(&runtime_path, &SmolStr::new(resource_name), cycle_ms)?;
    println!("{}", style::success("✓ Setup complete!"));
    println!("Start the PLC with:");
    println!("  trust-runtime --project {}", bundle_path.display());
    println!("Open http://localhost:8080 to monitor.");
    Ok(())
}

fn run_cli_manual() -> anyhow::Result<()> {
    println!("Manual setup:");
    println!("1) Create a project folder with:");
    println!("   - runtime.toml");
    println!("   - program.stbc");
    println!("   - io.toml (optional if using system IO)");
    println!("2) Start runtime with:");
    println!("   trust-runtime --project <project-folder>");
    println!("3) System IO config (optional):");
    println!("   sudo trust-runtime setup");
    println!("Need help later? Run: trust-runtime setup");
    Ok(())
}

fn default_bundle_path() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("project")
}

#[derive(Debug, Clone)]
pub(crate) struct SetupDefaults {
    pub resource_name: SmolStr,
    pub cycle_ms: u64,
    pub driver: String,
}

impl SetupDefaults {
    pub fn from_bundle(root: &Path) -> Self {
        let resource_name = wizard::default_resource_name(root);
        let cycle_ms = 100;
        let driver = if trust_runtime::setup::is_raspberry_pi_hint() {
            "gpio"
        } else {
            "loopback"
        };
        Self {
            resource_name,
            cycle_ms,
            driver: driver.to_string(),
        }
    }
}
