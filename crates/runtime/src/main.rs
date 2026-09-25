use std::{fs, path::PathBuf, time::Duration};

use anyhow::Result;
use clap::Parser;
use selium_abi::{Capability, CapabilityGrant, ResourceClass, ResourceSelector};
use selium_kernel::Kernel;
use selium_runtime::{ReadinessCondition, Runtime, RuntimeConfig, SystemGuestDescriptor};
use selium_service::FlatMsg;
use tokio::time::sleep;

#[derive(Debug, Clone)]
struct AppDef {
    name: String,
    path: PathBuf,
    entrypoint: String,
    module_id: String,
    dependencies: Vec<String>,
    tenant: Option<String>,
    readiness: String,
}

/// Selium runtime — loads and executes WebAssembly system guests.
#[derive(Parser)]
struct Cli {
    /// Start the discovery subsystem
    #[arg(long, default_value_t = false)]
    start_discovery: bool,

    /// Run the event-driven network wake demo: bootstraps the bundled
    /// net-demo guest (built for wasm32-unknown-unknown) and serves until
    /// Ctrl-C. Connect to the printed listener address and send a line —
    /// the guest progresses purely on kernel-thread wakes; this process
    /// contains no wake-delivery or reactor-pumping code.
    #[arg(long, default_value_t = false)]
    demo_net_wake: bool,

    /// One or more app definitions.
    ///
    /// Format: name=NAME,path=WASM[,entrypoint=FN][,module-id=ID][,dependencies=A,B][,tenant=T][,readiness=COND]
    ///
    /// Required keys: name, path.
    /// Defaults: entrypoint="main", module-id=<name>, readiness="immediate".
    #[arg(long, value_parser = parse_app)]
    app: Vec<AppDef>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let guests: Vec<SystemGuestDescriptor> = cli
        .app
        .iter()
        .map(|app| {
            let readiness = if app.readiness == "immediate" {
                ReadinessCondition::Immediate
            } else {
                ReadinessCondition::ActivityLogContains(app.readiness.clone())
            };

            Ok(SystemGuestDescriptor {
                name: app.name.clone(),
                module_id: app.module_id.clone(),
                module_bytes: fs::read(&app.path)?,
                entrypoint: app.entrypoint.clone(),
                arguments: Vec::new(),
                grants: Vec::new(),
                dependencies: app.dependencies.clone(),
                readiness,
                tenant: app.tenant.clone(),
                serving_role: None,
                handlers: Vec::new(),
            })
        })
        .collect::<Result<_>>()?;

    let kernel = Kernel::default();
    let runtime = Runtime::new(kernel);

    if cli.demo_net_wake {
        return run_demo_net_wake(runtime).await;
    }

    let config = RuntimeConfig {
        start_discovery: cli.start_discovery,
        system_guests: guests,
        domain_table: Vec::new(),
    };
    let report = runtime.bootstrap_system_guests(config)?;

    // Print activity log events
    let activity = runtime.activity_log();
    println!("=== Activity Log ===");
    for event in &activity {
        println!(
            "  [{:?}] {}: {}",
            event.kind,
            event.process_id.unwrap_or(0),
            event.message
        );
    }

    // Drain and print guest log messages for each bootstrapped guest
    for guest_report in &report.guests {
        println!("\n=== Guest Logs ({}) ===", guest_report.process_id);
        let frames = runtime
            .kernel()
            .processes()
            .drain_log_channel(guest_report.process_id)
            .map_err(|error| anyhow::anyhow!("drain log channel: {error}"))?;
        for frame in &frames {
            let record = selium_service::log::LogRecord::decode(frame)
                .map_err(|error| anyhow::anyhow!("decode log record: {error}"))?;
            println!("  {}", record.message);
        }
    }

    // Stop all processes and clean up
    println!("\n--- stopping processes ---");
    for guest_report in &report.guests {
        runtime.stop_process(guest_report.process_id)?;
    }
    if runtime.loaded_guest_count() != 0 {
        anyhow::bail!("expected 0 loaded guests after stop");
    }

    Ok(())
}

fn net_demo_descriptor(module_bytes: Vec<u8>) -> SystemGuestDescriptor {
    SystemGuestDescriptor {
        name: "net-demo".to_string(),
        module_id: "net-demo-module".to_string(),
        module_bytes,
        entrypoint: "net_demo".to_string(),
        arguments: Vec::new(),
        grants: vec![
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpListener)],
            ),
            CapabilityGrant::new(
                Capability::Network,
                vec![ResourceSelector::ResourceClass(ResourceClass::TcpStream)],
            ),
            CapabilityGrant::new(
                Capability::SharedMemory,
                vec![ResourceSelector::ResourceClass(ResourceClass::SharedRegion)],
            ),
            // Receiving from the bind-created host queue needs HostQueue.
            CapabilityGrant::new(
                Capability::HostQueue,
                vec![ResourceSelector::ResourceClass(ResourceClass::HostQueue)],
            ),
        ],
        dependencies: Vec::new(),
        readiness: ReadinessCondition::Immediate,
        tenant: None,
        serving_role: None,
        handlers: Vec::new(),
    }
}

/// Path to the compiled net-demo WASM module.
fn net_demo_wasm_path() -> PathBuf {
    let target_dir =
        std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_e| "../../target".to_string());
    PathBuf::from(target_dir).join("wasm32-unknown-unknown/debug/selium_net_demo.wasm")
}

fn parse_app(raw: &str) -> Result<AppDef, String> {
    // Stateful split: a comma-separated segment that contains '=' starts
    // a new key; segments without '=' are appended to the previous key's
    // value (e.g. dependencies=a,b).
    let mut kvs: Vec<(String, String)> = Vec::new();
    for part in raw.split(',') {
        if let Some(eq) = part.find('=') {
            let key = part.get(..eq).unwrap_or("").trim().to_owned();
            let val = part.get(eq + 1..).unwrap_or("").trim().to_owned();
            if key.is_empty() {
                return Err(format!("empty key in '{}'", raw));
            }
            kvs.push((key, val));
        } else if let Some((_, last_val)) = kvs.last_mut() {
            last_val.push(',');
            last_val.push_str(part);
        } else {
            return Err(format!(
                "expected key=value pair, got '{}' in '{}'",
                part, raw
            ));
        }
    }

    let mut name = None;
    let mut path = None;
    let mut entrypoint = "main".to_owned();
    let mut module_id = None;
    let mut dependencies = Vec::new();
    let mut tenant = None;
    let mut readiness = "immediate".to_owned();

    for (key, val) in &kvs {
        match key.as_str() {
            "name" => {
                if val.is_empty() {
                    return Err("name must not be empty".into());
                }
                name = Some(val.clone());
            }
            "path" => {
                if val.is_empty() {
                    return Err("path must not be empty".into());
                }
                path = Some(PathBuf::from(val));
            }
            "entrypoint" => entrypoint = val.clone(),
            "module-id" => module_id = Some(val.clone()),
            "dependencies" => {
                dependencies = val.split(',').map(|s| s.trim().to_owned()).collect();
            }
            "tenant" => {
                tenant = if val.is_empty() {
                    None
                } else {
                    Some(val.clone())
                }
            }
            "readiness" => readiness = val.clone(),
            unknown => return Err(format!("unknown key '{}'", unknown)),
        }
    }

    let name = name.ok_or_else(|| format!("missing required key 'name' in '{}'", raw))?;
    let path = path.ok_or_else(|| format!("missing required key 'path' in '{}'", raw))?;
    let module_id = module_id.unwrap_or_else(|| name.clone());

    Ok(AppDef {
        name,
        path,
        entrypoint,
        module_id,
        dependencies,
        tenant,
        readiness,
    })
}

/// Event-driven network wake demo (task 4.3): bootstraps the net-demo
/// guest, then serves until Ctrl-C. The service loop below performs NO
/// wake delivery and no reactor pumping — the guest's parked tasks are
/// driven entirely by kernel threads (poller accept, WaitRegister/mailbox
/// generation wakes, EOF bumps) under the runtime's execution guard. This
/// is the proof that the deferred-exec footgun is gone: delete every line
/// of this loop except the log printing and the guest still progresses.
async fn run_demo_net_wake(runtime: Runtime) -> Result<()> {
    let path = net_demo_wasm_path();
    let module_bytes = fs::read(&path).map_err(|error| {
        anyhow::anyhow!(
            "net demo guest not found at {} ({error}).\nBuild it first:\n  \
             cargo build --target wasm32-unknown-unknown -p selium-net-demo",
            path.display()
        )
    })?;

    let report = runtime
        .bootstrap_system_guests(RuntimeConfig {
            start_discovery: false,
            system_guests: vec![net_demo_descriptor(module_bytes)],
            domain_table: Vec::new(),
        })
        .map_err(|error| anyhow::anyhow!("bootstrap: {error}"))?;
    #[expect(
        clippy::indexing_slicing,
        reason = "exactly one guest was bootstrapped above"
    )]
    let process_id = report.guests[0].process_id;

    // Give the guest a moment to bind its listener before we print hints.
    sleep(Duration::from_millis(300)).await;
    let addrs = runtime.kernel().network().tcp_listener_addrs();
    println!("=== net-wake demo running — press Ctrl-C to stop ===");
    if let Some(addr) = addrs.first() {
        println!("connect and send a line, e.g.:  nc {addr} <<< 'hello'");
    } else {
        println!("(no guest listeners registered yet)");
    }

    // Service phase: host-side cosmetics only (log printing). Everything
    // that makes the guest progress happens on kernel threads.
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("--- ctrl-c received, stopping guests ---");
                break;
            }
            _ = sleep(Duration::from_millis(500)) => {
                let frames = runtime
                    .kernel()
                    .processes()
                    .drain_log_channel(process_id)
                    .map_err(|error| anyhow::anyhow!("drain log channel: {error}"))?;
                for frame in &frames {
                    let record = selium_service::log::LogRecord::decode(frame)
                        .map_err(|error| anyhow::anyhow!("decode log record: {error}"))?;
                    println!("  [guest] {}", record.message);
                }
            }
        }
    }

    runtime.stop_process(process_id)?;
    Ok(())
}
