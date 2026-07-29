# Bridgeboard Changelog

- 2026-07-30: Added `bridgeboard ports --no-runtime` for fast app-panel
  listing on Windows owners. The default `ports` behavior still performs local
  runtime/PID checks, while `--no-runtime` exports local rows without
  per-service probes and marks skipped local runtime as `not-checked`.
  Validation: eva-02 `ports --json --no-runtime` measured 0.03s, and
  `ports --json --peers --no-runtime` measured 4.75s, below TethysUNE's 20s
  provider timeout.
- 2026-07-30: Added the `prepare-open` JSON integration command for
  TethysUNE-style native shells. It accepts a stable service reference
  (`--id`, optional `--owner-host`, `--source-machine`, `--local-port`, and
  `--target internal|external`), performs on-demand service and peer tunnel
  preparation, and returns the resolved URL plus action/warning metadata
  without opening the system browser. The result includes the owner/source
  config path so TethysUNE can audit legacy handoff records such as Denia Score
  Annotator without mutating them. Existing `bridgeboard open` behavior is
  preserved by opening the prepared URL externally. Validation: `cargo fmt
  --check`, `cargo test`, `cargo check --bins`, and Windows target check passed;
  refreshed CLI binaries were deployed to tethys and eva-02; Denia prepared on
  both hosts and `http://127.0.0.1:24321/` returned HTTP 200 from tethys.
- 2026-07-29: Renamed the TethysUNE branch desktop/dashboard display surface
  from Bridgeboard to TethysUNE. The CLI binary and agent handoff command remain
  `bridgeboard` for compatibility with existing services and prompts.
- 2026-07-29: Refined the TethysUNE dashboard palette toward a blue/black/white
  Shorekeeper-inspired direction and added a separate read-only `Devices`
  page. The page currently summarizes local/peer/owner machines from the
  service registry; TODO: turn it into the configuration surface for local and
  peer display names, SSH aliases, dashboard endpoints, trust policy, and
  local-forward defaults.
- 2026-07-29: Started the TethysUNE terminal-sessions branch. The dashboard
  now has a `Terminals` view backed by `portable-pty`, so Linux shells and the
  Windows ConPTY path share one internal API. The MVP exposes token-protected
  local terminal endpoints for start/list/read/input/resize/stop, supports
  launching a local shell or a locally registered service with
  `service.command`/`service.start_command`, and adds `Terminal` buttons to
  local service cards. The first UI is a lightweight polling terminal pane with
  ANSI escape cleanup; full xterm.js/WebSocket TUI support remains a follow-up.
  Terminal APIs are disabled when the dashboard is bound to a non-loopback
  address, even if the normal dashboard was explicitly exposed.
  Validation: `cargo fmt --check` passed; `cargo test` passed with 18 tests;
  `cargo check --bins` passed; `cargo check --target x86_64-pc-windows-gnu
  --bin bridgeboard` passed; a Linux shell session was started through the
  dashboard API, received input, returned output, and was stopped. Direct
  Tauri check on this Linux host still requires GTK/WebKit pkg-config
  development files.
- 2026-07-09: Avoided a slow Windows dashboard path when showing peer
  services. Remote rows now decide whether to use a same-port local forward or
  a fallback port from a direct loopback port-open check, instead of running a
  per-row process-owner lookup through PowerShell/CIM. This keeps eva-02 and
  tethys peer visibility responsive when many peer services are shown.
- 2026-06-26: Made peer service identity owner-aware for federation. Port
  conflict validation is now scoped to `(owner_host, port)`, so different hosts
  can each expose their own service on the same owner-local port. Dashboard and
  `ports --peers` row de-duplication now uses `(owner_host, port, id)` instead
  of `(port, id)`, so `tethys:cutex-agent-bus:24260` and
  `eva-02:cutex-agent-bus:24260` can both be listed. Dashboard row actions now
  pass `owner_host` and `source_machine` to the backend, so open/start/stop/
  restart/rename target the selected owner instead of the first matching id.
  The README and `port-plan` wording now describe the owner-local port rule:
  local forwards mirror the owner's service port by default, and `--local-port`
  is only for local conflicts or intentionally distinct tunnel ports.
  Registration-time port validation now uses no-runtime exports, and Unix PID
  liveness probes silence `kill -0` stderr so stale PIDs do not pollute
  dashboard or peer-export output. Record-only external handoffs no longer kill
  the recorded PID/listener on `stop`; Bridgeboard only cleans external child
  processes when the record has an explicit `stop_command` or scheduled-task
  ownership metadata. Remote peer rows now carry an action `local_port`; when
  the owner's fixed port is already held by a local non-SSH service, the
  dashboard uses a stable fallback such as `24260 -> 24660` for that peer
  tunnel instead of opening the wrong local service.
  Validation: `cargo fmt --check` passed; `cargo test` passed with 18 tests;
  `cargo check --bins` passed.
- 2026-06-25: Fixed two dashboard regressions seen with tethys/eva-02 peer
  management. The dashboard API now logs peer port conflicts but still returns
  the available service rows, so one unrelated duplicate port no longer makes
  the whole peer view disappear. The Apps launcher now treats remote on-demand
  "Start & Open" as `remote-up` followed by open, instead of only creating a
  local forward for a service that has not been started on its owner host.
  Plain CLI `up`/`open` for peer services now follows the same owner-start
  behavior before opening local forwards, so agents do not need to know the
  dashboard-only `remote-up` action.
  Managed services with a passing health check now reconcile pid_file/state to
  the current single listener PID, covering Windows launches where an initial
  wrapper PID is replaced by the real long-running process.
  Remote up/restart now reuses an already-listening reverse-forward port
  instead of creating a duplicate same-port local-forward process that exits
  immediately, using a short loopback TCP readiness check so Linux can detect
  SSH reverse listeners even when PID ownership is not visible.
  Unix local-forward tunnel startup now uses `ssh -f` with
  `ExitOnForwardFailure` and waits for the listener PID before returning, so
  CLI-launched forwards survive after the Bridgeboard command exits.
- 2026-06-25: Made the dashboard resilient to slow Windows PID probes and
  flaky SSH peer discovery. The web dashboard now keeps export snapshots in
  memory and on disk (`dashboard-cache.json`). `/api/ports` returns the last
  successful local/peer snapshot immediately while a background refresh updates
  the cache. Slow local port checks or peer SSH calls no longer block the page
  or make remote services disappear on transient timeouts.
- 2026-06-24: Made peer registry fetches fast by default. `registry export`
  now accepts `--no-runtime`, and SSH peer discovery uses that mode so the
  dashboard can list remote services without waiting on every Windows
  PID/port probe. Owner-local status commands still compute live runtime state.
- 2026-06-24: Tightened managed `up` after Windows PID mismatch reports.
  After `start_service` returns, Bridgeboard now samples managed runtime state
  repeatedly and requires `running:<pid>` before it marks the service started
  or opens tunnels. If the pid_file PID and fixed-port listener still disagree,
  `up/open` fails with the explicit unstable status instead of reporting a
  misleading successful start.
- 2026-06-24: Made fixed-port listener detection set-based. Bridgeboard now
  reads every PID listening on a configured port instead of selecting an
  arbitrary first PID. Managed status reports `multi-listener:<pids>` when a
  Windows port has multiple owners, and managed start/stop paths refuse or
  clean up based on the full listener set. This prevents status from
  oscillating between `running` and `pid-mismatch` when Windows reports
  multiple listeners on the same port.
- 2026-06-24: Prevented accidental deployment of the legacy Windows tray under
  the modern tray name. The root crate now exposes the fallback native Win32
  tray as `bridgeboard-win32-tray`; the `bridgeboard-tray` binary name is
  reserved for the Tauri tray/window shell built from `apps/bridgeboard-tauri`,
  matching Linux and Windows desktop behavior.
- 2026-06-24: Added response-body health expectations. Service configs can now
  set `service.health_expect.body_contains`, and `bridgeboard handoff` accepts
  repeatable `--health-contains <text>` checks. Health checks still require a
  2xx/3xx response and now also fail when required body markers such as
  `"version": 3` are missing. Managed service startup records `healthy` or
  `unhealthy` plus the last health result in state so the dashboard can show
  business-version mismatches instead of only process liveness. Validation:
  `cargo fmt --check` passed; `cargo test` passed with 13 tests;
  `cargo check --bins` passed.
- 2026-06-24: Hardened managed service process/port consistency. Managed
  service status now compares the pid_file PID with the actual configured port
  listener, reporting `pid-mismatch`, `no-listener`, `stale:...;listener:...`,
  or `port-owned` instead of blindly treating a live pid_file process as
  running. Managed `up` refuses to start over a stale listener owned by another
  PID, waits for the launched service to bind the configured port, records the
  real listener PID, and includes recent log lines when startup exits or times
  out. Managed `stop` now attempts both the pid_file PID and fixed-port
  listener PID, surfacing `taskkill`/`kill` output on failure. Validation:
  `cargo fmt --check` passed; `cargo test` passed with 11 tests;
  `cargo check --bins` passed.
- 2026-06-23: Fixed Windows remote SSH tunnels started through Bridgeboard.
  Windows tunnel startup now uses a one-shot Scheduled Task so `ssh -N`
  survives the OpenSSH session that invoked `bridgeboard up` remotely. Tunnel
  state records the task name for later cleanup, and startup waits for the real
  `ssh.exe` PID by listener port or matching command line. Validation:
  `cargo fmt --check` passed; `cargo test` passed with 11 tests;
  `cargo check --bins` passed; Windows `cargo build --release --bins` passed on
  EVA-02; `bridgeboard up --peer tethys --local-port 24660 cutex-agent-bus`
  kept `127.0.0.1:24660` listening after the SSH session exited and returned
  HTTP 200.
- 2026-06-04: Added an app-launcher dashboard mode. The dashboard now opens on
  an `Apps` panel for recorded web apps, with `Services` and `Ports` kept as
  operator views. API rows include `direct_open`, allowing already-running
  services with reachable URLs to use a quick backend URL-open path that avoids
  a fresh SSH peer lookup, while on-demand starts and missing SSH local forwards
  still use Bridgeboard's managed action path. Peer registry fetches now run in
  parallel to reduce dashboard load time when multiple SSH peers are configured. Validation:
  `cargo fmt --check` passed; `cargo test` passed with 10 tests;
  `cargo check --bins` passed; a local `--no-peers` dashboard smoke produced
  `/tmp/bridgeboard-apps-fastopen.png`.
- 2026-06-02: Hardened the local control surface before public release.
  Dashboard state-changing APIs now require `POST` plus a random per-process
  `X-Bridgeboard-Token`, and `bridgeboard serve` refuses non-loopback binds
  unless `--unsafe-remote-dashboard` is explicitly set. Peer SSH commands now
  pass service arguments through a hidden hex-encoded JSON command path instead
  of interpolating user-controlled titles into a remote shell command line.
  Added README security model notes. Validation: `cargo fmt --check` passed;
  `cargo test` passed with 10 tests; smoke checks confirmed `exec-encoded`
  dispatch, dashboard token injection, GET action rejection, tokenless POST
  rejection, and non-loopback bind refusal.
- 2026-05-29: Generalized Bridgeboard for public packaging. Repository docs,
  agent handoff guidance, example configs, source test fixtures, and CLI port
  policy wording now use generic `workstation`/`gpu-box` examples instead of
  lab-specific hostnames or paths. Windows install no longer creates a
  host-specific config file by default; installers copy examples and preserve
  local config. Refreshed Linux and Windows archives with generic examples.
  Validation: `cargo fmt --check` passed; `cargo test` passed with 9 tests;
  Linux `cargo build --release --bins` passed; Windows `cargo build --release
  --bins` passed on a Windows peer; package contents were listed after rebuild.
- 2026-05-29: Reworked service cards as a balanced three-column grid so the
  action area stays visible while extra width is shared between info and tags.
  `Start`/`Stop` and `Restart` now sit on the same action row, reducing card
  height. Added client-side service sort modes and local pinning with a star
  button; pinned services stay at the top.
- 2026-05-29: Tightened the service row layout from four columns to three.
  Service URLs now live under the service identity/owner/source line instead
  of occupying a dedicated column.
- 2026-05-29: Added a dashboard `Copy Agent Prompt` action next to
  `Open Browser` and `Refresh`. The dashboard now exposes
  `/api/agent-prompt` and copies a short bilingual handoff prompt for agents
  that need to register background web services. The page shell no longer
  scrolls as a whole; only the active services/ports pane scrolls.
- 2026-05-29: Split the agent prompt copy action by machine. The dashboard now
  exposes `/api/agent-prompts` and renders `Copy <machine>` buttons for the
  local host and configured peers, so SSH workflows can copy the correct
  `--owner-host` prompt for any configured host.
- 2026-05-26: Added display-title rename and a cleaner service dashboard. New
  `bridgeboard rename <id> --title ...` edits only the title field and forwards
  peer-owned renames to the owner host over SSH. The dashboard now includes
  search, port badges, fixed-width two-column action groups, and per-service
  `Rename` controls while keeping service ids and ports stable.
- 2026-05-26: Reworked dashboard actions around service state instead of
  locality. Cards now show prominent `Running`/`Stopped`/`Stale` status and
  use service-level `Start`/`Stop`/`Restart` controls. New
  `remote-down <id>` and `remote-restart <id>` commands let non-owner hosts
  stop or restart owner services over SSH, while local/remote context is shown
  as badges.
- 2026-05-26: Increased the native Tauri window default size for 4K displays,
  moved action toasts away from the right-side button area, and made toasts
  click-through. External service stop now ends a Windows scheduled task and
  then kills the recorded/listening service process when Bridgeboard can
  identify it from the fixed port.
- 2026-05-27: Fixed Windows tray dashboard console flicker. Internal status,
  peer SSH, port PID, task, and process-control helper commands now use
  `CREATE_NO_WINDOW` on Windows, so opening the Tauri panel no longer spawns
  transient PowerShell/SSH console windows or retriggers focus refresh loops.
- 2026-05-27: Added Bridgeboard-wide tunnel defaults. `defaults.handoff_tunnel_modes`
  controls what `bridgeboard handoff` writes when an agent omits
  `--tunnel-mode`, and `defaults.assume_local_forward_for_peers` lets a local
  operator SSH-forward peer records that forgot to declare `local_forward`.
- 2026-05-26: Removed fixed 5-second dashboard polling. The dashboard now
  refreshes on initial load, manual refresh, focus/visibility return, and after
  service actions, avoiding constant SSH peer registry fetches while preserving
  explicit operator updates.
- 2026-05-26: Changed external handoff defaults toward SSH-local access instead
  of direct VPN/LAN HTTP. `bridgeboard handoff` now defaults to
  `local_forward` unless `--no-tunnel` is passed, and peer dashboard rows prefer
  the same fixed local port URL when a peer service allows `local_forward`.
  This lets a service owned by a peer on `24201` be opened locally through
  `127.0.0.1:24201` while the service remains bound to the owner host's
  localhost.
- 2026-05-26: Changed dashboard open behavior so service URLs no longer open
  inside the Tauri WebView. Dashboard `Open` now calls Bridgeboard's backend
  `open` action, which uses the system browser and preserves on-demand/tunnel
  semantics. The Tauri close handler now hides only the main Bridgeboard window;
  any secondary WebView windows can close normally. Validation: `cargo
  fmt --check` passed; `cargo test` passed with 7 tests; Tauri release build
  passed with local runtime-library symlinks; the local tray was restarted and
  `/health` returned `ok`; dashboard HTML no longer contains service
  `window.open` calls.
- 2026-05-25: Fixed Tauri tray lifetime on desktop close. The main
  Bridgeboard window now hides when the user clicks the window close button,
  and only the tray `Quit` action exits the process. This prevents the KDE tray
  icon from disappearing after closing the dashboard window. Validation:
  `cargo fmt --check` passed; Linux Tauri release build passed; the local
  `bridgeboard-tray.service` is active; `/health` returns `ok`; KDE
  StatusNotifierWatcher lists the Bridgeboard tray item.
- 2026-05-25: Migrated the preferred desktop shell to Tauri and refined the
  dashboard into a Clash-style service control panel. Added
  `apps/bridgeboard-tauri`, expanded `src/dashboard.rs` with local action APIs
  and service controls, updated Linux/Windows packaging to install the Tauri
  `bridgeboard-tray` binary, and refreshed README/blueprint guidance. Validation:
  `cargo fmt --check` passed; `cargo test` passed with 7 tests;
  `cargo build --manifest-path apps/bridgeboard-tauri/Cargo.toml --release`
  passed on Linux; Windows Tauri build passed on a Windows peer; deployed tray
  builds on both test hosts returned `/health` = `ok` and served the refined
  dashboard.
- 2026-05-25: Added explicit remote owner controls. Changed `src/peer.rs`,
  `src/core.rs`, `src/main.rs`, `src/bin/bridgeboard-ui.rs`,
  `scripts/bridgeboard-tray.py`, `README.md`, and `AGENT_SERVICE_HANDOFF.md`
  because tray/UI users should be able to start a peer-owned service on its
  owner host without making `Open` silently launch remote workloads.
  Validation: `cargo fmt` passed; `cargo test` passed with 7 tests;
  `cargo run --bin bridgeboard -- remote-up definitely-missing-service-for-smoke`
  failed cleanly without mutating services; `python3 -m py_compile
  scripts/bridgeboard-tray.py` passed.
- 2026-05-25: Refreshed and packaged v0.3 binaries. Windows and Linux bundles
  now install prebuilt CLI/tray/UI binaries, icons, examples, and login tray
  launchers without depending on local source paths or local rebuilds.
  Validation: `cargo fmt --check` passed; `cargo test` passed with 7 tests;
  `cargo check --target x86_64-pc-windows-gnu --bins` passed before package
  refresh; Windows release binaries were built on a Windows peer from a temp source
  checkout; archive listing checks passed for both packages. Active lab
  Bridgeboard installs were not replaced.
- 2026-05-24: Started v0.3 handoff reliability work. Updated `BLUEPRINT.md`
  with scope, subsystem boundaries, staged validation rules, and the Stage 1
  checklist. Validation: planning checkpoint only. Follow-up: implement Stage 1
  schema/CLI/core changes and record test results here.
- 2026-05-24: Completed v0.3 Stage 1 handoff reliability slice. Changed
  `src/config.rs`, `src/main.rs`, `src/core.rs`, `src/process.rs`,
  `src/state.rs`, `src/registry.rs`, `README.md`, and
  `examples/external-handoff.yaml` because Windows agent handoff needs
  health visibility, PID-from-port discovery, richer metadata, safer logs, and
  a `stop` alias. Validation: `cargo fmt --check` passed; `cargo test` passed
  with 7 tests; `cargo check --target x86_64-pc-windows-gnu --bins` passed;
  temp handoff smoke recorded `handoff-unhealthy` without touching live
  services; `--require-healthy` rejected before writing a handoff file; log
  smoke stripped NUL bytes. Follow-up: Stage 2 should implement
  `handoff --start-command ... --detach scheduled-task` for Windows-owned
  services.
- 2026-05-25: Started v0.3 Stage 2. Updated `BLUEPRINT.md` with the active
  scheduled-task detach checklist. Validation: planning checkpoint only.
  Follow-up: implement CLI/process/core changes and safe smoke tests without
  mutating live project services.
- 2026-05-25: Completed v0.3 Stage 2 scheduled-task detach. Changed
  `src/main.rs`, `src/process.rs`, `src/core.rs`, `src/config.rs`,
  `src/registry.rs`, `README.md`, and `examples/external-handoff.yaml` because
  Windows agents need Bridgeboard-owned Scheduled Task launch, wrapper log
  redirection, PID-from-port after launch, and command-aware external
  stop/restart. Validation: `cargo fmt --check` passed; `cargo test` passed
  with 7 tests; `cargo check --target x86_64-pc-windows-gnu --bins` passed;
  Linux smoke confirmed `--detach scheduled-task` rejects without writing a
  handoff file; temp external service smoke confirmed `stop_command` and
  `restart_command` execution. Follow-up: package v0.3 and deploy only when the
  user authorizes replacing active Bridgeboard binaries.
- 2026-05-25: Completed v0.3 Stage 3 status JSON enrichment. Changed
  `src/status.rs` and `src/registry.rs` because later agents need health,
  last status, PID source, PID port, task name, service mode, lifecycle, and
  local/network URLs from `bridgeboard status --json` instead of parsing
  handoff YAML. Validation: `cargo check` passed. Follow-up: include this in
  final v0.3 package validation.
