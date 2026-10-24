//! Loads the plugin through the real `maki-lua` host as a package, the way
//! maki itself would after `maki.pack.add`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use maki_agent::ToolOutput;
use maki_agent::tools::ToolRegistry;
use maki_lua::{ChatItemStatus, PluginHost, PluginPermissions, UiAction};
use serde_json::json;

const MAILBOX_WAIT: Duration = Duration::from_secs(15);

fn monitor_host() -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    host.load_package(
        "monitor",
        root,
        PluginPermissions::trusted(),
        Default::default(),
    )
    .unwrap();
    (reg, host)
}

fn exec_tool(reg: &ToolRegistry, name: &str, input: serde_json::Value) -> Result<String, String> {
    let entry = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool {name} not registered"));
    let inv = entry.tool.parse(&input).expect("parse failed");
    let ctx = maki_agent::tools::test_support::stub_ctx(&maki_agent::AgentMode::Build);
    smol::block_on(async { inv.execute(&ctx).await })
        .output
        .map_or_else(Err, |out| match out {
            ToolOutput::Plain(s) => Ok(s.text),
            other => panic!("unexpected output: {other:?}"),
        })
}

fn poll_until<T>(deadline_msg: &str, f: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + MAILBOX_WAIT;
    loop {
        if let Some(found) = f() {
            return found;
        }
        assert!(Instant::now() < deadline, "{deadline_msg}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn monitor_plugin_registers_five_tools() {
    let (reg, _host) = monitor_host();
    assert!(reg.has("monitor"), "monitor tool should register");
    assert!(reg.has("monitor_stop"), "monitor_stop tool should register");
    assert!(reg.has("monitor_list"), "monitor_list tool should register");
    assert!(reg.has("monitor_peek"), "monitor_peek tool should register");
    assert!(reg.has("monitor_wait"), "monitor_wait tool should register");
}

#[cfg(unix)]
#[test]
fn monitor_plugin_writes_logs_and_reports_after_exit() {
    let (reg, host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let mailbox = maki_agent::SessionMailbox::register(session);
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "echo hello; exit 0", "session": sid }),
    )
    .unwrap();
    assert!(started.contains("started:"), "got: {started}");

    let stdout_path = started
        .lines()
        .find_map(|l| l.strip_prefix("stdout: "))
        .unwrap_or_else(|| panic!("expected a stdout log path in: {started}"))
        .to_string();
    let id: u32 = started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"));

    poll_until("stdout.log never captured the command's output", || {
        std::fs::read_to_string(&stdout_path)
            .ok()
            .filter(|s| s.contains("hello"))
    });

    let peek = poll_until("monitor_peek never reported the exit", || {
        let peek = exec_tool(&reg, "monitor_peek", json!({ "id": id })).unwrap();
        peek.contains("exited with code 0").then_some(peek)
    });
    assert!(
        peek.contains("hello"),
        "peek should include the tail: {peek}"
    );

    let meta_path = started
        .lines()
        .find_map(|l| l.strip_prefix("meta: "))
        .unwrap_or_else(|| panic!("expected a meta path in: {started}"))
        .to_string();
    let meta = poll_until("on_exit never wrote meta.json with the exit code", || {
        let meta = std::fs::read_to_string(&meta_path).ok()?;
        meta.contains("\"exit_code\":0")
            .then_some(meta)
            .filter(|m| m.contains(&format!("\"id\":{id}")))
    });
    for field in ["\"command\"", "\"pid\"", "\"started\"", "\"finished\""] {
        assert!(
            meta.contains(field),
            "meta.json must keep {field} through the exit rewrite: {meta}"
        );
    }

    poll_until(
        "exit notification never reached the session mailbox",
        || {
            mailbox
                .drain()
                .iter()
                .any(|m| {
                    m.user_text()
                        .is_some_and(|t| t.contains(&format!("[job {id}]")))
                })
                .then_some(String::new())
        },
    );

    let list = exec_tool(&reg, "monitor_list", json!({ "session": sid })).unwrap();
    assert!(
        list.contains(&id.to_string()) && list.contains("exited"),
        "monitor_list should report the exited monitor, got: {list}"
    );

    host.event_handle()
        .end_session(session, maki_lua::SessionEndReason::Shutdown);
    assert!(
        std::path::Path::new(&stdout_path).exists(),
        "SessionEnd must keep monitor logs so callers can collect them after the session ends"
    );
}

#[cfg(unix)]
#[test]
fn exit_notification_survives_a_focus_change() {
    let (reg, host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let mailbox = maki_agent::SessionMailbox::register(session);
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 1; exit 0", "session": sid }),
    )
    .unwrap();
    let id: u32 = started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"));

    host.event_handle()
        .fire_autocmd("SessionFocusChanged", json!({ "session_id": sid }));

    poll_until(
        "exit notification never arrived after the focus change",
        || {
            mailbox
                .drain()
                .iter()
                .any(|m| {
                    m.user_text()
                        .is_some_and(|t| t.contains(&format!("[job {id}]")))
                })
                .then_some(String::new())
        },
    );
}

#[test]
fn monitor_wait_requires_a_positive_timeout() {
    let (reg, _host) = monitor_host();
    let entry = reg.get("monitor_wait").unwrap();

    let missing = entry.tool.parse(&json!({ "id": 1 }));
    assert!(
        missing.is_err(),
        "monitor_wait must require timeout_ms in its schema"
    );

    const ZERO_TIMEOUT_ERR: &str = "timeout_ms must be an integer of at least 1";
    let zero = exec_tool(&reg, "monitor_wait", json!({ "id": 1, "timeout_ms": 0 }));
    match zero {
        Ok(text) => assert!(
            text.contains(ZERO_TIMEOUT_ERR),
            "zero timeout should be rejected: {text}"
        ),
        Err(err) => assert!(
            err.contains(ZERO_TIMEOUT_ERR),
            "zero timeout should be rejected: {err}"
        ),
    }
}

#[cfg(unix)]
#[test]
fn monitor_wait_times_out_without_deafening_the_job() {
    let (reg, _host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let mailbox = maki_agent::SessionMailbox::register(session);
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 2; exit 0", "session": sid }),
    )
    .unwrap();
    let id: u32 = started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"));

    let began = Instant::now();
    let wait = exec_tool(&reg, "monitor_wait", json!({ "id": id, "timeout_ms": 300 })).unwrap();
    assert!(
        wait.contains("still running"),
        "a timed-out park should report the running monitor: {wait}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "park ran for {:?}; the 300ms timeout must cap it",
        began.elapsed()
    );

    poll_until(
        "exit notification never arrived after a timed-out park",
        || {
            mailbox
                .drain()
                .iter()
                .any(|m| {
                    m.user_text()
                        .is_some_and(|t| t.contains(&format!("[job {id}]")))
                })
                .then_some(String::new())
        },
    );

    let peek = exec_tool(
        &reg,
        "monitor_wait",
        json!({ "id": id, "timeout_ms": 1000 }),
    )
    .unwrap();
    assert!(
        peek.contains("exited with code 0"),
        "the exited monitor must still answer after a timed-out park: {peek}"
    );
}

#[cfg(unix)]
#[test]
fn monitor_wait_parks_until_exit_and_reports_the_tail() {
    let (reg, _host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 0.5; echo done; exit 0", "session": sid }),
    )
    .unwrap();
    let id: u32 = started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"));

    let began = Instant::now();
    let wait = exec_tool(
        &reg,
        "monitor_wait",
        json!({ "id": id, "timeout_ms": 30000 }),
    )
    .unwrap();
    assert!(
        began.elapsed() >= Duration::from_millis(300),
        "park returned early after {:?}",
        began.elapsed()
    );
    assert!(
        wait.contains("exited with code 0"),
        "a parked wait should report the exit: {wait}"
    );
    assert!(
        wait.contains("done"),
        "the tail should include the job's output: {wait}"
    );
}

#[cfg(unix)]
#[test]
fn monitor_list_reports_only_its_own_monitors() {
    const NOISE_PLUGIN: &str = r#"
maki.api.register_tool({
  name = "noise_job",
  description = "Spawn an unrelated session job",
  handler = function(input)
    local ok, id = pcall(maki.fn.jobstart, input.command, { scope = { session = input.session } })
    if not ok then
      return { llm_output = tostring(id), is_error = true }
    end
    return "job " .. id
  end,
})
"#;

    let (reg, host) = monitor_host();
    host.load_source_with_opts("noise", NOISE_PLUGIN, Default::default())
        .unwrap();
    let session = maki_storage::id::MakiId::generate();
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 30; exit 0", "session": sid }),
    )
    .unwrap();
    let id: u32 = started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"));

    let noise = exec_tool(
        &reg,
        "noise_job",
        json!({ "command": "sleep 30", "session": sid }),
    )
    .unwrap();
    let noise_id: u32 = noise
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a job id in: {noise}"));

    let list = exec_tool(&reg, "monitor_list", json!({ "session": sid })).unwrap();
    assert!(
        list.contains(&format!("monitor {id}")) || list.contains(&format!("  {id}  ")),
        "monitor_list should report its own monitor, got: {list}"
    );
    let noise_row = format!("  {noise_id}  ");
    assert!(
        !list.lines().any(|line| line.starts_with(&noise_row)),
        "monitor_list must not report the unrelated job {noise_id}, got: {list}"
    );

    exec_tool(&reg, "monitor_stop", json!({ "id": id })).unwrap();
    host.event_handle()
        .end_session(session, maki_lua::SessionEndReason::Shutdown);
}

#[cfg(unix)]
#[test]
fn monitor_peek_tails_logs_larger_than_the_tail_window() {
    let (reg, _host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "yes overflow | head -c 400000", "session": sid }),
    )
    .unwrap();
    let id: u32 = started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"));

    let peek = poll_until("monitor_peek never tailed the large log", || {
        let peek = exec_tool(&reg, "monitor_peek", json!({ "id": id })).unwrap();
        peek.contains("--- stdout tail ---").then_some(peek)
    });
    assert!(
        peek.contains("overflow"),
        "the tail of a 400KB log must survive, got: {peek}"
    );
    assert!(
        !peek.contains("(no output captured)"),
        "a large log is not a missing log, got: {peek}"
    );
}

fn monitor_id_of(started: &str) -> u32 {
    started
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("expected a monitor id in: {started}"))
}

fn next_chat_item(
    host: &PluginHost,
    want: ChatItemStatus,
    item_id: &str,
) -> Option<maki_lua::ChatItem> {
    while let Ok(action) = host.ui_action_rx().try_recv() {
        if let UiAction::ChatItem { item, .. } = action
            && item.status == want
            && item.id.as_ref() == item_id
        {
            return Some(item);
        }
    }
    None
}

fn segment_texts(host: &PluginHost) -> Vec<String> {
    host.hint_reader()
        .load_full()
        .segments
        .iter()
        .map(|(text, _)| text.clone())
        .collect()
}

#[cfg(unix)]
#[test]
fn a_monitor_run_lands_in_the_transcript_and_status_bar() {
    let (reg, host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let sid = session.to_string();
    host.event_handle()
        .fire_autocmd("SessionFocusChanged", json!({ "session_id": sid }));

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 2; exit 0", "session": sid }),
    )
    .unwrap();
    let id = monitor_id_of(&started);
    let item_id = format!("monitor:monitor-{id}");

    poll_until("the running chat item never reached the UI", || {
        next_chat_item(&host, ChatItemStatus::Running, &item_id)
    });
    poll_until("the live count never reached the status bar", || {
        segment_texts(&host)
            .iter()
            .any(|text| text == "1 monitor running")
            .then_some(String::new())
    });

    let closed = poll_until("the done chat item never reached the UI", || {
        next_chat_item(&host, ChatItemStatus::Done, &item_id)
    });
    assert!(
        closed
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("exited with code 0")),
        "the closed item should carry the exit: {:?}",
        closed.detail
    );
    poll_until("the segment never left the status bar", || {
        segment_texts(&host).is_empty().then_some(String::new())
    });
}

#[cfg(unix)]
#[test]
fn a_stopped_monitor_closes_quietly() {
    let (reg, host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let mailbox = maki_agent::SessionMailbox::register(session);
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 30", "session": sid }),
    )
    .unwrap();
    let id = monitor_id_of(&started);
    let item_id = format!("monitor:monitor-{id}");

    exec_tool(&reg, "monitor_stop", json!({ "id": id })).unwrap();

    let closed = poll_until("a stopped monitor must close its item", || {
        next_chat_item(&host, ChatItemStatus::Failed, &item_id)
    });
    assert_eq!(closed.detail.as_deref(), Some("killed"));

    let meta_path = started
        .lines()
        .find_map(|l| l.strip_prefix("meta: "))
        .unwrap()
        .to_string();
    poll_until("a stop must record the kill in meta.json", || {
        std::fs::read_to_string(&meta_path)
            .ok()
            .filter(|m| m.contains("\"exit_code\":-1"))
    });
    assert!(
        !mailbox.drain().iter().any(|m| m
            .user_text()
            .is_some_and(|t| t.contains(&format!("[job {id}]")))),
        "a stop the agent asked for must not wake the session"
    );

    host.event_handle()
        .end_session(session, maki_lua::SessionEndReason::Shutdown);
}

#[cfg(unix)]
#[test]
fn exit_while_unloaded_is_finalized_on_reload() {
    let (reg, host) = monitor_host();
    let session = maki_storage::id::MakiId::generate();
    let mailbox = maki_agent::SessionMailbox::register(session);
    let sid = session.to_string();

    let started = exec_tool(
        &reg,
        "monitor",
        json!({ "command": "sleep 0.2; exit 0", "session": sid }),
    )
    .unwrap();
    let id = monitor_id_of(&started);

    host.unload("monitor").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    host.load_package(
        "monitor",
        root,
        PluginPermissions::trusted(),
        Default::default(),
    )
    .unwrap();

    poll_until(
        "an exit while unloaded must still notify after the reload",
        || {
            mailbox
                .drain()
                .iter()
                .any(|m| {
                    m.user_text()
                        .is_some_and(|t| t.contains(&format!("[job {id}]")))
                })
                .then_some(String::new())
        },
    );

    host.event_handle()
        .end_session(session, maki_lua::SessionEndReason::Shutdown);
}
