// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
};
use objc2_app_kit::NSRunningApplication;
use serde_json::{json, Value};

const EXIT_WAIT: Duration = Duration::from_secs(3);
const EXIT_POLL: Duration = Duration::from_millis(100);

pub struct QuitAppTool;

static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

fn def() -> &'static ToolDef {
    DEF.get_or_init(|| ToolDef {
        name: "quit_app".into(),
        description: "Ask an app to quit the way the Dock's Quit does, without accessibility, \
             focus or keystrokes: the app runs its normal quit path, so it can prompt to save \
             or refuse. Works when the app's accessibility is unresponsive and hotkey cmd+q is \
             refused (off_space_or_ax_unresolved). Waits up to 3 s and reports `exited`; an app \
             that stays running (a save prompt, a refusal) is not killed. Use kill_app only as \
             a last resort."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["pid"],
            "properties": {
                "pid": { "type": "integer", "description": "PID of the app to quit." },
                "session": cua_driver_core::tool_schema::session_schema()
            },
            "additionalProperties": false,
        }),
        read_only: false,
        destructive: true,
        idempotent: true,
        open_world: false,
    })
}

fn running(pid: i32) -> bool {
    unsafe {
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .is_some_and(|app| !app.isTerminated())
    }
}

fn request_quit(pid: i32) -> Result<(), String> {
    unsafe {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .ok_or_else(|| format!("no running app with pid {pid}"))?;
        if app.terminate() {
            Ok(())
        } else {
            Err(format!("pid {pid} did not accept the quit request"))
        }
    }
}

#[async_trait]
impl Tool for QuitAppTool {
    fn def(&self) -> &ToolDef {
        def()
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let pid = match args.get("pid").and_then(Value::as_i64) {
            Some(p) if p > 0 && p <= i64::from(i32::MAX) => p as i32,
            _ => return ToolResult::error("quit_app: `pid` must be a positive integer"),
        };
        let outcome = tokio::task::spawn_blocking(move || {
            request_quit(pid)?;
            let deadline = Instant::now() + EXIT_WAIT;
            while Instant::now() < deadline && running(pid) {
                std::thread::sleep(EXIT_POLL);
            }
            Ok::<bool, String>(!running(pid))
        })
        .await;
        match outcome {
            Ok(Ok(exited)) => ToolResult::text(if exited {
                format!("✅ pid {pid} quit.")
            } else {
                format!("Asked pid {pid} to quit; it is still running (a save prompt or a refusal).")
            })
            .with_structured(json!({ "pid": pid, "exited": exited })),
            Ok(Err(error)) => ToolResult::error(format!("quit_app: {error}")),
            Err(error) => ToolResult::error(format!("quit_app: task failed: {error}")),
        }
    }
}
