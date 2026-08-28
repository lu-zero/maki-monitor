# maki-monitor

Long-running command supervision for [maki](https://github.com/tontinton/maki).

`bash` kills the command when its timeout hits (default 120s). For a test
suite, a build, or a server that should keep running after the tool call
returns, use `monitor`.

```
monitor(command)     start it, get log paths back
                     keep working

exit observation     mailbox message with exit, paths, short tail
                     TUI starts a turn if the session is idle

monitor_wait         only when you have nothing else to do
monitor_peek         look now, do not wait
read stdout/stderr   post-mortem, not cat-through-bash
monitor_stop         kill the process group (safe after exit)
```

You will be notified when it exits, including on success (`notify_on_success =
false` hears only about failures). Do not `sleep` and do not poll.

## Install

```
maki.pack.add({ src = "https://github.com/lu-zero/maki-monitor" })
```

Or place a checkout under `<data>/site/pack/<group>/maki-monitor/` by hand.

## How it works

Built on the same session-owned jobs any plugin can use:
`monitor(command)` starts a job that survives a plugin reload and outlives
the tool call, and streams its stdout/stderr into a directory as they arrive:

```
{logs}/maki/{session}/monitor-{id}/
  meta.json      command, pid, times, exit
  stdout.log     raw stdout
  stderr.log     raw stderr
```

`peek` and the exit observation keep a short tail in memory (the host tracks
this regardless of the plugin, so it survives a reload too). The files are
the source of truth for anything past that tail. On session end the host
kills the process group. Log files stay so a headless caller can still
collect stdout, stderr, and meta after the session returns.

## Tools

- `monitor` — spawn a supervised command (`command`, `cwd`, `description`,
  `session`, `wake`, `notify_on_success`, `tail`)
- `monitor_stop` — kill one by id (safe after exit)
- `monitor_list` — this session's live and recently-exited monitors
- `monitor_peek` — tails, exit status, and log paths without waiting
- `monitor_wait` — block until exit or timeout (`timeout_ms`, default 30s,
  max 600s)

## Development

Tests load the plugin through the real `maki-lua` host:

```
just test
```

The dev-dependencies point at a sibling `../maki` checkout; point them at a
git revision instead when testing a release.
