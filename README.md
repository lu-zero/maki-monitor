# maki-monitor

Long-running command supervision for [maki](https://github.com/tontinton/maki).

The `bash` tool kills its command when the tool timeout hits, 120 seconds by
default. A test suite, a build, or a server often needs longer. `monitor`
starts the command outside that limit and tells the agent when it exits.

```
monitor(command)      start it, return at once with log paths
                      the agent keeps working

exit                  one observation: exit code, log paths, short tail
                      an idle TUI session starts a new turn on it

monitor_peek          look at output now, without waiting
monitor_wait          snapshot without blocking; the exit notifies on its own
read stdout/stderr    the full output, during or after the run
monitor_stop          kill the process group, safe after exit
```

## Install

Requires maki 0.4.12 or newer. Add this to `~/.config/maki/init.lua`:

```lua
maki.pack.add({
  { src = "https://github.com/lu-zero/maki-monitor", name = "monitor" },
})
```

On the next start maki clones the repository, pins the commit in
`pack-lock.json`, and asks twice: once before installing, once before granting
the permissions the plugin requests (`run`, `fs_read`, `fs_write`, `env`). To
move the pin later, set `version` in the declaration and restart.

A checkout placed by hand skips both prompts. Maki grants it exactly the
permissions its `plugin.toml` requests:

```
~/.local/share/maki/site/pack/local/start/monitor
```

A symlink to a working copy is fine. That is how this repository is developed.
See the packages page in the maki docs for the full story on `start` and `opt`
packages, approvals, and headless installs.

## Tools

| Tool | Does |
|---|---|
| `monitor` | Start a supervised command and return at once |
| `monitor_peek` | Recent stdout, stderr, status, and log paths |
| `monitor_wait` | Snapshot without blocking; the exit notifies on its own |
| `monitor_list` | Live and recently exited monitors of the session |
| `monitor_stop` | Kill one monitor by id |

### monitor

| Parameter | Type | Default | Meaning |
|---|---|---|---|
| `command` | string | required | Shell command to supervise |
| `cwd` | string | current dir | Working directory |
| `description` | string | none | Short label shown in the UI |
| `session` | string | calling session | Session that receives the exit observation |
| `wake` | boolean | `true` | Start a turn when the session is idle (TUI only) |
| `notify_on_success` | boolean | `true` | Set `false` to hear only about failures |
| `tail` | integer | 20 | Lines shown by peek/wait, read from the log files |

### monitor_wait

The call never blocks. It returns the same snapshot as `monitor_peek` and
says so when the monitor is still running. `timeout_ms` is accepted for
compatibility and ignored.

## Notifications

The exit observation reads `[job N] "command" exited with code C` and carries
the log paths and a short tail. With `wake = true` an idle TUI session starts
a turn on it. In a headless run the observation joins the session and the
next turn sees it.

Agents are told to keep working after starting a monitor. Waiting never
stalls the conversation: `monitor_wait` returns at once, and the exit
observation starts the next turn.

## Logs

```
~/.local/logs/maki/{session}/monitor-{stamp}-{id4}/
  meta.json      command, cwd, session, pid, started, exit_code, finished
  stdout.log     raw stdout, written by the job itself
  stderr.log     raw stderr, written by the job itself
```

The job writes its own streams to the files, so output lands as it arrives
and nothing is buffered in the plugin. `peek` and the exit observation read
a short tail from the files. The files are the source of truth for
everything past that tail, and they stay after the session ends.

## Reloads

A `/reload` drops the Lua callbacks of running monitors, not the monitors.
When the plugin loads it finds all of its monitors again, and a focus change
re-checks the session that gained focus; either path re-arms the exit
callback. A monitor that exited while the plugin was unloaded still gets its
meta written and its notification delivered.

## Permissions

Starting a monitor prompts for its command, the same way `bash` does.
`monitor_stop` prompts with the command it is about to kill. Peek, wait, and
list run without a prompt.

## Development

The tests load the plugin through the real `maki-lua` host:

```
just test
```

Dev-dependencies pin a revision of `lu-zero/maki`. Move the pin forward when
the host gains something the plugin wants, and re-point at `tontinton/maki`
once the extraction lands there.
