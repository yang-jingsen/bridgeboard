# TethysUNE Bridgeboard Contract

This note is the Bridgeboard-side integration contract for TethysUNE. It is
deliberately reversible: TethysUNE should consume Bridgeboard service records
and prepared URLs, not move or rewrite legacy handoff data during validation.

## Open Preparation

Use `prepare-open` when TethysUNE wants to open a service inside its native web
workspace:

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

Use `--target external` only when a shell wants Bridgeboard preparation but
will still open the returned URL outside the embedded workspace.

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
  "actions": ["local tunnel 24321 -> eva-02:24321 pid 2193991"],
  "warnings": []
}
```

`source_config_path` is the owner/source Bridgeboard YAML path. TethysUNE may
display it or log it for audit, but it should not mutate that file directly.

## Denia Migration

Current real service:

- id: `denia-score-annotator`
- title: `Denia Score Annotator`
- owner/source: `eva-02`
- owner port: `24321`
- owner URL: `http://127.0.0.1:24321/`
- owner config: `C:\Users\senxiu\AppData\Roaming\bridgeboard\handoffs\denia-score-annotator.yaml`
- owner registry: `C:\Users\senxiu\AppData\Roaming\bridgeboard\registry.json`

How to identify it from tethys:

```bash
bridgeboard ports --json --peers
bridgeboard status denia-score-annotator --json --peers
bridgeboard prepare-open \
  --id denia-score-annotator \
  --owner-host eva-02 \
  --source-machine eva-02 \
  --local-port 24321 \
  --target internal
```

How to identify it on eva-02:

```powershell
bridgeboard status denia-score-annotator --json
Get-Content "$env:APPDATA\bridgeboard\handoffs\denia-score-annotator.yaml"
```

Expected owner-side handoff shape:

```yaml
schema: portal-bridge.v1
id: denia-score-annotator
title: Denia Score Annotator
owner_host: eva-02
port: 24321
service:
  mode: external
  lifecycle:
    startup: manual
    restart: never
  pid_source: port:24321
  pid_port: 24321
  health_url: http://127.0.0.1:24321/
tunnel:
  modes: [local_forward]
  bind_host: 127.0.0.1
local_url: http://127.0.0.1:24321/
open_url: http://127.0.0.1:24321/
```

Migration path:

1. Keep the eva-02 handoff YAML and registry entry unchanged.
2. In TethysUNE, list services from `bridgeboard ports --json --peers`.
3. When the Denia tile is opened, call `prepare-open` with the exact row
   identity: id, owner_host, source_machine, and local_port when present.
4. Load the returned `url` inside the embedded web workspace.
5. Store only TethysUNE UI state separately, such as tabs, pins, recent opens,
   and display grouping.
6. Validate Denia opens from tethys and from eva-02 before removing any legacy
   UI shortcut.

Rollback path:

1. Stop using the TethysUNE embedded opener for the service.
2. Continue using `bridgeboard open denia-score-annotator` or the legacy
   Bridgeboard dashboard/tray entry.
3. Leave `denia-score-annotator.yaml` and the registry entry in place; no data
   migration is required to roll back.

Out of scope for this migration:

- Moving Denia files into TethysUNE.
- Rewriting the Denia handoff YAML.
- Integrating cutex-manager or Waveline runtime controls.
- Changing the legacy Bridgeboard dashboard UI.
