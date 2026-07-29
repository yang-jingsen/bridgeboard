use crate::config::{BridgeConfig, ServiceMode};
use crate::core::BridgeEnv;
use crate::registry::Registry;
use anyhow::{bail, Context, Result};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_CHUNKS: usize = 2_000;
const MAX_CHUNK_BYTES: usize = 8 * 1024;

#[derive(Clone, Default)]
pub struct TerminalManager {
    inner: Arc<Mutex<HashMap<String, Arc<TerminalSession>>>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalSessionInfo {
    pub id: String,
    pub title: String,
    pub service_id: Option<String>,
    pub command: String,
    pub cwd: Option<String>,
    pub started_at: String,
    pub status: String,
    pub exit: Option<String>,
    pub latest_seq: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalChunk {
    pub seq: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TerminalRead {
    pub session: TerminalSessionInfo,
    pub chunks: Vec<TerminalChunk>,
}

struct TerminalSession {
    id: String,
    title: String,
    service_id: Option<String>,
    command: String,
    cwd: Option<PathBuf>,
    started_at: String,
    state: Mutex<TerminalState>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
}

struct TerminalState {
    chunks: VecDeque<TerminalChunk>,
    next_seq: u64,
    status: TerminalStatus,
    exit: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalStatus {
    Running,
    Exited,
    ReaderClosed,
    Error,
    Killed,
}

impl TerminalManager {
    pub fn list(&self) -> Vec<TerminalSessionInfo> {
        let sessions = self.sessions();
        sessions.iter().map(|session| self.info(session)).collect()
    }

    pub fn start_shell(&self, cols: u16, rows: u16) -> Result<TerminalSessionInfo> {
        let spec = TerminalSpec::default_shell();
        self.start(spec, cols, rows)
    }

    pub fn start_service(
        &self,
        env: &BridgeEnv,
        service_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSessionInfo> {
        let registry = Registry::load(&env.paths.registry_file)?;
        let cfg = registry
            .try_get_config(service_id)?
            .with_context(|| format!("service `{service_id}` is not registered locally"))?;
        if cfg.owner_host != env.machine_id {
            bail!(
                "terminal sessions are local-only in this MVP; `{}` is owned by `{}`",
                cfg.id,
                cfg.owner_host
            );
        }
        let spec = TerminalSpec::from_service(&cfg)?;
        self.start(spec, cols, rows)
    }

    pub fn read(&self, session_id: &str, after: Option<u64>) -> Result<TerminalRead> {
        let session = self.get(session_id)?;
        let session_info = self.info(&session);
        let chunks = {
            let state = session.state.lock().unwrap_or_else(|err| err.into_inner());
            state
                .chunks
                .iter()
                .filter(|chunk| after.map(|seq| chunk.seq > seq).unwrap_or(true))
                .cloned()
                .collect()
        };
        Ok(TerminalRead {
            session: session_info,
            chunks,
        })
    }

    pub fn input(&self, session_id: &str, data: &str) -> Result<TerminalSessionInfo> {
        let session = self.get(session_id)?;
        if !self.is_running(&session) {
            bail!("terminal session `{session_id}` is not running");
        }
        {
            let mut writer = session.writer.lock().unwrap_or_else(|err| err.into_inner());
            writer
                .write_all(data.as_bytes())
                .with_context(|| format!("write terminal input to `{session_id}`"))?;
            writer.flush().ok();
        }
        Ok(self.info(&session))
    }

    pub fn resize(&self, session_id: &str, cols: u16, rows: u16) -> Result<TerminalSessionInfo> {
        let session = self.get(session_id)?;
        let size = pty_size(cols, rows);
        session
            .master
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .resize(size)
            .with_context(|| format!("resize terminal session `{session_id}`"))?;
        Ok(self.info(&session))
    }

    pub fn stop(&self, session_id: &str) -> Result<TerminalSessionInfo> {
        let session = self.get(session_id)?;
        {
            let mut child = session.child.lock().unwrap_or_else(|err| err.into_inner());
            child
                .kill()
                .with_context(|| format!("kill terminal session `{session_id}`"))?;
        }
        {
            let mut state = session.state.lock().unwrap_or_else(|err| err.into_inner());
            state.status = TerminalStatus::Killed;
            state.exit = Some("killed".into());
            push_chunk_locked(&mut state, "\r\n[bridgeboard] terminal killed\r\n");
        }
        Ok(self.info(&session))
    }

    fn start(&self, spec: TerminalSpec, cols: u16, rows: u16) -> Result<TerminalSessionInfo> {
        let size = pty_size(cols, rows);
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(size)?;
        let mut cmd = CommandBuilder::from_argv(spec.argv.clone());
        cmd.env("TERM", "xterm-256color");
        if let Some(cwd) = &spec.cwd {
            cmd.cwd(cwd.as_os_str());
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("spawn terminal `{}`", spec.command))?;
        let mut reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let session = Arc::new(TerminalSession {
            id: new_session_id(),
            title: spec.title,
            service_id: spec.service_id,
            command: spec.command,
            cwd: spec.cwd,
            started_at: crate::time::now_iso(),
            state: Mutex::new(TerminalState {
                chunks: VecDeque::new(),
                next_seq: 0,
                status: TerminalStatus::Running,
                exit: None,
            }),
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
        });
        {
            let mut state = session.state.lock().unwrap_or_else(|err| err.into_inner());
            push_chunk_locked(&mut state, "[bridgeboard] terminal session started\r\n");
        }
        let reader_session = Arc::clone(&session);
        std::thread::spawn(move || {
            let mut buf = [0_u8; MAX_CHUNK_BYTES];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        let mut state = reader_session
                            .state
                            .lock()
                            .unwrap_or_else(|err| err.into_inner());
                        if state.status == TerminalStatus::Running {
                            state.status = TerminalStatus::ReaderClosed;
                        }
                        break;
                    }
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                        let mut state = reader_session
                            .state
                            .lock()
                            .unwrap_or_else(|err| err.into_inner());
                        push_chunk_locked(&mut state, &text);
                    }
                    Err(err) => {
                        let mut state = reader_session
                            .state
                            .lock()
                            .unwrap_or_else(|err| err.into_inner());
                        if state.status == TerminalStatus::Running {
                            state.status = TerminalStatus::Error;
                            state.exit = Some(err.to_string());
                            push_chunk_locked(
                                &mut state,
                                &format!("\r\n[bridgeboard] terminal read error: {err}\r\n"),
                            );
                        }
                        break;
                    }
                }
            }
        });
        let info = self.info(&session);
        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .insert(info.id.clone(), session);
        Ok(info)
    }

    fn sessions(&self) -> Vec<Arc<TerminalSession>> {
        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn get(&self, session_id: &str) -> Result<Arc<TerminalSession>> {
        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .get(session_id)
            .cloned()
            .with_context(|| format!("terminal session `{session_id}` was not found"))
    }

    fn info(&self, session: &Arc<TerminalSession>) -> TerminalSessionInfo {
        self.refresh_child_status(session);
        let state = session.state.lock().unwrap_or_else(|err| err.into_inner());
        TerminalSessionInfo {
            id: session.id.clone(),
            title: session.title.clone(),
            service_id: session.service_id.clone(),
            command: session.command.clone(),
            cwd: session
                .cwd
                .as_deref()
                .map(|path| path.display().to_string()),
            started_at: session.started_at.clone(),
            status: status_label(state.status).into(),
            exit: state.exit.clone(),
            latest_seq: state.next_seq.saturating_sub(1),
        }
    }

    fn is_running(&self, session: &Arc<TerminalSession>) -> bool {
        self.refresh_child_status(session);
        let state = session.state.lock().unwrap_or_else(|err| err.into_inner());
        state.status == TerminalStatus::Running
    }

    fn refresh_child_status(&self, session: &Arc<TerminalSession>) {
        {
            let state = session.state.lock().unwrap_or_else(|err| err.into_inner());
            if state.status != TerminalStatus::Running
                && state.status != TerminalStatus::ReaderClosed
            {
                return;
            }
        }
        let mut child = session.child.lock().unwrap_or_else(|err| err.into_inner());
        let Ok(Some(exit)) = child.try_wait() else {
            return;
        };
        let mut state = session.state.lock().unwrap_or_else(|err| err.into_inner());
        if state.status == TerminalStatus::Running || state.status == TerminalStatus::ReaderClosed {
            state.status = TerminalStatus::Exited;
            let label = exit_label(&exit);
            state.exit = Some(label.clone());
            push_chunk_locked(
                &mut state,
                &format!("\r\n[bridgeboard] terminal exited: {label}\r\n"),
            );
        }
    }
}

struct TerminalSpec {
    title: String,
    service_id: Option<String>,
    argv: Vec<OsString>,
    command: String,
    cwd: Option<PathBuf>,
}

impl TerminalSpec {
    fn default_shell() -> Self {
        let argv = default_shell_argv();
        let command = argv
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            title: "Local Shell".into(),
            service_id: None,
            argv,
            command,
            cwd: None,
        }
    }

    fn from_service(cfg: &BridgeConfig) -> Result<Self> {
        let (argv, command) = service_terminal_command(cfg)?;
        Ok(Self {
            title: cfg.title.clone(),
            service_id: Some(cfg.id.clone()),
            argv,
            command,
            cwd: crate::config::service_cwd(cfg).map(Path::to_path_buf),
        })
    }
}

fn service_terminal_command(cfg: &BridgeConfig) -> Result<(Vec<OsString>, String)> {
    if cfg.service.mode == ServiceMode::Managed && !cfg.service.command.is_empty() {
        let argv = cfg
            .service
            .command
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        return Ok((argv, cfg.service.command.join(" ")));
    }
    if !cfg.service.command.is_empty() {
        let argv = cfg
            .service
            .command
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        return Ok((argv, cfg.service.command.join(" ")));
    }
    if let Some(command) = cfg.service.start_command.as_deref() {
        return Ok((shell_command_argv(command), command.to_string()));
    }
    bail!(
        "service `{}` has no service.command or service.start_command for terminal launch",
        cfg.id
    )
}

fn push_chunk_locked(state: &mut TerminalState, text: &str) {
    let seq = state.next_seq;
    state.next_seq = state.next_seq.saturating_add(1);
    state.chunks.push_back(TerminalChunk {
        seq,
        text: text.to_string(),
    });
    while state.chunks.len() > MAX_CHUNKS {
        state.chunks.pop_front();
    }
}

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        cols: cols.clamp(20, 300),
        rows: rows.clamp(5, 120),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn new_session_id() -> String {
    let mut random = [0_u8; 4];
    let _ = getrandom::fill(&mut random);
    format!(
        "{}-{}",
        crate::time::now_iso()
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>(),
        hex_encode(&random)
    )
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn status_label(status: TerminalStatus) -> &'static str {
    match status {
        TerminalStatus::Running => "running",
        TerminalStatus::Exited => "exited",
        TerminalStatus::ReaderClosed => "closed",
        TerminalStatus::Error => "error",
        TerminalStatus::Killed => "killed",
    }
}

fn exit_label(exit: &portable_pty::ExitStatus) -> String {
    if let Some(signal) = exit.signal() {
        format!("signal:{signal}")
    } else {
        format!("exit:{}", exit.exit_code())
    }
}

#[cfg(windows)]
fn default_shell_argv() -> Vec<OsString> {
    vec![
        OsString::from("powershell.exe"),
        OsString::from("-NoLogo"),
        OsString::from("-NoProfile"),
    ]
}

#[cfg(not(windows))]
fn default_shell_argv() -> Vec<OsString> {
    vec![OsString::from(
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
    )]
}

#[cfg(windows)]
fn shell_command_argv(command: &str) -> Vec<OsString> {
    vec![
        OsString::from("cmd.exe"),
        OsString::from("/d"),
        OsString::from("/c"),
        OsString::from(command),
    ]
}

#[cfg(not(windows))]
fn shell_command_argv(command: &str) -> Vec<OsString> {
    vec![
        OsString::from("sh"),
        OsString::from("-lc"),
        OsString::from(command),
    ]
}
