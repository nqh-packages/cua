// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Cua AI, Inc.

use async_trait::async_trait;
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
    tool_args::ArgsExt,
};
use serde_json::{json, Value};

use crate::{session_resources, video_sckit};

pub struct DisplayRecordingTool;

static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

fn def() -> &'static ToolDef {
    DEF.get_or_init(|| ToolDef {
        name: "display_recording".into(),
        description: "Record one display to an mp4 (H.264, 30 fps), for example the virtual \
             display created by `agent_display`, which `start_recording {record_video:true}` \
             cannot capture because it records the main display only. `start` needs `display_id` \
             and `output_path`; `stop` needs `display_id` and returns the file path and duration \
             once the mp4 is finalized. A recording belongs to the calling `session` and is \
             stopped when that session ends, idles out or the display is removed. Requires \
             Screen Recording permission."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["action", "display_id"],
            "properties": {
                "action": { "type": "string", "enum": ["start", "stop"] },
                "display_id": { "type": "integer", "description": "Id from agent_display create/list." },
                "output_path": { "type": "string", "description": "start: absolute path of the .mp4 to write." },
                "session": cua_driver_core::tool_schema::session_schema()
            },
            "additionalProperties": false,
        }),
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
    })
}

#[async_trait]
impl Tool for DisplayRecordingTool {
    fn def(&self) -> &ToolDef {
        def()
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("");
        let Some(display_id) = args
            .get("display_id")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
        else {
            return ToolResult::error("display_recording: `display_id` must be an integer");
        };
        match action {
            "start" => {
                let Some(path) = args.opt_str("output_path").filter(|p| !p.is_empty()) else {
                    return ToolResult::error("display_recording: `start` needs `output_path`");
                };
                let session = args.opt_str("_session_id").unwrap_or_default();
                let target = std::path::PathBuf::from(&path);
                let started = tokio::task::spawn_blocking(move || {
                    video_sckit::start_on_display(&target, display_id)
                })
                .await;
                match started {
                    Ok(Ok(backend)) => match session_resources::own_recording(&session, display_id, backend) {
                        Ok(()) => ToolResult::text(format!(
                            "✅ Recording display {display_id} to {path}."
                        ))
                        .with_structured(json!({ "display_id": display_id, "output_path": path })),
                        Err(backend) => {
                            let _ = tokio::task::spawn_blocking(move || backend.stop()).await;
                            ToolResult::error(format!(
                                "display_recording: display {display_id} is already being recorded"
                            ))
                        }
                    },
                    Ok(Err(error)) => ToolResult::error(format!("display_recording: {error}")),
                    Err(error) => ToolResult::error(format!("display_recording: task failed: {error}")),
                }
            }
            "stop" => {
                let stopped = tokio::task::spawn_blocking(move || {
                    session_resources::finish_recording(display_id)
                })
                .await;
                match stopped {
                    Ok(Some(Ok(meta))) => ToolResult::text(format!(
                        "✅ Saved {} ({} ms).",
                        meta.path.display(),
                        meta.duration_ms
                    ))
                    .with_structured(json!({
                        "display_id": display_id,
                        "path": meta.path,
                        "duration_ms": meta.duration_ms,
                        "finalized": meta.finalized,
                    })),
                    Ok(Some(Err(error))) => ToolResult::error(format!("display_recording: {error}")),
                    Ok(None) => ToolResult::error(format!(
                        "display_recording: display {display_id} is not being recorded"
                    )),
                    Err(error) => ToolResult::error(format!("display_recording: task failed: {error}")),
                }
            }
            _ => ToolResult::error("display_recording: `action` must be start or stop"),
        }
    }
}
