# Bridgeboard Agent Service Handoff Protocol

This document is for agents that need to start or record a local web service for the user. Do not invent ad hoc ports, hidden background commands, or one-off handoff notes. Use Bridgeboard.

## Goal / 目标

Register every background web service in Bridgeboard so the user and later agents can see:

- what is running
- which host owns it
- which fixed port it uses
- how to open it
- how to check health
- how to stop or restart it safely
- where logs are stored

## Port Policy / 端口规则

Use fixed `24xxx` ports only.

- `24000`: Bridgeboard itself.
- `24001-24299`: project-specific portals.
- `24300-24499`: project tools.
- `24500-24699`: agent/protocol/automation services.
- `24700-24899`: ad hoc temporary tunnels.
- `24900-24999`: diagnostics/manual override/emergency.

Before choosing a port:

```bash
bridgeboard ports --peers
bridgeboard doctor
```

Do not reuse an occupied port unless it is the exact same service id.

## Preferred Agent Flow / 推荐流程

1. Pick a service id:

Use lowercase kebab-case, for example:

```text
model-inspector
```

2. Pick a fixed port from the correct range.

3. Start the service through Bridgeboard when possible.

4. Register handoff metadata immediately.

5. Verify with:

```bash
bridgeboard status <service-id> --json
bridgeboard doctor
```

## Windows Stable Start

When starting a Windows service over SSH, do not rely on plain `Start-Process`, `cmd /c start`, or shell backgrounding. SSH-owned background processes can die when the SSH session ends.

Use Bridgeboard's Scheduled Task detach mode:

```powershell
bridgeboard.exe handoff `
  --id model-inspector `
  --title "Model Inspector" `
  --owner-host gpu-box `
  --port 24201 `
  --cwd "D:\Projects\model-inspector" `
  --start-command "npm run dev -- --host 127.0.0.1 --port 24201" `
  --detach scheduled-task `
  --task-name Bridgeboard-model-inspector `
  --pid-from-port `
  --health-url http://127.0.0.1:24201/ `
  --require-healthy `
  --local-url http://127.0.0.1:24201/ `
  --network-url http://100.x.y.z:24201/ `
  --tunnel-mode local_forward `
  --log-file "$env:APPDATA\bridgeboard\handoffs\model-inspector.log" `
  --note "started by agent via Bridgeboard"
```

Bridgeboard will create and run a Windows Scheduled Task, write a `.cmd` wrapper, redirect logs, discover the real listener PID from the port, and record the handoff YAML. Peer access should normally go through SSH `local_forward`: if `gpu-box` owns port `24201`, `workstation` opens `127.0.0.1:24201` and Bridgeboard maintains the SSH tunnel to the owner host's `127.0.0.1:24201`.

## Linux Stable Start

For Linux managed services, prefer a project `portal-bridge.yaml` and:

```bash
bridgeboard register /path/to/project/portal-bridge.yaml
bridgeboard up <service-id>
```

For a quick external service that is already running:

```bash
bridgeboard handoff \
  --id my-service \
  --title "My Service" \
  --owner-host workstation \
  --port 24510 \
  --cwd /path/to/project \
  --pid-from-port \
  --health-url http://127.0.0.1:24510/ \
  --require-healthy \
  --local-url http://127.0.0.1:24510/ \
  --log-file /path/to/project/.bridgeboard/server.log \
  --note "started by agent"
```

If the service must be restarted later, include `--start-command`, `--stop-command`, and `--restart-command` when practical.

## Required Metadata / 必填或强烈建议字段

Always provide:

- `--id`
- `--title`
- `--owner-host`
- `--port`
- `--cwd`
- `--health-url`
- `--local-url`
- `--log-file`
- `--pid-from-port`
- `--require-healthy`

For Windows SSH-started services, also provide:

- `--start-command`
- `--detach scheduled-task`
- `--task-name Bridgeboard-<service-id>`

For remotely useful services, also provide:

- `--network-url`, usually a VPN or LAN URL when one is intentionally exposed.
- `--tunnel-mode local_forward`, unless the service must be owner-local only.

Bridgeboard can enforce these defaults from user config:

```yaml
defaults:
  handoff_tunnel_modes: [local_forward]
  assume_local_forward_for_peers: true
```

With this config, agents using `bridgeboard handoff` do not need to repeat `--tunnel-mode local_forward`, and the local operator can still SSH-forward peer records whose YAML forgot to declare `local_forward`. Use `--no-tunnel` only for services that should be owner-local/reserved.

## Reading Status / 给后续 agent 读取

Do not parse YAML unless necessary. Prefer:

```bash
bridgeboard status <service-id> --json
bridgeboard ports --json --peers
bridgeboard logs <service-id> --lines 120
```

The JSON status includes service mode, lifecycle, URLs, health/status text, PID source, PID port, and task name.

To change the user-facing title without changing the stable service id or port:

```bash
bridgeboard rename <service-id> --title "New Display Name"
```

If the service is owned by a configured peer, run the same command from the local host; Bridgeboard forwards the rename to the owner over SSH.

## Remote Owner Control / 远端 owner 控制

If you are on a non-owner host and the service is owned by a configured peer, do not SSH by hand. Use:

```bash
bridgeboard remote-up <service-id>
bridgeboard remote-down <service-id>
bridgeboard remote-restart <service-id>
```

These ask the owner host to run `bridgeboard up/down/restart <service-id>` through the configured SSH peer entry. `remote-up` also restores the local SSH tunnel when `local_forward` is enabled; `remote-down` stops the owner service and then removes the local tunnel. These commands are intentionally separate from `bridgeboard open`: opening a page should not silently start or stop a heavy remote service.

For Windows Scheduled Task handoffs, `remote-down`/`stop` first ends the task and then kills the recorded PID or the current process listening on the configured fixed port when Bridgeboard can identify it. This handles the common case where ending the task leaves a Node/Vite child process alive.

For opening a peer service:

- If `local_forward` is enabled, `bridgeboard open <service-id>` starts the local tunnel and opens the local URL.
- Otherwise Bridgeboard opens `network_url` when available. Prefer enabling `local_forward` over relying on direct VPN/LAN HTTP, because services often bind only to `127.0.0.1` on the owner host.
- If neither exists, it falls back to the exported `open_url`.

## Stop / Restart

Use Bridgeboard first:

```bash
bridgeboard stop <service-id>
bridgeboard restart <service-id>
```

If Bridgeboard says the service is record-only and no stop/restart command exists, ask the user before killing unrelated processes.

## Do Not / 禁止

- Do not choose random non-24xxx ports.
- Do not leave an unrecorded background web server.
- Do not rely on wrapper PIDs when `--pid-from-port` is available.
- Do not silently record an unhealthy service as successful.
- Do not overwrite another service's port.
- Do not mutate unrelated Bridgeboard handoff files.

## User-Facing Report Template

After deployment, report:

```text
Service id:
Title:
Host:
Port:
Local URL:
Network URL:
Health:
PID:
Log:
Bridgeboard status command:
```
