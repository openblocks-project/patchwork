# Changelog

## Unreleased — MCP Tier 0

Reduces friction for LLM-driven workflows. Three changes that, together, eliminate the most common silent-failure modes when building graphs over MCP.

### Added

- **`describe_node_type(name)`** — full schema for one node: `inputs`/`outputs` as `[{index, name, kind}]` and `properties` as `{key: {type, default}}`. Drill in after `list_node_types` narrows down a candidate; avoids paying for the verbose schema for every type.
- **`list_node_types` filters** — new optional params `category`, `name_filter`, `full`. Default behavior is now summary mode (see soft-break below).
- **MidiIn / Serial auto-reconcile** — setting `port_name` (and `baud_rate` for Serial) via `update_node` now binds the device on the next render frame. Mirrors the existing AudioDevice (`enabled`) and OscIn (`listening`) pattern. No separate trigger required.
- **MidiIn / Serial trigger actions** — `trigger_node` now accepts `connect` / `disconnect` for these node types. `disconnect` clears `port_name` so the reconcile loop tears down the binding.
- **`apply_properties` diagnostics** — `create_node`, `update_node`, and `create_workflow` now return per-key feedback:
  - `applied` — keys honored
  - `ignored_unknown` — keys not in the node's schema (likely typos)
  - `readonly_rejected` — writes to runtime/auto-managed fields (status, response, log, etc.)
  - `errors` — deserialize failures (e.g. wrong type for the field)
  - `create_workflow` aggregates these per-node under `property_warnings` and reports out-of-range connections under `connection_errors`.

### Changed (soft-break)

- **`list_node_types` default response is now compact**. Each entry is `{name, category, in_count, out_count, wip}` — typically <5KB total. Pass `{"full": true}` to get the legacy verbose schema (ports + properties for every node).

  *Migration:* clients that previously read `inputs`/`outputs`/`properties` directly from `list_node_types` should either pass `full: true` or switch to `describe_node_type` for the specific node they care about.

- **`update_node` / `create_node` response shape gains diagnostic fields**. The previous `{success: true}` is preserved for backward compatibility, but new fields (`applied`, `ignored_unknown`, `readonly_rejected`, `errors`) appear when relevant.

  *Heads up:* writes to known runtime fields (`status`, `response`, `log`, `last_hash`, etc.) now return `success: true` with the key in `readonly_rejected` instead of being silently dropped into the state. The runtime field will not be modified.

### Tool-description updates

- All affected tool descriptions now mention the auto-reconcile behavior so clients learn from `tools/list` rather than from source.
- `trigger_node` description enumerates supported actions per node type instead of a flat list.
