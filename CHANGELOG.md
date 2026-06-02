# Bridgeboard Changelog

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
