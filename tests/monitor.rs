//! Loads the plugin through the real `maki-lua` host as a package, the way
//! maki itself would after `maki.pack.add`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use maki_agent::ToolOutput;
use maki_agent::tools::ToolRegistry;
use maki_lua::{PluginHost, PluginPermissions};
use serde_json::json;

const MAILBOX_WAIT: Duration = Duration::from_secs(5);

fn monitor_host() -> (Arc<ToolRegistry>, PluginHost) {
    let reg = Arc::new(ToolRegistry::new());
    let host = PluginHost::new(Arc::clone(&reg)).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    host.load_package("monitor", root, PluginPermissions::trusted(), Default::default())
        .unwrap();
    (reg, host)
}

fn exec_tool(reg: &ToolRegistry, name: &str, input: serde_json::Value) -> Result<String, String> {
    let entry = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool {name} not registered"));
    let inv = entry.tool.parse(&input).expect("parse failed");
    let ctx = maki_agent::tools::test_support::stub_ctx(&maki_agent::AgentMode::Build);
    smol::block_on(async { inv.execute(&ctx).await }).output.map_or_else(
        Err,
        |out| match out {
            ToolOutput::Plain(s) => Ok(s.text),
            other => panic!("unexpected output: {other:?}"),
        },
    )
}

fn poll_until(deadline_msg: &str, f: impl Fn() -> Option<String>) -> String {
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

    poll_until("exit notification never reached the session mailbox", || {
        mailbox
            .drain()
            .iter()
            .any(|m| m.user_text().is_some_and(|t| t.contains(&format!("[job {id}]"))))
            .then_some(String::new())
    });

    let list = exec_tool(&reg, "monitor_list", json!({ "session": sid })).unwrap();
    assert!(
        list.contains(&id.to_string()) && list.contains("exited"),
        "monitor_list should report the exited monitor, got: {list}"
    );

    host.event_handle().end_session(session);
    assert!(
        std::path::Path::new(&stdout_path).exists(),
        "SessionEnd must keep monitor logs so callers can collect them after the session ends"
    );
}
