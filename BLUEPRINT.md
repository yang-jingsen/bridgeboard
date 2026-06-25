# Bridgeboard Blueprint

## Goal

Bridgeboard is the local control plane for fixed-port research services, project portals, agent handoffs, and optional SSH tunnels across configured peer machines. Agents should be able to start or register a service without inventing one-off process management; users should be able to see, open, stop, restart, and debug those services from CLI, tray, native UI, or dashboard.

## Current Baseline

- Rust CLI and shared core actions for register, handoff, list, status, ports, up, down, restart, logs, open, startup, supervise, doctor, watch, dashboard, and registry export.
- `portal-bridge.v1` config with fixed 24xxx port policy, managed/external service modes, optional lifecycle policy, service logs, health URL, and tunnel modes.
- External agent handoff creates a YAML config under the local Bridgeboard handoff directory and registers it.
- Linux release binaries and Windows release binaries exist; both platforms now use the Tauri `bridgeboard-tray` shell as the preferred tray/window app, with the older terminal/native UI paths kept as fallbacks.
- External handoff can now either record an existing service or launch a Windows-owned service through a Bridgeboard-managed Scheduled Task, then resolve the real listener PID from the service port.
- Active lab installations have been replaced with the Tauri tray build after user authorization.

## v0.3 Scope

Make agent/service handoff reliable enough for Windows and Linux service deployment:

- Record richer handoff metadata: `cwd`, command metadata, log path, stop/restart commands, Windows task name, PID source, and both local/network URLs.
- Validate service health at handoff time and persist status so stale or failed services are visible.
- Resolve the real listening process PID from a port, especially on Windows where SSH-launched wrappers often report the wrong PID.
- Provide agent-friendly stop/status/log surfaces without requiring agents to read YAML or manually inspect ports.
- Add Windows detach support in a later stage so agents can launch via Bridgeboard rather than writing scheduled-task boilerplate.

## Subsystem Boundaries

- `config`: compatibility-preserving schema additions and validation.
- `process`: platform-specific process/PID/discovery helpers and future Windows task helpers.
- `core`: semantic service actions and state updates; no CLI parsing.
- `main`: command adapter only.
- `dashboard`, tray, and native UI: presentation over shared core rows/actions.

## Branch / Checkpoint Strategy

This folder is a Git repository. Keep checkpoints as small commits with validation logs in `CHANGELOG.md`. Preserve behavior unless explicitly changing handoff semantics. Do not touch live project services while developing handoff changes unless separately requested.

## Completed Stage 1 Plan

Goal: Strengthen the existing handoff path without changing how agents launch services yet. After this stage, a handoff can validate health, discover the real listener PID from a port, preserve richer metadata for future stop/restart support, and expose cleaner logs/status.

| Task | Status | Notes |
| --- | --- | --- |
| Add compatibility-preserving schema fields for handoff metadata and URLs | Completed | `open_url` remains compatible; `local_url`/`network_url` added. |
| Add `--pid-from-port`, `--require-healthy`, and metadata CLI flags to `handoff` | Completed | No external services were started. |
| Persist handoff health/PID status into state | Completed | Failed health records `handoff-unhealthy`; `--require-healthy` rejects before writing. |
| Sanitize `bridgeboard logs` output for NUL/encoding issues | Completed | Uses lossy UTF-8 read and strips NUL bytes. |
| Add `stop <id>` alias to `down <id>` | Completed | `down` compatibility preserved. |
| Update README/examples and validation log | Completed | Includes Windows handoff guidance. |

Validation:

- `cargo fmt --check`
- `cargo test`
- `cargo check --target x86_64-pc-windows-gnu --bins`
- CLI smoke checks with temp config/state/registry, no live services.

## Completed Stage 2 Plan

Goal: Let agents ask Bridgeboard to start a Windows-owned service in a session-stable way instead of hand-writing Scheduled Task boilerplate. This stage should still avoid touching live project services during validation; use temp config/state and compile/smoke checks unless the user explicitly asks for deployment.

| Task | Status | Notes |
| --- | --- | --- |
| Add CLI detach strategy `--detach scheduled-task` | Completed | Requires `--start-command`; Windows-only launch path. |
| Generate a UTF-8 cmd wrapper for scheduled-task services | Completed | Wrapper lives next to handoff YAML and redirects logs with cmd redirection. |
| Create/run Windows Scheduled Task and wait for PID/health | Completed | Uses PID-from-port after launch by default. |
| Make external `stop`/`restart` honor recorded stop/restart commands | Completed | Preserves record-only behavior when commands are absent. |
| Update docs/examples/changelog | Completed | Includes agent command examples and safety notes. |

Validation:

- `cargo fmt --check`
- `cargo test`
- `cargo check --target x86_64-pc-windows-gnu --bins`
- Safe CLI smoke tests with temp paths; no live project service mutation.

## Completed Stage 3 Plan

Goal: Make `status --json` useful as an agent handoff API so agents do not need to parse YAML for common metadata.

| Task | Status | Notes |
| --- | --- | --- |
| Add health/status/PID/task/URL metadata to `StatusRow` JSON | Completed | Text table remains compact for humans. |
| Export detach/task metadata through peer registry export | Completed | Keeps older peer exports compatible through serde defaults. |

Validation:

- `cargo check`
- Final suite listed in changelog.

## Completed Stage 4 Package

Goal: Produce refreshed distributable artifacts without mutating live services or active Bridgeboard installs.

| Task | Status | Notes |
| --- | --- | --- |
| Build Linux release CLI/tray/UI binaries | Completed | `target/release/bridgeboard*` rebuilt locally. |
| Build Windows release CLI/tray/UI binaries | Completed | Built on a Windows peer in a temp source checkout and copied back. |
| Refresh binary installers | Completed | Installers copy prebuilt binaries, icons, examples, and login tray launchers. |
| Recreate Windows/Linux distribution archives | Completed | Outputs are under `dist/`. |

Validation:

- `cargo fmt --check`
- `cargo test`
- `cargo check --target x86_64-pc-windows-gnu --bins`
- Windows release build on a Windows peer
- Package listing checks for both archives
- No live project service mutation.

## Completed Stage 5 Remote Owner Controls

Goal: Let tray/CLI users explicitly start a service on its owner host without making `Open` silently launch remote workloads.

| Task | Status | Notes |
| --- | --- | --- |
| Add `remote-up <id>` CLI | Completed | SSHs to the owner peer and runs `bridgeboard up <id>`. |
| Make peer `open <id>` network-aware | Completed | Uses local tunnel URL when a tunnel is opened; otherwise prefers `network_url`. |
| Add remote service actions to Linux tray/native UI | Completed | Peer records show network open and owner-start actions. |
| Update docs and handoff protocol | Completed | Agents can read the protocol instead of receiving ad hoc instructions. |

Validation:

- `cargo fmt`
- `cargo test`
- `cargo run --bin bridgeboard -- remote-up definitely-missing-service-for-smoke` failed cleanly without mutating services
- `python3 -m py_compile scripts/bridgeboard-tray.py`

## Completed Stage 6 Tauri Shell

Goal: migrate the desktop shell to a lightweight Tauri tray/window app, similar to a Clash-style control panel: a native tray owns the local dashboard window and the dashboard itself provides service controls.

| Task | Status | Notes |
| --- | --- | --- |
| Add a Tauri tray subproject without binding the CLI crate to Tauri | Completed | `apps/bridgeboard-tauri` builds `bridgeboard-tray`; CLI/lib remain WebView-free. |
| Start/reuse the existing dashboard and lifecycle worker from the Tauri tray | Completed | Avoids a separate browser command window and owns the local `24000` dashboard. |
| Add dashboard action API/buttons for service control | Completed | `remote-up`, `up`, `down`, `stop`, and `restart` route through local HTTP actions. |
| Update Linux/Windows installers to prefer the Tauri tray binary | Completed | Old tray implementations remain fallback/source files only. |
| Validate Linux runtime and package both platforms | Completed | Built, installed, and smoke-tested on Linux and Windows test hosts. |

Validation:

- `cargo test`
- `cargo build --release --bins`
- `cargo build --manifest-path apps/bridgeboard-tauri/Cargo.toml --release`
- Tauri tray/dashboard smoke on Linux
- Windows Tauri build/deploy smoke
- `/health` and dashboard HTML smoke checks on both platforms

## Current Stage 7 UI Operations

Goal: make the dashboard more useful as the daily service console without turning it into a heavy app. Keep service ids and ports stable, but allow safe metadata edits such as display-title rename through the same core/CLI/remote-owner path that agents can use.

| Task | Status | Notes |
| --- | --- | --- |
| Add safe display-title rename to core and CLI | Completed | Rename `title`, not service id or port. |
| Route remote peer rename through owner Bridgeboard over SSH | Completed | Peer records mutate on the owning host. |
| Add dashboard rename control and improve service scanning | Completed | Search/filter, port badges, fixed-width row actions, and state-first `Start`/`Stop` controls. |
| Add remote owner stop/restart actions | Completed | `remote-down` and `remote-restart` control the owner service over SSH. |
| Fix native window/toast ergonomics | Completed | Larger default Tauri window and click-through lower-left action toast. |
| Make external stop kill real Windows child listeners | Completed | Stop now ends task and kills recorded/listening PID when available. |
| Add configurable tunnel defaults | Completed | User config can default handoffs to `local_forward` and locally assume `local_forward` for peer records that forgot it. |
| Validate and redeploy Tauri tray packages on lab hosts | Completed | Both trays restarted; Linux tarball and Windows zip refreshed. |

Validation:

- `cargo fmt --check`
- `cargo test`
- Tauri release build on Linux and Windows
- `/health` and dashboard HTML/action smoke checks on both platforms
- Remote rename smoke between configured peers with a space-containing title

## Completed Stage 8 Public Packaging

Goal: make Bridgeboard suitable for a public repository without baking in lab
machine names, private paths, or private network conventions. Existing user
installations keep their local config files; the repository should ship generic
examples that operators can copy and edit.

| Task | Status | Notes |
| --- | --- | --- |
| Replace lab-specific README/protocol examples | Completed | Uses `workstation`, `gpu-box`, and generic service ids. |
| Replace machine-specific example configs | Completed | Provides copyable generic peer config examples only. |
| Remove machine-specific installer defaults | Completed | Installers do not create a host-specific config by default. |
| Sanitize code/test fixture wording | Completed | Behavior unchanged; user-visible examples and tests use generic names. |
| Rebuild and refresh distributable archives | Completed | Packages contain the generic examples/docs. |

Validation:

- `cargo fmt --check`
- `cargo test`
- `cargo build --release --bins`
- Windows `cargo build --release --bins`
- Linux and Windows archive refresh

## Current Stage 9 Federated Service Identity

Goal: let multiple peer machines expose services with the same stable service id
and owner-local port without dashboard or CLI actions targeting the wrong host.
The service `port` remains the owner's real service port. A local SSH tunnel
uses that same port by default; `--local-port` is only for local conflicts or
when the operator intentionally wants a distinct local tunnel port.

| Task | Status | Notes |
| --- | --- | --- |
| Scope port conflict validation by `(owner_host, port)` | Completed | Different owners can each reserve the same owner-local port. |
| De-duplicate peer rows by `(owner_host, port, id)` | Completed | Peer views no longer collapse same-id services from different owners. |
| Route dashboard actions with owner/source context | Completed | Buttons target the selected row, not the first matching id. |
| Stop only tunnels for the selected peer owner | Completed | Avoids deleting unrelated same-id tunnels. |
| Validate refreshed source build | Completed | Full Rust checks pass locally. |
| Deploy refreshed binaries | Pending | Replace local installs after the source checkpoint. |

Validation:

- `cargo fmt --check`
- `cargo test`
- `cargo check --bins`
