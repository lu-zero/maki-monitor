maki-monitor is a standalone maki plugin that supervises long-running commands: monitor, monitor_peek, monitor_wait, monitor_list, monitor_stop. It loads through the maki pack system, not as a builtin.

## Code guidelines

- Lua in `plugin/`. No trivial comments, minimal bloat, no unnecessary state.
- Keep the `MAX_*` constants at the top of the file.
- Fallible runtime operations return the `(value, err)` pair and never throw. Tool handlers fail with `{ llm_output = msg, is_error = true }`; a plain string is always success.
- Every tool that declares `permission_scopes` must also declare `permission`, and the capability must be granted in `plugin.toml`. maki refuses the load otherwise.
- `plugin.toml` grants exactly what the code calls. maki walks bundled plugins' lua and requires to keep manifests honest; keep ours aligned by hand.
- The handler and the `on_exit` callback share the `meta` table, so the exit write keeps `pid` and `started`. Do not replace it with a fresh table.
- Log appends retry after `mkdir`: a stdout line can be delivered before the handler's own `mkdir` lands.

## Testing

Cheapest first:

- `just check`
- `just lint`
- `just test`

Tests live in `tests/monitor.rs` and load the plugin through the real `maki-lua` host with `PluginHost::load_package`, passing the repo root (it derives `plugin/` itself; passing `plugin/` fails with `PackageEmpty`).

Dev-dependencies pin a maki revision. Move the pin when the host changes what the plugin uses; re-point at `tontinton/maki` once the relevant code lands there.

Assert Lua-visible effects (callback output, mailbox messages, meta contents), not just files. `smol::unblock` side effects land even when a callback aborts, so file-only checks can pass while the Lua-level API is broken.

## Layout

- `plugin/monitor.lua` — the whole plugin: path helpers, `write_meta`, the five `register_tool` specs, and the tool-usage prompt hint.
- `plugin.toml` — `min_maki_version` and the `[permissions]` request.
- `tests/monitor.rs` — host harness and the two integration tests.
- `justfile` — check, lint, test.

## Docs

The README is the canonical home for install and usage. Follow the maki docs voice: plain words, no em-dashes, no contractions, state facts once.
