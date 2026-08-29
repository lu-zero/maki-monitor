maki-monitor is a standalone maki plugin that supervises long-running commands: monitor, monitor_peek, monitor_wait, monitor_list, monitor_stop. It loads through the maki pack system, not as a builtin.

## Code guidelines

- Lua in `plugin/`. No trivial comments, minimal bloat, no unnecessary state.
- Keep the `MAX_*` constants at the top of the file.
- Fallible runtime operations return the `(value, err)` pair and never throw. Tool handlers fail with `{ llm_output = msg, is_error = true }`; a plain string is always success.
- Every tool that declares `permission_scopes` must also declare `permission`, and the capability must be granted in `plugin.toml`. maki refuses the load otherwise.
- `plugin.toml` grants exactly what the code calls. maki walks bundled plugins' lua and requires to keep manifests honest; keep ours aligned by hand.
- Jobs write their own logs: the handler `mkdir`s the monitor directory before `jobstart`, which takes `stdout` and `stderr` file paths. Do not read output back through `on_stdout` callbacks.
- `meta.json` on disk is the source of truth: it carries `notify_on_success`, `wake`, and `tail`, and `on_exit` rewrites it wholesale. Adoption after a reload rebuilds the exit callback from it.
- Never call a UI roundtrip (`maki.session.current`, `maki.session.list`) at load time. The UI loop drains `UiAction` between frames only, so a load-time call waits forever. Adoption works around this by listing jobs without a session filter; each job snapshot carries its own session.
- Never block a tool on `jobwait`. A timed-out wait takes the job's event receiver and never gives it back: the exit callback is dead from then on. `monitor_wait` returns a snapshot at once and the exit notification does the waking.

## Testing

Cheapest first:

- `just check`
- `just lint`
- `just test`

Tests live in `tests/monitor.rs` and load the plugin through the real `maki-lua` host with `PluginHost::load_package`, passing the repo root (it derives `plugin/` itself; passing `plugin/` fails with `PackageEmpty`).

Dev-dependencies pin a maki revision. Move the pin when the host changes what the plugin uses; re-point at `tontinton/maki` once the relevant code lands there.

Assert Lua-visible effects (callback output, mailbox messages, meta contents), not just files. `smol::unblock` side effects land even when a callback aborts, so file-only checks can pass while the Lua-level API is broken.

## Layout

- `plugin/monitor.lua` — the whole plugin: path helpers, meta read and write, `adopt` (re-arms exit callbacks after a reload or a focus change through `jobattach`, finalizes jobs that exited while unloaded), the five `register_tool` specs, and the tool-usage prompt hint.
- `plugin.toml` — `min_maki_version` and the `[permissions]` request.
- `tests/monitor.rs` — host harness and the four integration tests.
- `justfile` — check, lint, test, fmt-lua.

## Docs

The README is the canonical home for install and usage. Follow the maki docs voice: plain words, no em-dashes, no contractions, state facts once.
