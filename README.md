# Bridgeboard

Bridgeboard is a low-resource port ledger, service recorder, and optional SSH tunnel manager for project portals and small research tools. It keeps a fixed 24xxx port map across configured peer machines, starts local web services when this machine owns them, and creates SSH local/reverse forwards when a service allows that bridge mode or the local operator has enabled a peer fallback default.

核心概念 / core terms:

- **service**: a project web app or tool. It can be `managed` by Bridgeboard or `external` and only recorded.
- **peer**: another machine reachable by SSH, for example `gpu-box`.
- **portal config**: a project-owned `portal-bridge.yaml` file.
- **registry**: Bridgeboard's local list of known portal configs.
- **port ledger**: the global list of reserved 24xxx ports, including services that are not bridged.
- **mirror port rule**: the same service id owns the same 24xxx port on every peer.

## Port Plan

Run `bridgeboard port-plan` to print the policy.

- `24000`: Bridgeboard reserved.
- `24001-24299`: project-specific portals.
- `24300-24499`: project tools.
- `24500-24699`: agent/protocol/automation services.
- `24700-24899`: ad hoc temporary tunnels.
- `24900-24999`: diagnostics/manual override/emergency.

If `gpu-box` owns `image-review-portal` on `24001`, then `workstation:24001` is reserved for the tunnel entry to that same service. Do not assign `workstation:24001` to a different local app.

A service can be **owner-only** and still reserve its port globally. In that case use `tunnel.modes: []` or `bridgeboard handoff --no-tunnel`. Otherwise Bridgeboard's handoff path defaults to SSH `local_forward`, so a service owned by `gpu-box` on `24201` can be opened from `workstation` through `127.0.0.1:24201` without exposing the service on a shared network or VPN. If `defaults.assume_local_forward_for_peers: true` is set locally, even peer records that forgot to declare `local_forward` can still be opened through SSH.

## Build

```bash
cargo build --release
cargo build --manifest-path apps/bridgeboard-tauri/Cargo.toml --release
```

Put `target/release/bridgeboard` on `PATH` on each peer. Peer sync uses SSH to run:

```bash
bridgeboard registry export --json
```

## Security Model

Bridgeboard is designed as a local operator tool, not an internet-facing
control panel.

- The dashboard binds to `127.0.0.1:24000` by default. Binding it to a
  non-loopback address is refused unless `--unsafe-remote-dashboard` is passed.
- Dashboard actions use `POST` plus a random per-process
  `X-Bridgeboard-Token` embedded in the served dashboard page. This blocks
  ordinary cross-site requests from triggering service start/stop/restart.
- `portal-bridge.yaml`, handoff YAML, and Bridgeboard machine config are
  trusted inputs. Managed service commands and external stop/restart commands
  can execute local shell commands.
- SSH peers are trusted operators. Bridgeboard encodes remote command arguments
  before invoking peer Bridgeboard over SSH so service titles and ids are not
  interpolated by the remote shell.
- External service stop can kill the recorded PID or current listener on the
  service's fixed port. Keep handoff metadata current and do not register
  untrusted services.

## Binary Packages

The release bundles are written under `dist/`:

- `bridgeboard-windows-x64.zip`: Windows CLI, Tauri tray/window shell, legacy UI fallback, icon, examples, and `install-user.ps1`.
- `bridgeboard-linux-x64.tar.gz`: Linux CLI, Tauri tray/window shell, legacy UI fallback, icon, examples, and `install-user.sh`.

Windows install from an extracted zip:

```powershell
.\install-user.ps1
```

Linux install from an extracted tarball:

```bash
./install-user.sh
```

The installers copy binaries into the user bin directory, install the tray launcher, keep any existing Bridgeboard config untouched, copy example configs under Bridgeboard's examples directory, and register the tray for login startup by default. New installs can run without a config file; Bridgeboard falls back to the local hostname until peers are configured.

## Machine Config

Create `~/.config/bridgeboard/config.yaml` on each host:

```yaml
machine_id: workstation
defaults:
  handoff_tunnel_modes: [local_forward]
  assume_local_forward_for_peers: true
peers:
  gpu-box:
    ssh_alias: gpu-box
    bridgeboard_bin: bridgeboard
```

On the peer, use its own `machine_id` and add the workstation as a peer if reverse tunnels should be opened back to this machine.

`bridgeboard_bin` is the command run on the peer during SSH registry sync. It can be an absolute path, for example `/home/alice/.local/bin/bridgeboard`, when non-interactive SSH does not include the user's local bin directory in `PATH`.

Copy/paste examples are in `examples/config.workstation.yaml` and `examples/config.gpu-box.yaml`.

## Portal Config Protocol

Each project writes a `portal-bridge.yaml` file and registers it:

```bash
bridgeboard register /path/to/project/portal-bridge.yaml
```

Minimal project config:

```yaml
schema: portal-bridge.v1
id: image-review-portal
title: Image Review Portal
owner_host: gpu-box
port: 24001
service:
  mode: managed
  lifecycle:
    startup: on_demand
    restart: on_failure
  cwd: /srv/research/image-review-portal
  command: ["python3", "-m", "http.server", "24001", "--bind", "127.0.0.1"]
  pid_file: .bridgeboard/server.pid
  log_file: .bridgeboard/server.log
  health_url: http://127.0.0.1:24001/
  health_expect:
    body_contains:
      - '"service": "image-review-portal"'
  startup_timeout_sec: 10
tunnel:
  modes: [local_forward, reverse_forward]
  bind_host: 127.0.0.1
open_url: http://127.0.0.1:24001/
```

The owning host starts `service.command` for `mode: managed`. Non-owning hosts create an SSH local forward only when `tunnel.modes` includes `local_forward`. If the owner config includes `reverse_forward`, `bridgeboard up <id>` on the owner also opens reverse tunnels to configured peers.

`service.lifecycle` is optional and defaults to `startup: manual` plus `restart: never`:

- `manual`: only `bridgeboard up <id>` starts the service.
- `on_demand`: `bridgeboard open <id>` first ensures the service or local tunnel is running.
- `autostart`: `bridgeboard startup` and tray startup start the local owner service.
- `on_failure`: `bridgeboard supervise` restarts a managed service only while its desired state is `running`.

Autostart is intentionally local-service only: it does not start tunnels and it does not control `external` records.

For a service that should only be recorded and not managed:

```yaml
schema: portal-bridge.v1
id: model-inspector
title: Model Inspector
owner_host: gpu-box
port: 24050
service:
  mode: external
  pid: 12345
  pid_source: port:24050
  pid_port: 24050
  cwd: /path/to/project
  start_command: npm run dev -- --host 127.0.0.1 --port 24050
  detach: scheduled_task
  stop_command: taskkill /PID 12345 /T /F
  restart_command: npm run restart -- --port 24050
  task_name: Bridgeboard-model-inspector
  log_file: server.log
  health_url: http://127.0.0.1:24050/
  notes: started by an agent outside Bridgeboard
tunnel:
  modes: []
  bind_host: 127.0.0.1
local_url: http://127.0.0.1:24050/
network_url: http://100.x.y.z:24050/
open_url: http://127.0.0.1:24050/
```

## Agent Handoff

Agents that start background services can record them without hand-writing YAML:

```bash
bridgeboard handoff \
  --id model-inspector \
  --title "Model Inspector" \
  --port 24050 \
  --owner-host gpu-box \
  --pid-from-port \
  --health-url http://127.0.0.1:24050/ \
  --health-contains '"version": 3' \
  --require-healthy \
  --network-url http://100.x.y.z:24050/ \
  --cwd /path/to/project \
  --log-file server.log \
  --start-command "npm run dev -- --host 127.0.0.1 --port 24050" \
  --detach scheduled-task \
  --task-name Bridgeboard-model-inspector \
  --note "started by agent outside Bridgeboard"
```

`handoff` creates an `external` record and defaults to `local_forward`, so peers can create an SSH local tunnel on the same fixed port. This default comes from `defaults.handoff_tunnel_modes`; set it once in Bridgeboard config instead of repeating `--tunnel-mode local_forward` for every agent. It checks `health_url` when provided, records the result in Bridgeboard state, and can resolve the real listening PID from the port with `--pid-from-port`. Add `--health-contains <text>` one or more times when the response body must contain version markers such as `"version": 3`; managed YAML configs can use `service.health_expect.body_contains` for the same check. Add `--require-healthy` when a failed health check should reject the handoff instead of recording an unhealthy service. Add `--no-tunnel` for a strictly owner-local record.

On Windows, `--detach scheduled-task` makes Bridgeboard create and run a Windows Scheduled Task instead of relying on an SSH session-owned background process. It requires `--start-command`, writes a UTF-8 `.cmd` wrapper next to the handoff YAML, redirects output to `--log-file` or a default handoff log, starts the task, then uses the service port to find the real listener PID.

Useful handoff metadata flags for agent-started services:

```bash
--start-command "npm run dev -- --host 127.0.0.1 --port 24050"
--detach scheduled-task
--stop-command "taskkill /PID 12345 /T /F"
--restart-command "bridgeboard stop eva-only-demo && bridgeboard up eva-only-demo"
--task-name Bridgeboard-eva-only-demo
--local-url http://127.0.0.1:24050/
--network-url http://100.x.y.z:24050/
```

Peer access should normally use SSH local forwarding, not direct VPN/LAN HTTP. This is the default for `handoff`; the explicit equivalent is:

```bash
--tunnel-mode local_forward
```

To allow the owner to open reverse tunnels to peers, add:

```bash
--tunnel-mode reverse_forward
```

## CLI

```bash
bridgeboard register ./portal-bridge.yaml
bridgeboard unregister image-review-portal
bridgeboard handoff --id model-inspector --port 24050 --owner-host gpu-box
bridgeboard ports
bridgeboard ports --peers
bridgeboard list
bridgeboard list --peers
bridgeboard status image-review-portal --peers
bridgeboard up image-review-portal
bridgeboard up --peer gpu-box image-review-portal
bridgeboard up --peer gpu-box --local-port 24660 image-review-portal
bridgeboard remote-up image-review-portal
bridgeboard remote-down image-review-portal
bridgeboard remote-restart image-review-portal
bridgeboard down image-review-portal
bridgeboard stop image-review-portal
bridgeboard restart image-review-portal
bridgeboard rename image-review-portal --title "Image Review Portal"
bridgeboard logs image-review-portal --lines 120
bridgeboard open image-review-portal
bridgeboard startup
bridgeboard supervise --interval 15
bridgeboard doctor
bridgeboard watch
```

`ports --peers` is the quickest operator view: it shows globally reserved ports, owner host, `managed` vs `external`, tunnel modes, and runtime status. `up <id>` can start a local tunnel from a peer registry entry even when the YAML file exists only on that peer, as long as the peer is configured, reachable by SSH, and the service enables `local_forward`. If a local record has the same id and shadows the peer record, use `up --peer <peer> <id>` or `up --host <peer> <id>` to explicitly tunnel the peer export. Add `--local-port <port>` when the peer's fixed service port is already occupied locally; Bridgeboard still forwards to the owner's service port, but listens on the requested local port. If you set `defaults.assume_local_forward_for_peers: true`, Bridgeboard treats peer records with empty `tunnel.modes` as locally tunnelable and displays them as `local(default)`.

`remote-up <id>`, `remote-down <id>`, and `remote-restart <id>` control the service on its owner host over SSH. Use these for remote records such as a `gpu-box` owned service visible from `workstation`. For local owner services they behave like `up`, `down`, and `restart`. Remote start/stop is explicit by design; `open <id>` does not silently start an owner service on another machine.

`unregister <id>` removes a stale local registry entry without touching the service process. Add `--delete-config` to also remove the registered handoff or portal YAML file. This is useful when a machine has an old local record that shadows a peer service id.

`rename <id> --title ...` changes the display title only. It does not change the stable service id, port, state keys, or tunnel keys. If the service is owned by a configured peer, Bridgeboard asks the owner host to apply the rename over SSH.

For peer services, `open <id>` starts a local tunnel only when `local_forward` is enabled. Otherwise it prefers `network_url` before falling back to the exported `open_url`, so peer records do not accidentally open the owner's `127.0.0.1` URL when a VPN or LAN URL is available.

For agents, prefer JSON status instead of reading handoff YAML:

```bash
bridgeboard status model-inspector --json
bridgeboard ports --json --peers
```

`status --json` includes health/status metadata, PID source, PID port, task name, lifecycle, service mode, and local/network URLs when available.

## Dashboard And Tray

The dashboard is a local control service on port `24000`:

```bash
bridgeboard serve
bridgeboard dashboard
```

The preferred desktop entry on Linux and Windows is the Tauri tray/window shell:

```bash
bridgeboard-tray
bridgeboard-tray.exe
```

It owns the local dashboard server on `127.0.0.1:24000`, starts local `autostart` services, runs the lightweight supervisor loop, and opens a small native WebView control window instead of launching a browser or command console. The tray menu includes `Open Bridgeboard`, `Open Web Dashboard`, `Ports`, `Doctor`, and `Quit`. Left-clicking the tray icon opens Bridgeboard.

Build `bridgeboard-tray` from `apps/bridgeboard-tauri`; the root crate's
`bridgeboard-win32-tray` binary is a legacy Windows fallback and intentionally
uses a different native Win32 implementation.

The in-window dashboard is the main UI. Its default `Apps` view is a launcher panel for recorded web apps, while `Services` and `Ports` keep the denser operator views. It shows local and peer services, fixed port ownership, health/status, local/network URLs, logs/status commands, and service-level action buttons for `Start`, `Stop`, `Restart`, `Open`, and `Rename`. Owner/local/remote context is shown as badges; the primary action follows whether the service is running, not where it lives. Service rows support local star pinning and toolbar sort modes, with pinned services kept first. `Open` uses a quick backend URL-open path for running services whose displayed URL is already reachable, so the system browser opens without re-querying SSH peers; on-demand starts, remote owner starts, and missing SSH local forwards still go through Bridgeboard's managed action path. External service status prefers the current fixed-port listener over stale handoff PIDs, so Windows wrapper processes do not hide the real service state. It refreshes on startup, manual refresh, window focus/visibility return, and after actions; it does not poll peer machines every few seconds. Peer registry fetches are run in parallel so a slow SSH peer does not block all other peer results serially. The top toolbar also includes one `Copy <machine>` handoff prompt button for the local machine and each configured peer, such as `Copy workstation` and `Copy gpu-box`.

Fallback tools remain available:

```bash
bridgeboard-ui
bridgeboard tray
python3 scripts/bridgeboard-tray.py
```

Set `BRIDGEBOARD_BIN=/path/to/bridgeboard` if the CLI binary is not on `PATH`. The packaged installers register `bridgeboard-tray` or `bridgeboard-tray.exe` for login startup by default.

## Runtime Files

Default paths:

- config: `~/.config/bridgeboard/config.yaml`
- registry: `~/.local/share/bridgeboard/registry.json`
- state: `~/.local/state/bridgeboard/state.json`

Windows defaults follow the platform config/data directories:

- config: `%APPDATA%\bridgeboard\config.yaml`
- registry: `%APPDATA%\bridgeboard\registry.json`
- state: `%LOCALAPPDATA%\bridgeboard\state.json`

Service PID and log files are project-relative, controlled by each `portal-bridge.yaml`.

## License

Bridgeboard is released under the MIT License. See `LICENSE` for details.
