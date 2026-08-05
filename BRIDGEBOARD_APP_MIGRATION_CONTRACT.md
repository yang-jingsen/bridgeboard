# Bridgeboard App Migration Contract

This is the Bridgeboard-side contract for migrating Bridgeboard-managed service
control into an external app/plugin runtime boundary. TethysUNE is the current
first consumer of this contract, but Bridgeboard remains a separate legacy
service manager and protocol provider.

Scope correction:

- Migrate services currently managed or recorded by Bridgeboard through its
  registry, state, lifecycle, and tunnel model.
- Do not preserve standalone `bridgeboard serve`, `bridgeboard-tray`, or their
  autostart/systemd products as long-term runtime products after the migration
  is accepted.
- Do not stop live services during inventory or contract migration.
- Do not migrate Cutex-owned services. `cutex-agent-bus` and
  `cutex-management-api` are explicitly excluded; `cutex-desktop-notify` should
  also remain outside this migration unless the owner says otherwise.
- Keep legacy Bridgeboard project branding as `Bridgeboard`. External app hosts
  such as TethysUNE stay separate and may host a Bridgeboard App/plugin.

## Runtime Boundary

Bridgeboard remains the source of truth for service metadata until an external
runtime host has accepted the migrated runtime:

- registry entries and config paths
- service lifecycle policy
- fixed owner ports
- local/reverse tunnel metadata
- desired running state
- health/status snapshots
- safe start/stop/restart semantics

Runtime hosts should consume Bridgeboard through structured commands first.
They should not parse or mutate YAML/state files directly except for audited
backup or future one-time import tooling.

## Fast List Path

Use this for a Bridgeboard App/plugin service grid:

```bash
bridgeboard ports --json --peers --no-runtime
```

`--no-runtime` is the intended app-panel list path. It avoids Windows
per-service PID/health probes and keeps discovery inside the UI timeout. Local
rows whose runtime was skipped report:

```json
"runtime_status": "not-checked"
```

Use targeted calls after the user selects a service:

```bash
bridgeboard status <id> --json --peers
bridgeboard runtime-spec <id> --json
bridgeboard prepare-open --id <id> --owner-host <owner> --source-machine <source> --local-port <local-port> --target internal
```

Observed eva-02 latency after the no-runtime CLI update:

- `ports --json --no-runtime`: about 0.03s
- `ports --json --peers --no-runtime`: about 4.75s
- Plain `ports --json`: observed about 33s on eva-02 and is not the UI list
  SLA path.

## Open Preparation

Use `prepare-open` when a runtime host wants to open a service inside its
native web workspace:

```bash
bridgeboard prepare-open \
  --id <service-id> \
  --owner-host <owner-host> \
  --source-machine <source-machine> \
  --local-port <local-port-from-ports-json-if-present> \
  --target internal
```

The command starts an `on_demand` service when Bridgeboard owns the lifecycle,
starts or reuses an SSH `local_forward` for peer services, and prints JSON. It
does not call `webbrowser::open`.

Important result fields:

```json
{
  "target": "internal",
  "service_ref": {
    "id": "denia-score-annotator",
    "owner_host": "eva-02",
    "source_machine": "eva-02",
    "port": 24321
  },
  "source_config_path": "C:\\Users\\senxiu\\AppData\\Roaming\\bridgeboard\\handoffs\\denia-score-annotator.yaml",
  "title": "Denia Score Annotator",
  "url": "http://127.0.0.1:24321/",
  "origin": "http://127.0.0.1:24321",
  "local_machine_id": "tethys",
  "service_mode": "external",
  "tunnel_modes": "local",
  "startup_policy": "manual",
  "restart_policy": "never",
  "runtime_status": "peer-export",
  "direct_open": true,
  "local_port": 24321,
  "network_url": null,
  "actions": ["local tunnel 24321 -> eva-02:24321 pid 3751054"],
  "warnings": []
}
```

`source_config_path` is the owner/source Bridgeboard YAML path. Runtime hosts
may display it or log it for audit, but should not mutate that file directly.

## Action Mapping

Row identity must be preserved for every action:

- `id`
- `owner_host`
- `source_machine`
- `port`
- `local_port` when present

List:

```bash
bridgeboard ports --json --peers --no-runtime
```

Open embedded:

```bash
bridgeboard prepare-open --id <id> --owner-host <owner> --source-machine <source> --port <port> --local-port <local-port> --target internal
```

Read-only live observation:

```bash
bridgeboard observe --json --peers --timeout-sec <seconds>
```

This returns `schema: bridgeboard.observe.v1`. The command is side-effect free:
it does not start services, create tunnels, or write state. Peer observation is
batched per source/owner machine and bounded by the explicit timeout. Remote
service identity is `service_ref.id`, `owner_host`, `source_machine`, and owner
`port`. Nullable `local_port` sits beside the service ref because it is a
caller-local endpoint/forwarding parameter for opening or tunnel correlation,
not part of the remote service identity. App validators should treat missing
and null `local_port` as equivalent for row correlation, and should reject a
numeric mismatch against the App's current row.

Observation timing is bounded and deterministic. Local health probes run with a
fixed cap of 16 concurrent workers. Peer observation uses one batch command per
peer with an outer process timeout of `timeout_sec + 6s`, capped at 18 seconds,
so the App's current `--timeout-sec 2` path remains below a 20 second runner
ceiling while allowing SSH/process startup margin.

Status values:

- `healthy`: reachable HTTP 2xx/3xx and body expectations passed.
- `unhealthy`: reachable HTTP endpoint failed status/body expectations.
- `unreachable`: no usable listener or network path within timeout. A stopped
  remote service is represented here, usually as `connection-refused`.
- `unknown`: not observed, unsupported URL, peer observe unavailable, or schema
  mismatch.

Small schema excerpt:

```json
{
  "schema": "bridgeboard.observe.v1",
  "rows": [
    {
      "service_ref": {
        "id": "image-review-portal",
        "owner_host": "gpu-box",
        "source_machine": "gpu-box",
        "port": 24001
      },
      "local_port": 24660,
      "observation": {
        "status": "healthy",
        "reason": "http-ok",
        "observed_at": "unix:0"
      },
      "safe_open_actions": ["prepare-open"],
      "safe_lifecycle_actions": ["remote-up", "remote-down", "remote-restart"]
    }
  ],
  "warnings": []
}
```

Local managed launch-spec and desired-state:

```bash
bridgeboard runtime-spec --json
bridgeboard runtime-spec <id> --json
```

This is the stable structured interface for a managed Runtime Host cutover.
It returns only services where the current machine is the owner and
`service_mode` is `managed`. It includes `schema:
bridgeboard.runtime-spec.v1`, row identity, source config path, desired state,
current managed runtime status, `cwd`, argv `command`, resolved `pid_file`,
resolved `log_file`, health expectation, startup timeout, URLs, and tunnel
policy. Runtime hosts should use this instead of parsing `portal-bridge.yaml`.

Local owner lifecycle:

```bash
bridgeboard up <id>
bridgeboard stop <id>
bridgeboard restart <id>
```

Peer/local-forward lifecycle:

```bash
bridgeboard up --peer <source-machine> --local-port <local-port> <id>
```

Remote owner lifecycle for native app hosts:

```bash
bridgeboard remote-up <id> --owner-host <owner> --source-machine <source> --port <port> --local-port <local-port> --json
bridgeboard remote-down <id> --owner-host <owner> --source-machine <source> --port <port> --json
bridgeboard remote-restart <id> --owner-host <owner> --source-machine <source> --port <port> --local-port <local-port> --json
```

JSON lifecycle output uses `schema: bridgeboard.lifecycle-action.v1`, echoes the
verified `service_ref`, echoes nullable `local_port`, and returns `messages`
and `warnings` arrays. JSON lifecycle verifies remote service identity as
`id + owner_host + source_machine + port`. Echoed `local_port` is a local
forwarding/open parameter for caller correlation, not backend service identity;
id-only remote lifecycle remains a human compatibility path and should not be
used by strict app integrations. `remote-down` stops the owner service and then
stops all local Bridgeboard tunnels recorded for that service id and owner, not
only one local port.

External/manual records may be opened and tunneled, but lifecycle actions are
safe only when the record has explicit `start_command`, `stop_command`,
`restart_command`, or `task_name` metadata. Otherwise runtime hosts should
withhold Start/Stop/Restart or label them as owner/manual.

## Inventory

Snapshot source:

```bash
bridgeboard registry export --json --no-runtime
bridgeboard runtime-spec --json
ssh eva-02 'bridgeboard.exe registry export --json --no-runtime'
bridgeboard ports --json --peers --no-runtime
```

### Managed Lifecycle Services

These have `service_mode: managed` and should be migrated with lifecycle,
desired state, pid/state metadata, logs, and tunnel rules.

| id | owner/source | port | lifecycle | tunnel | config |
| --- | --- | ---: | --- | --- | --- |
| `ifm-rescue-portal` | `tethys` | 24021 | `on_demand/on_failure`, desired `running` | `local_forward` | `/home/senxiu/Projects/IFM/portal-bridge.yaml` |
| `scpolya-ui` | `tethys` | 24120 | `on_demand/on_failure`, desired `running` | reserved/local owner | `/home/senxiu/Projects/sc-polya-v2/portal-bridge.yaml` |
| `aria-console` | `tethys` | 24210 | `on_demand/on_failure`, desired `running` | reserved/local owner | `/home/senxiu/Projects/aria-console/portal-bridge.yaml` |
| `eva02-experiment-console` | `eva-02` | 24301 | `on_demand/on_failure`, desired `running` | `local_forward, reverse_forward` | `E:\Projects (Aemeath)\eva-02-image-service\portal-bridge.yaml` |
| `eva02-image2-results` | `eva-02` | 24308 | `on_demand/on_failure`, desired `running` | `local_forward, reverse_forward` | `E:\Projects (Aemeath)\eva-02-image-service\portal-bridge-image2-results.yaml` |

For tethys direct cutover, the immediate `runtime-spec --json` services are:

- `ifm-rescue-portal`
- `scpolya-ui`
- `aria-console`

The owner reported these are currently children of legacy `bridgeboard-tray`.
Do not stop them until the receiving runtime controller is ready to create
replacement Runtime Host sessions.

### Bridgeboard Handoff Records

These are `service_mode: external` registry records. Migrate their visibility,
fixed port, owner/source identity, URLs, tunnel behavior, pid metadata, and
safe lifecycle metadata when present.

| id | owner/source | port | lifecycle metadata | config |
| --- | --- | ---: | --- | --- |
| `heltia-web` | `eva-02` | 24085 | manual record, no tunnel in YAML but local default may apply | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\heltia-web.yaml` |
| `aemeath-hexmap-editor` | `eva-02` | 24201 | scheduled task `Bridgeboard-aemeath-hexmap-editor` | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\aemeath-hexmap-editor.yaml` |
| `wuthering-waves-bill` | `eva-02` | 24202 | scheduled task `Bridgeboard-wuthering-waves-bill` | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\wuthering-waves-bill.yaml` |
| `menghualu-remastered-portal` | `eva-02` | 24231 | manual external record | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\menghualu-remastered-portal.yaml` |
| `tokyo-dreams-portal` | `eva-02` | 24232 | manual external record | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\tokyo-dreams-portal.yaml` |
| `daniya-voice-audition` | `eva-02` | 24233 | manual external record | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\daniya-voice-audition.yaml` |
| `kks-voice-audition` | `eva-02` | 24234 | scheduled task `Bridgeboard-kks-voice-audition` | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\kks-voice-audition.yaml` |
| `denia-score-annotator` | `eva-02` | 24321 | manual external record, no start config | `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\denia-score-annotator.yaml` |

### Excluded Cutex-Owned Records

Do not migrate these into the Bridgeboard App service lifecycle unless the owner
explicitly changes scope:

- `cutex-agent-bus` on `tethys` and `eva-02`
- `cutex-management-api` on `tethys` and `eva-02`
- `cutex-desktop-notify` on `tethys`, treated as Cutex-owned by name and
  purpose

State-only entries not present in registry, such as eva-02 `goods-portal`, are
not migration sources unless re-registered.

## Denia Today

Current real service:

- id: `denia-score-annotator`
- title: `Denia Score Annotator`
- owner/source: `eva-02`
- owner port: `24321`
- owner URL: `http://127.0.0.1:24321/`
- owner config: `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\denia-score-annotator.yaml`
- owner registry: `C:\Users\senxiu\AppData\Roaming\bridgeboard\registry.json`
- service mode: `external`
- startup/restart: `manual/never`
- pid source: `port:24321`
- local tunnel from tethys: `denia-score-annotator:local:eva-02`, local port
  `24321`

Open from tethys:

```bash
bridgeboard prepare-open \
  --id denia-score-annotator \
  --owner-host eva-02 \
  --source-machine eva-02 \
  --local-port 24321 \
  --target internal
```

Open on eva-02:

```powershell
bridgeboard.exe prepare-open --id denia-score-annotator --owner-host eva-02 --source-machine eva-02 --local-port 24321 --target internal
```

Denia has no `start_command`, `stop_command`, `restart_command`, or
`task_name` today. A runtime host can migrate its registry visibility,
tunnel/open behavior, and status reads now, but should not claim it can start
the owner process after reboot until a later handoff update adds explicit start
metadata.

## Forward Migration

1. Keep all Bridgeboard registry/config/state files unchanged while validating.
2. Import/list through `ports --json --peers --no-runtime`.
3. For each row, preserve `id + owner_host + source_machine + port + local_port`.
4. Route opens through `prepare-open`.
5. Route lifecycle through Bridgeboard semantics only when the row mode and
   metadata prove it is safe.
6. Persist runtime-host UI state separately from Bridgeboard registry/state.
7. After managed-service migration is accepted, retire standalone
   `bridgeboard serve`, `bridgeboard-tray`, their autostart entries, and
   user-systemd units in a separate stop-and-remove stage.

## Rollback

Before standalone runtime retirement, rollback is simple:

1. Stop using the external Bridgeboard App runtime.
2. Continue using legacy `bridgeboard open`, `bridgeboard serve`, dashboard, or
   tray.
3. Leave registry/config/state files in place.
4. Restore CLI backups only if a binary regression is found.

Do not delete registry entries, handoff YAML, state files, autostart entries, or
systemd units until the owner accepts the migration and explicitly approves the
retirement stage.
