use anyhow::{bail, Context, Result};
use bridgeboard::config::{
    self, load_bridge_config, BridgeConfig, LifecycleConfig, ServiceConfig, ServiceMode,
    TunnelConfig, TunnelMode,
};
use bridgeboard::core::{self, BridgeEnv};
use bridgeboard::dashboard;
use bridgeboard::health;
use bridgeboard::paths::AppPaths;
use bridgeboard::peer;
use bridgeboard::process;
use bridgeboard::registry::{validate_no_port_conflicts, Registry, RegistryExport};
use bridgeboard::state::{ServiceState, State};
use bridgeboard::time;
use bridgeboard::tray;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "bridgeboard")]
#[command(about = "Low-resource portal/tool service and SSH tunnel manager")]
struct Cli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    registry: Option<PathBuf>,
    #[arg(long)]
    state: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Handoff(HandoffArgs),
    Register(RegisterArgs),
    List(ListArgs),
    Status(StatusArgs),
    Ports(PortsArgs),
    Up(IdArgs),
    RemoteUp(IdArgs),
    RemoteDown(IdArgs),
    RemoteRestart(IdArgs),
    Down(IdArgs),
    Stop(IdArgs),
    Restart(IdArgs),
    Rename(RenameArgs),
    Logs(LogsArgs),
    Open(IdArgs),
    Doctor,
    Serve(ServeArgs),
    Dashboard,
    Watch(WatchArgs),
    Tray(TrayArgs),
    Startup(StartupArgs),
    Supervise(SuperviseArgs),
    PortPlan,
    Registry {
        #[command(subcommand)]
        command: RegistryCommand,
    },
}

#[derive(Args)]
struct RegisterArgs {
    config_path: PathBuf,
    #[arg(long)]
    skip_peers: bool,
}

#[derive(Args)]
struct HandoffArgs {
    #[arg(long)]
    id: String,
    #[arg(long)]
    title: Option<String>,
    #[arg(long)]
    port: u16,
    #[arg(long)]
    owner_host: Option<String>,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long)]
    pid: Option<u32>,
    #[arg(long)]
    pid_from_port: bool,
    #[arg(long)]
    pid_port: Option<u16>,
    #[arg(long)]
    log_file: Option<PathBuf>,
    #[arg(long)]
    health_url: Option<String>,
    #[arg(long, default_value_t = 5)]
    health_timeout_sec: u64,
    #[arg(long)]
    require_healthy: bool,
    #[arg(long)]
    local_url: Option<String>,
    #[arg(long)]
    network_url: Option<String>,
    #[arg(long)]
    open_url: Option<String>,
    #[arg(long)]
    start_command: Option<String>,
    #[arg(long, value_enum)]
    detach: Option<DetachStrategy>,
    #[arg(long)]
    stop_command: Option<String>,
    #[arg(long)]
    restart_command: Option<String>,
    #[arg(long)]
    task_name: Option<String>,
    #[arg(long)]
    note: Option<String>,
    #[arg(long = "tunnel-mode")]
    tunnel_modes: Vec<String>,
    #[arg(long)]
    no_tunnel: bool,
    #[arg(long)]
    skip_peers: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum DetachStrategy {
    ScheduledTask,
}

impl DetachStrategy {
    fn as_config_value(self) -> &'static str {
        match self {
            DetachStrategy::ScheduledTask => "scheduled_task",
        }
    }
}

#[derive(Args)]
struct OutputArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct ListArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    peers: bool,
}

#[derive(Args)]
struct StatusArgs {
    id: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    peers: bool,
}

#[derive(Args)]
struct PortsArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    peers: bool,
}

#[derive(Args)]
struct IdArgs {
    id: String,
}

#[derive(Args)]
struct RenameArgs {
    id: String,
    #[arg(long)]
    title: String,
}

#[derive(Args)]
struct LogsArgs {
    id: String,
    #[arg(long, default_value_t = 80)]
    lines: usize,
}

#[derive(Args)]
struct ServeArgs {
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, default_value_t = 24000)]
    port: u16,
    #[arg(long)]
    no_peers: bool,
}

#[derive(Args)]
struct WatchArgs {
    #[arg(long, default_value_t = 5)]
    interval: u64,
}

#[derive(Args)]
struct TrayArgs {
    #[arg(long, default_value_t = 5)]
    interval: u64,
}

#[derive(Args)]
struct StartupArgs {
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args)]
struct SuperviseArgs {
    #[arg(long, default_value_t = 15)]
    interval: u64,
    #[arg(long)]
    once: bool,
}

#[derive(Subcommand)]
enum RegistryCommand {
    Export(OutputArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut paths = AppPaths::discover()?;
    if let Some(path) = cli.config {
        paths.config_file = path;
    }
    if let Some(path) = cli.registry {
        paths.registry_file = path;
    }
    if let Some(path) = cli.state {
        paths.state_file = path;
    }
    let env = BridgeEnv::from_paths(paths)?;
    dispatch(env, cli.command)
}

fn dispatch(env: BridgeEnv, command: Command) -> Result<()> {
    match command {
        Command::Handoff(args) => cmd_handoff(&env, args),
        Command::Register(args) => cmd_register(&env, args),
        Command::List(args) => cmd_list(&env, args),
        Command::Status(args) => cmd_status(&env, args),
        Command::Ports(args) => cmd_ports(&env, args),
        Command::Up(args) => print_lines(core::up(&env, &args.id)?),
        Command::RemoteUp(args) => print_lines(core::remote_up(&env, &args.id)?),
        Command::RemoteDown(args) => print_lines(core::remote_down(&env, &args.id)?),
        Command::RemoteRestart(args) => print_lines(core::remote_restart(&env, &args.id)?),
        Command::Down(args) => print_lines(core::down(&env, &args.id)?),
        Command::Stop(args) => print_lines(core::down(&env, &args.id)?),
        Command::Restart(args) => print_lines(core::restart(&env, &args.id)?),
        Command::Rename(args) => print_lines(core::rename_title(&env, &args.id, &args.title)?),
        Command::Logs(args) => cmd_logs(&env, args),
        Command::Open(args) => {
            println!("{}", core::open(&env, &args.id)?);
            Ok(())
        }
        Command::Doctor => cmd_doctor(&env),
        Command::Serve(args) => cmd_serve(&env, args),
        Command::Dashboard => cmd_dashboard(),
        Command::Watch(args) => cmd_watch(&env, args.interval),
        Command::Tray(args) => {
            tray::run_tray_loop(args.interval, || core::status_rows(&env, None, false))
        }
        Command::Startup(args) => print_lines(core::startup(&env, args.dry_run)?),
        Command::Supervise(args) => cmd_supervise(&env, args),
        Command::PortPlan => {
            print_port_plan();
            Ok(())
        }
        Command::Registry { command } => match command {
            RegistryCommand::Export(args) => cmd_registry_export(&env, args),
        },
    }
}

fn cmd_handoff(env: &BridgeEnv, args: HandoffArgs) -> Result<()> {
    if args.no_tunnel && !args.tunnel_modes.is_empty() {
        bail!("--no-tunnel cannot be combined with --tunnel-mode");
    }
    let modes = if args.no_tunnel {
        Vec::new()
    } else if args.tunnel_modes.is_empty() {
        env.app.defaults.handoff_tunnel_modes.clone()
    } else {
        parse_tunnel_modes(&args.tunnel_modes)?
    };
    let mut messages = Vec::new();
    let handoff_dir = env
        .paths
        .registry_file
        .parent()
        .map(|path| path.join("handoffs"))
        .unwrap_or_else(|| PathBuf::from("handoffs"));
    fs::create_dir_all(&handoff_dir)?;
    let safe_id = safe_file_stem(&args.id);
    let config_path = handoff_dir.join(format!("{safe_id}.yaml"));
    let owner_host = args
        .owner_host
        .clone()
        .unwrap_or_else(|| env.machine_id.clone());
    if args.require_healthy && args.health_url.is_none() {
        bail!("--require-healthy requires --health-url");
    }
    let detach = args.detach;
    let detach_label = detach.map(|strategy| strategy.as_config_value().to_string());
    let mut task_name = args.task_name.clone();
    let mut log_file = args.log_file.clone();
    if detach == Some(DetachStrategy::ScheduledTask) {
        if owner_host != env.machine_id {
            bail!("--detach scheduled-task can only start services owned by this machine");
        }
        if args.start_command.is_none() {
            bail!("--detach scheduled-task requires --start-command");
        }
        if task_name.is_none() {
            task_name = Some(format!("Bridgeboard-{safe_id}"));
        }
        if log_file.is_none() {
            log_file = Some(handoff_dir.join(format!("{safe_id}.log")));
        }
        let wrapper_path = handoff_dir.join(format!("{safe_id}.cmd"));
        let start_command = args.start_command.as_deref().unwrap();
        let task_name_ref = task_name.as_deref().unwrap();
        process::start_windows_scheduled_task(
            task_name_ref,
            &wrapper_path,
            args.cwd.as_deref(),
            start_command,
            log_file.as_deref(),
        )?;
        messages.push(format!(
            "scheduled task {task_name_ref} started via {}",
            wrapper_path.display()
        ));
    }
    let pid_lookup_port = if args.pid_from_port {
        Some(args.pid_port.unwrap_or(args.port))
    } else if detach == Some(DetachStrategy::ScheduledTask) {
        Some(args.pid_port.unwrap_or(args.port))
    } else {
        args.pid_port
    };
    let mut pid = args.pid;
    let mut pid_source = pid.map(|_| "manual".to_string());
    if pid.is_none() {
        if let Some(port) = pid_lookup_port {
            pid_source = Some(format!("port:{port}"));
            let found_pid = if detach == Some(DetachStrategy::ScheduledTask) {
                wait_pid_listening_on_port(port, args.health_timeout_sec)?
            } else {
                process::pid_listening_on_port(port)?
            };
            match found_pid {
                Some(found) => {
                    pid = Some(found);
                    messages.push(format!("pid resolved from port {port}: {found}"));
                }
                None => messages.push(format!("warning: no listening PID found on port {port}")),
            }
        }
    }
    let local_url = args
        .local_url
        .clone()
        .or_else(|| args.health_url.clone())
        .or_else(|| Some(format!("http://127.0.0.1:{}/", args.port)));
    let open_url = args
        .open_url
        .clone()
        .or_else(|| local_url.clone())
        .or_else(|| args.network_url.clone());
    let cfg = BridgeConfig {
        schema: "portal-bridge.v1".into(),
        id: args.id.clone(),
        title: args.title.clone().unwrap_or_else(|| args.id.clone()),
        owner_host,
        port: args.port,
        service: ServiceConfig {
            mode: ServiceMode::External,
            lifecycle: LifecycleConfig::default(),
            cwd: args.cwd.clone(),
            command: Vec::new(),
            start_command: args.start_command.clone(),
            detach: detach_label,
            stop_command: args.stop_command.clone(),
            restart_command: args.restart_command.clone(),
            task_name: task_name.clone(),
            pid_source: pid_source.clone(),
            pid_port: pid_lookup_port,
            pid_file: None,
            pid,
            log_file: log_file.clone(),
            health_url: args.health_url.clone(),
            startup_timeout_sec: args.health_timeout_sec,
            notes: args.note.clone(),
        },
        tunnel: TunnelConfig {
            modes,
            bind_host: "127.0.0.1".into(),
        },
        local_url,
        network_url: args.network_url.clone(),
        open_url,
    };
    config::validate_bridge_config(&cfg)?;

    let (last_health, last_status) = match cfg.service.health_url.as_ref() {
        Some(url) => match wait_health(url, args.health_timeout_sec) {
            Ok(status) => {
                messages.push(format!("health: {status}"));
                (Some(status), Some("handoff-healthy".to_string()))
            }
            Err(err) => {
                if args.require_healthy {
                    if let Some(task_name) = task_name.as_deref() {
                        let _ = process::end_windows_scheduled_task(task_name);
                        let _ = process::delete_windows_scheduled_task(task_name);
                    }
                    bail!("handoff health check failed for {url}: {err}");
                }
                let text = err.to_string();
                messages.push(format!("warning: health check failed: {text}"));
                (
                    Some(format!("failed: {text}")),
                    Some("handoff-unhealthy".to_string()),
                )
            }
        },
        None => (None, Some("handoff-recorded".to_string())),
    };
    fs::write(&config_path, serde_yaml::to_string(&cfg)?)?;

    let mut registry = Registry::load(&env.paths.registry_file)?;
    registry.register(config_path.clone())?;
    validate_registry_ports(env, &registry, !args.skip_peers)?;
    registry.save(&env.paths.registry_file)?;
    let mut state = State::load(&env.paths.state_file)?;
    state.services.insert(
        cfg.id.clone(),
        ServiceState {
            pid: cfg.service.pid,
            last_health,
            last_status,
            updated_at: Some(time::now_iso()),
            desired: None,
            pid_source: cfg.service.pid_source.clone(),
            pid_port: cfg.service.pid_port,
        },
    );
    state.save(&env.paths.state_file)?;
    println!(
        "handoff recorded {} on {}:{} ({})",
        cfg.id,
        cfg.owner_host,
        cfg.port,
        config_path.display()
    );
    print_lines(messages)?;
    Ok(())
}

fn cmd_register(env: &BridgeEnv, args: RegisterArgs) -> Result<()> {
    let config_path = args.config_path.canonicalize().unwrap_or(args.config_path);
    let cfg = load_bridge_config(&config_path)?;
    let mut registry = Registry::load(&env.paths.registry_file)?;
    registry.register(config_path)?;
    validate_registry_ports(env, &registry, !args.skip_peers)?;
    registry.save(&env.paths.registry_file)?;
    println!("registered {} on port {}", cfg.id, cfg.port);
    Ok(())
}

fn cmd_list(env: &BridgeEnv, args: ListArgs) -> Result<()> {
    let rows = core::status_rows(env, None, args.peers)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print!("{}", bridgeboard::status::render_table(&rows));
    }
    Ok(())
}

fn cmd_status(env: &BridgeEnv, args: StatusArgs) -> Result<()> {
    let rows = core::status_rows(env, args.id.as_deref(), args.peers)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print!("{}", bridgeboard::status::render_table(&rows));
    }
    Ok(())
}

fn cmd_ports(env: &BridgeEnv, args: PortsArgs) -> Result<()> {
    let rows = core::port_rows(env, args.peers)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print!("{}", core::render_port_table(&rows));
    }
    Ok(())
}

fn cmd_registry_export(env: &BridgeEnv, args: OutputArgs) -> Result<()> {
    let registry = Registry::load(&env.paths.registry_file)?;
    let export: RegistryExport = registry.export(&env.machine_id)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&export)?);
    } else {
        println!("{}:", export.machine_id);
        for service in export.services {
            println!(
                "  {} port={} owner={} mode={} lifecycle={} tunnel={}",
                service.id,
                service.port,
                service.owner_host,
                core::service_mode_label(service.service_mode),
                core::lifecycle_label(&service.lifecycle),
                core::tunnel_modes_label(&service.tunnel_modes)
            );
        }
    }
    Ok(())
}

fn cmd_logs(env: &BridgeEnv, args: LogsArgs) -> Result<()> {
    println!("{}", core::log_tail(env, &args.id, args.lines)?);
    Ok(())
}

fn cmd_doctor(env: &BridgeEnv) -> Result<()> {
    println!("machine_id: {}", env.machine_id);
    println!("config: {}", env.paths.config_file.display());
    println!("registry: {}", env.paths.registry_file.display());
    println!("state: {}", env.paths.state_file.display());
    let registry = Registry::load(&env.paths.registry_file)?;
    validate_registry_ports(env, &registry, true)?;
    println!("port conflicts: none");
    Ok(())
}

fn cmd_dashboard() -> Result<()> {
    let url = "http://127.0.0.1:24000/";
    webbrowser::open(url).with_context(|| format!("open {url}"))?;
    println!("{url}");
    Ok(())
}

fn cmd_serve(env: &BridgeEnv, args: ServeArgs) -> Result<()> {
    let addr = format!("{}:{}", args.host, args.port);
    println!("Bridgeboard dashboard: http://{addr}/");
    dashboard::serve(env.clone(), &args.host, args.port, !args.no_peers)
}

fn cmd_watch(env: &BridgeEnv, interval: u64) -> Result<()> {
    loop {
        let rows = core::status_rows(env, None, false)?;
        print!("\x1b[2J\x1b[H");
        print!("{}", bridgeboard::status::render_table(&rows));
        thread::sleep(Duration::from_secs(interval.max(1)));
    }
}

fn cmd_supervise(env: &BridgeEnv, args: SuperviseArgs) -> Result<()> {
    loop {
        let messages = core::supervise_once(env)?;
        if !messages.is_empty() {
            print_lines(messages)?;
        } else if args.once {
            println!("no supervised services needed restart");
        }
        if args.once {
            return Ok(());
        }
        thread::sleep(Duration::from_secs(args.interval.max(1)));
    }
}

fn validate_registry_ports(
    env: &BridgeEnv,
    registry: &Registry,
    include_peers: bool,
) -> Result<()> {
    let mut exports = vec![registry.export(&env.machine_id)?];
    if include_peers {
        let peer_results = peer::fetch_peer_exports(&env.app);
        peer::print_peer_warnings(&peer_results);
        exports.extend(
            peer_results
                .into_iter()
                .filter_map(|(_, result)| result.ok()),
        );
    }
    validate_no_port_conflicts(&exports)
}

fn wait_pid_listening_on_port(port: u16, timeout_sec: u64) -> Result<Option<u32>> {
    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    loop {
        if let Some(pid) = process::pid_listening_on_port(port)? {
            return Ok(Some(pid));
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_health(url: &str, timeout_sec: u64) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_sec.max(1));
    loop {
        match health::check_http(url, Duration::from_secs(1)) {
            Ok(status) => return Ok(status),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(err);
                }
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn parse_tunnel_modes(values: &[String]) -> Result<Vec<TunnelMode>> {
    let mut modes = Vec::new();
    for value in values {
        let mode = match value.as_str() {
            "local_forward" | "local" => TunnelMode::LocalForward,
            "reverse_forward" | "reverse" => TunnelMode::ReverseForward,
            other => {
                bail!("unsupported tunnel mode `{other}`; use local_forward or reverse_forward")
            }
        };
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    Ok(modes)
}

fn print_lines(lines: Vec<String>) -> Result<()> {
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn safe_file_stem(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn print_port_plan() {
    println!("Bridgeboard fixed port ranges:");
    println!("  24000        bridgeboard reserved");
    println!("  24001-24299  project-specific portals");
    println!("  24300-24499  project tools");
    println!("  24500-24699  agent/protocol/automation services");
    println!("  24700-24899  ad hoc temporary tunnels");
    println!("  24900-24999  diagnostics/manual override/emergency");
    println!();
    println!("Mirror rule: the same service id owns the same 24xxx port on every peer.");
    println!(
        "If gpu-box owns service x on 24001, workstation:24001 is reserved as its tunnel entry."
    );
}
