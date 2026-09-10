local MAX_WAIT_MS = 600000
local MAX_TAIL_BYTES = 262144
local WAIT_TIMEOUT_ERR = "monitor_wait: timeout_ms must be an integer of at least 1"

local description = [[Spawn a long-running command you own (a test suite, build, or server).
The command keeps running after this tool returns. You will be notified in this
session when it exits, including on success. A new turn starts when the session
is idle unless you set `wake = false`.

Do not `sleep` or poll. Keep working; the exit observation includes log paths
and a short tail. `monitor_peek` reads the current output on demand and
never parks. `monitor_wait` always parks this turn until the monitor exits
or its timeout passes. Use `read` on the log paths for a post-mortem. Do
not `cat` them through bash.

Logs are always written to a per-monitor directory (stdout.log, stderr.log,
meta.json). `monitor_list` shows this session's live and recently-exited
monitors; `monitor_stop` kills one (safe after exit).

The monitor and its exit notification survive plugin reloads.]]

local function parse_session(input, ctx)
  if input.session and input.session ~= "" then
    return input.session
  end
  local id, err = ctx:session_id()
  if not id then
    return nil, err or "no session"
  end
  return id
end

local function session_dir(session)
  local root = maki.env.logs_dir()
  if not root then
    return nil
  end
  return maki.fs.joinpath(root, session)
end

local function file_size(path)
  if not path then
    return nil
  end
  local meta = maki.fs.metadata(path)
  return meta and meta.size
end

local function write_json(path, fields)
  local encoded = maki.json.encode(fields)
  if encoded then
    maki.fs.atomic_write(path, encoded)
  end
end

local function read_json(path)
  local text = maki.fs.read(path)
  if not text then
    return nil
  end
  return maki.json.decode(text)
end

local function next_dir(session)
  local sdir = session_dir(session)
  if not sdir then
    return nil
  end
  for _ = 1, 10 do
    local dir = maki.fs.joinpath(sdir, string.format("monitor-%d-%04x", os.time(), math.random(0, 65535)))
    if not maki.fs.metadata(dir) then
      return dir
    end
  end
  return nil
end

-- meta.json is the source of truth: a scan answers for every monitor this
-- plugin started, including ones the host has already reaped, and never
-- reports unrelated session jobs the way a joblist would.
local function scan_monitors(session)
  local sdir = session_dir(session)
  if not sdir then
    return nil
  end
  local entries = maki.fs.dir(sdir, { depth = 2 })
  if not entries then
    return nil
  end
  local out = {}
  for _, entry in ipairs(entries) do
    local rel = entry[1]
    if entry[2] == "file" and rel:match("meta%.json$") then
      local meta = read_json(maki.fs.joinpath(sdir, rel))
      if meta and meta.id then
        out[meta.id] = {
          dir = maki.fs.joinpath(sdir, rel:match("^(.*)/meta%.json$")),
          meta = meta,
        }
      end
    end
  end
  return out
end

local function find_monitor(session, id)
  local found = scan_monitors(session)
  if not found or not found[id] then
    return nil
  end
  return found[id].dir, found[id].meta
end

-- A spawned job writes its own logs, so the exit path only records the outcome
-- and notifies. The same closure re-arms after a reload via jobattach.
local function on_exit_for(session, dir, meta)
  return function(job_id, code)
    meta.id = job_id
    meta.exit_code = code
    meta.finished = os.time()
    write_json(maki.fs.joinpath(dir, "meta.json"), meta)
    if meta.notify_on_success == false and code == 0 then
      return
    end
    maki.session.notify(
      string.format('[job %d] "%s" exited with code %d', job_id, meta.command, code),
      { session = session, wake = meta.wake ~= false }
    )
  end
end

local function read_tail(path, lines)
  if not lines or lines < 1 then
    return nil
  end
  local size = file_size(path)
  if not size or size == 0 then
    return nil
  end
  -- Ask the host for the last window only; hosts without a windowed read
  -- answer with the whole file, which still tails correctly.
  local text = maki.fs.read(path, size > MAX_TAIL_BYTES and { offset = -MAX_TAIL_BYTES } or nil)
  if not text or text == "" then
    return nil
  end
  if #text < size then
    text = text:gsub("^[^\n]*\n", "")
  end
  local all = {}
  for line in text:gmatch("([^\n]*)\n") do
    all[#all + 1] = line
  end
  if text:sub(-1) ~= "\n" then
    all[#all + 1] = text:match("([^\n]+)$")
  end
  local out = {}
  for i = math.max(1, #all - lines + 1), #all do
    out[#out + 1] = all[i]
  end
  return table.concat(out, "\n")
end

local function format_paths(dir)
  local out_path = maki.fs.joinpath(dir, "stdout.log")
  local err_path = maki.fs.joinpath(dir, "stderr.log")
  return string.format("\nstdout: %s (%s bytes)", out_path, file_size(out_path) or 0)
    .. string.format("\nstderr: %s (%s bytes)", err_path, file_size(err_path) or 0)
    .. "\nmeta: "
    .. maki.fs.joinpath(dir, "meta.json")
end

local function format_tails(dir, meta)
  if not meta then
    return ""
  end
  local lines = meta.tail or 20
  local text = ""
  local out = read_tail(maki.fs.joinpath(dir, "stdout.log"), lines)
  if out then
    text = text .. "\n--- stdout tail ---\n" .. out
  end
  local err = read_tail(maki.fs.joinpath(dir, "stderr.log"), lines)
  if err then
    text = text .. "\n--- stderr tail ---\n" .. err
  end
  return text
end

local function format_snapshot(info, dir, meta)
  local header
  if info.status == "running" then
    header = string.format("monitor %d  [%ds]  %s  (pid %d)", info.id, info.elapsed_secs, info.command, info.pid)
  else
    header = string.format(
      "monitor %d exited with code %d after %ds: %s",
      info.id,
      info.exit_code,
      info.elapsed_secs,
      info.command
    )
  end
  if not dir then
    return header .. "\n(no output captured)"
  end
  local text = header .. format_paths(dir) .. format_tails(dir, meta)
  if text == header then
    return header .. "\n(no output captured)"
  end
  return text
end

-- Reload drops the Lua exit callbacks, not the monitors. Adoption walks the
-- plugin's job list: running jobs get their callback re-armed and jobs that
-- exited while unloaded still get their meta written and their notification
-- delivered. A UI roundtrip like maki.session.current() would wait forever at
-- load time, so the list is taken without a session filter and each job
-- carries its own session.
local function adopt(session)
  local jobs = maki.fn.joblist(session)
  for _, job in ipairs(jobs or {}) do
    local owner = session or job.session
    if owner then
      local dir, meta = find_monitor(owner, job.id)
      if dir then
        if job.status == "running" then
          maki.fn.jobattach(job.id, { on_exit = on_exit_for(owner, dir, meta) })
        elseif job.status == "exited" and not meta.exit_code then
          on_exit_for(owner, dir, meta)(job.id, job.exit_code)
        end
      end
    end
  end
end

adopt()

maki.api.register_prompt_hint({
  slot = "tool_usage",
  content = "- Use monitor for a test, build, or server that should outlive the tool call. You will be notified when it exits. Do not bash-sleep or poll; keep working or ask the user questions. monitor_peek reads current output, and monitor_wait parks the turn until exit or its timeout.",
})

maki.api.register_tool({
  name = "monitor",
  kind = "execute",
  description = description,
  schema = {
    type = "object",
    properties = {
      command = {
        type = "string",
        description = "The bash command to supervise (runs detached, outlives this call)",
        required = true,
      },
      cwd = { type = "string", description = "Working directory (default: cwd)" },
      description = { type = "string", description = "Short description (3-5 words) of what the command does" },
      session = {
        type = "string",
        description = "Session id to notify on exit. Defaults to the current session.",
      },
      wake = {
        type = "boolean",
        description = "Start a session turn when the process exits and the session is idle (default true). TUI only.",
      },
      notify_on_success = {
        type = "boolean",
        description = "Notify on every exit, including success (default true). Set false to hear only about failures.",
      },
      tail = {
        type = "integer",
        description = "Trailing lines per stream shown by peek/wait, read from the log files (default 20, 0 disables)",
      },
    },
  },
  permission = "run",
  permission_scopes = function(input)
    local command = input.command
    if not command or command:match("^%s*$") then
      return nil
    end
    return { scopes = { command }, force_prompt = true }
  end,
  header = function(input)
    local s = input.description or input.command
    local buf = maki.ui.buf()
    buf:line({ { "monitor ", "dim" }, { s } })
    return buf
  end,
  handler = function(input, ctx)
    if not input.command or input.command:match("^%s*$") then
      return { llm_output = "error: command is required", is_error = true }
    end

    local session, err = parse_session(input, ctx)
    if not session then
      return { llm_output = "error: " .. (err or "no session"), is_error = true }
    end

    local dir = next_dir(session)
    if not dir then
      return { llm_output = "error: could not create a monitor directory", is_error = true }
    end
    maki.fs.mkdir(dir, { parents = true })

    local meta = {
      command = input.command,
      cwd = input.cwd,
      session = session,
      notify_on_success = input.notify_on_success,
      wake = input.wake,
      tail = input.tail,
    }

    local ok, id_or_err = pcall(maki.fn.jobstart, input.command, {
      scope = { session = session },
      cwd = input.cwd,
      stdout = maki.fs.joinpath(dir, "stdout.log"),
      stderr = maki.fs.joinpath(dir, "stderr.log"),
      on_exit = on_exit_for(session, dir, meta),
    })
    if not ok then
      return { llm_output = "error: " .. tostring(id_or_err), is_error = true }
    end
    local id = id_or_err

    local info = maki.fn.jobinfo(id)
    meta.id = id
    meta.pid = info and info.pid
    meta.started = os.time()
    write_json(maki.fs.joinpath(dir, "meta.json"), meta)

    return "monitor "
      .. id
      .. " started: "
      .. input.command
      .. "\nstdout: "
      .. maki.fs.joinpath(dir, "stdout.log")
      .. "\nstderr: "
      .. maki.fs.joinpath(dir, "stderr.log")
      .. "\nmeta: "
      .. maki.fs.joinpath(dir, "meta.json")
      .. "\nYou will be notified when it exits. Do not sleep or poll. monitor_peek reads current output; monitor_wait parks the turn until exit or its timeout. Read the log files for the full output."
  end,
})

maki.api.register_tool({
  name = "monitor_stop",
  kind = "execute",
  description = [[Kill a running monitor and its process group.

Pass the monitor id returned by `monitor`. Safe to call after the process has
already exited.]],
  schema = {
    type = "object",
    properties = {
      id = { type = "integer", description = "Monitor id returned by `monitor`", required = true },
    },
  },
  permission = "run",
  permission_scopes = function(input)
    if input.id then
      local info = maki.fn.jobinfo(input.id)
      if info and info.command then
        return { scopes = { info.command }, force_prompt = true }
      end
    end
    return { scopes = { "monitor_stop" }, force_prompt = false }
  end,
  handler = function(input)
    local info = maki.fn.jobinfo(input.id)
    if not info then
      return { llm_output = "error: monitor not found", is_error = true }
    end
    local ok, err = pcall(maki.fn.jobstop, input.id)
    if not ok then
      return { llm_output = "error: " .. tostring(err), is_error = true }
    end
    return "monitor " .. input.id .. " stopped"
  end,
})

maki.api.register_tool({
  name = "monitor_list",
  kind = "execute",
  description = [[List the monitors this plugin started in this session, live and recently-exited.

Returns each monitor's id, status, exit code, and how long it ran. Other jobs
of the session are not monitors and are not listed.]],
  schema = {
    type = "object",
    properties = {
      session = {
        type = "string",
        description = "Session id to list. Defaults to the current session.",
      },
    },
  },
  permission = "run",
  permission_scopes = function()
    return { scopes = { "monitor_list" }, force_prompt = false }
  end,
  handler = function(input, ctx)
    local session, err = parse_session(input or {}, ctx)
    if not session then
      return { llm_output = "error: " .. (err or "no session"), is_error = true }
    end
    local monitors = scan_monitors(session)
    if not monitors or next(monitors) == nil then
      return "no monitors"
    end
    local ids = {}
    for id in pairs(monitors) do
      ids[#ids + 1] = id
    end
    table.sort(ids)
    local lines = {}
    for _, id in ipairs(ids) do
      local meta = monitors[id].meta
      local info = maki.fn.jobinfo(id)
      if info and info.status == "running" then
        lines[#lines + 1] =
          string.format("  %d  [%ds]  %s  (pid %s)", id, info.elapsed_secs, meta.command or "?", info.pid or "?")
      else
        local elapsed = info and info.elapsed_secs
        if not elapsed and meta.started and meta.finished then
          elapsed = meta.finished - meta.started
        end
        lines[#lines + 1] = string.format(
          "  %d  exited %s after %ss  %s",
          id,
          info and info.exit_code or meta.exit_code or "?",
          elapsed or "?",
          meta.command or "?"
        )
      end
    end
    return "monitors:\n" .. table.concat(lines, "\n")
  end,
})

maki.api.register_tool({
  name = "monitor_peek",
  kind = "execute",
  description = [[Read a monitor's recent stdout and stderr without waiting for it to exit.

Finished monitors still answer: exit status, tails, and log paths are reported
instead of "not found". For the full output, `read` the log paths.]],
  schema = {
    type = "object",
    properties = {
      id = { type = "integer", description = "Monitor id returned by `monitor`", required = true },
    },
  },
  permission = "run",
  permission_scopes = function()
    return { scopes = { "monitor_peek" }, force_prompt = false }
  end,
  handler = function(input)
    local info = maki.fn.jobinfo(input.id)
    if not info or not info.session then
      return { llm_output = "error: not found", is_error = true }
    end
    local dir, meta = find_monitor(info.session, input.id)
    return format_snapshot(info, dir, meta)
  end,
})

maki.api.register_tool({
  name = "monitor_wait",
  kind = "execute",
  description = [[Park this turn until a monitor exits, or until the timeout passes.

Always blocks for up to timeout_ms. A monitor that exits in time reports its
exit code, log paths, and output tail; a timeout reports the current snapshot
instead and leaves the monitor intact, so you can wait again. The exit
notification reaches the session on its own either way. Use monitor_peek to
look without parking.]],
  schema = {
    type = "object",
    properties = {
      id = { type = "integer", description = "Monitor id returned by `monitor`", required = true },
      timeout_ms = {
        type = "integer",
        description = "How long to park: until exit or this cap, whichever comes first (max 600000).",
        required = true,
      },
    },
  },
  permission = "run",
  permission_scopes = function()
    return { scopes = { "monitor_wait" }, force_prompt = false }
  end,
  handler = function(input)
    local timeout_ms = input.timeout_ms
    if timeout_ms < 1 then
      return { llm_output = WAIT_TIMEOUT_ERR, is_error = true }
    end
    if timeout_ms > MAX_WAIT_MS then
      timeout_ms = MAX_WAIT_MS
    end

    local ok, result = pcall(maki.fn.jobwait, input.id, timeout_ms)
    if not ok then
      return { llm_output = "error: " .. tostring(result), is_error = true }
    end

    local info = maki.fn.jobinfo(input.id)
    if not result then
      if not info or not info.session then
        return { llm_output = "error: not found", is_error = true }
      end
      local dir, meta = find_monitor(info.session, input.id)
      return format_snapshot(info, dir, meta)
        .. "\nstill running. The exit will notify this session on its own; keep working or ask the user questions meanwhile."
    end

    if not info or not info.session then
      return string.format("monitor %d exited with code %d", input.id, result.exit_code)
    end
    local dir, meta = find_monitor(info.session, input.id)
    return format_snapshot(info, dir, meta)
  end,
})
