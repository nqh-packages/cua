// SPDX-License-Identifier: MIT

use async_trait::async_trait;
use core_foundation::base::{CFRelease, CFTypeRef};
use cua_driver_core::{
    protocol::ToolResult,
    tool::{Tool, ToolDef},
    tool_args::ArgsExt,
};
use serde_json::{json, Value};

use crate::ax::bindings::{
    copy_children, copy_element_attr, copy_string_attr, perform_action, AXUIElementCreateApplication,
    AXUIElementRef, kAXErrorSuccess,
};

pub struct InvokeMenuExtraTool;

static DEF: std::sync::OnceLock<ToolDef> = std::sync::OnceLock::new();

fn def() -> &'static ToolDef {
    DEF.get_or_init(|| ToolDef {
        name: "invoke_menu_extra".into(),
        description: "Read or invoke an entry of an app's menu-bar status item (a menu bar \
             extra, for example a SwiftUI MenuBarExtra) through accessibility, with no window, \
             no pointer and no activation. Without `path` it lists the app's status items and \
             their menu entries. With `path` (labels inside the item's menu, 1 to 16) it \
             invokes the final entry; `item` picks the status item by title, description or \
             identifier when the app has more than one. A menu the app has not built yet \
             refuses `menu_not_materialized` and nothing is opened. Menu entries that open a \
             window are then found with `list_windows`."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["pid"],
            "properties": {
                "pid": { "type": "integer", "description": "Process ID of the app that owns the status item." },
                "item": { "type": "string", "description": "Status item title, description or identifier. Optional when the app has exactly one." },
                "path": {
                    "type": "array",
                    "items": { "type": "string", "minLength": 1, "maxLength": 200 },
                    "minItems": 1,
                    "maxItems": 16,
                    "description": "Menu labels from the status item's menu to the entry to invoke. Omit to list."
                },
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

struct Owned(AXUIElementRef);

impl Drop for Owned {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0 as CFTypeRef) }
    }
}

fn children(element: &Owned) -> Vec<Owned> {
    unsafe { copy_children(element.0) }.into_iter().map(Owned).collect()
}

fn attr(element: &Owned, name: &str) -> String {
    unsafe { copy_string_attr(element.0, name) }
        .unwrap_or_default()
        .trim()
        .to_owned()
}

fn labels_match(actual: &str, wanted: &str) -> bool {
    let wanted = wanted.trim();
    actual == wanted || actual == wanted.replace("...", "…")
}

fn item_names(item: &Owned) -> Vec<String> {
    ["AXTitle", "AXDescription", "AXIdentifier"]
        .iter()
        .map(|name| attr(item, name))
        .filter(|value| !value.is_empty())
        .collect()
}

fn item_label(item: &Owned) -> String {
    item_names(item).into_iter().next().unwrap_or_default()
}

fn menu_of(item: &Owned) -> Option<Owned> {
    children(item)
        .into_iter()
        .find(|child| attr(child, "AXRole") == "AXMenu")
}

fn entries(menu: &Owned) -> Vec<Owned> {
    children(menu)
        .into_iter()
        .filter(|child| attr(child, "AXRole") == "AXMenuItem")
        .collect()
}

fn listing(items: &[Owned]) -> Vec<Value> {
    items
        .iter()
        .map(|item| {
            let labels: Vec<String> = menu_of(item)
                .map(|menu| entries(&menu).iter().map(|entry| attr(entry, "AXTitle")).collect())
                .unwrap_or_default();
            json!({ "item": item_label(item), "entries": labels })
        })
        .collect()
}

fn refuse(code: &str, message: String, extra: Value) -> ToolResult {
    ToolResult::error(format!("invoke_menu_extra: {message}")).with_structured(json!({
        "effect": "refused",
        "refusal": { "code": code, "message": message },
        "detail": extra,
    }))
}

fn run(pid: i32, item: Option<String>, path: Option<Vec<String>>) -> ToolResult {
    let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
    let Some(extras) = unsafe { copy_element_attr(app.0, "AXExtrasMenuBar") }.map(Owned) else {
        return refuse(
            "no_status_items",
            format!("pid {pid} exposes no menu-bar status items"),
            json!({}),
        );
    };
    let items: Vec<Owned> = children(&extras)
        .into_iter()
        .filter(|child| attr(child, "AXRole") == "AXMenuBarItem")
        .collect();

    let Some(path) = path else {
        let listed = listing(&items);
        return ToolResult::text(format!("{} status item(s).", listed.len()))
            .with_structured(json!({ "pid": pid, "status_items": listed }));
    };

    let chosen = match &item {
        Some(wanted) => items
            .iter()
            .find(|candidate| item_names(candidate).iter().any(|name| labels_match(name, wanted))),
        None if items.len() == 1 => items.first(),
        None => {
            return refuse(
                "ambiguous_status_item",
                "the app has several status items; pass `item`".into(),
                json!({ "items": items.iter().map(item_label).collect::<Vec<_>>() }),
            )
        }
    };
    let Some(chosen) = chosen else {
        return refuse(
            "status_item_not_found",
            format!("no status item matches {item:?}"),
            json!({ "items": items.iter().map(item_label).collect::<Vec<_>>() }),
        );
    };

    let Some(mut menu) = menu_of(chosen) else {
        return refuse(
            "menu_not_materialized",
            format!("status item {:?} has no built menu; nothing was opened", item_label(chosen)),
            json!({}),
        );
    };

    let last = path.len() - 1;
    for (index, label) in path.iter().enumerate() {
        let level = entries(&menu);
        let Some(found) = level
            .into_iter()
            .find(|entry| labels_match(&attr(entry, "AXTitle"), label))
        else {
            let available: Vec<String> = entries(&menu).iter().map(|e| attr(e, "AXTitle")).collect();
            return refuse(
                "menu_path_unavailable",
                format!("no entry {label:?} at level {}", index + 1),
                json!({ "available": available }),
            );
        };
        if index == last {
            let err = unsafe { perform_action(found.0, "AXPick") };
            if err != kAXErrorSuccess {
                return refuse(
                    "invoke_failed",
                    format!("AXPick on {label:?} returned AX error {err}"),
                    json!({}),
                );
            }
            return ToolResult::text(format!("Invoked {path:?} on status item {:?}.", item_label(chosen)))
                .with_structured(json!({
                    "pid": pid,
                    "item": item_label(chosen),
                    "path": path,
                    "effect": "unverifiable",
                    "hint": "The entry was picked through accessibility; read list_windows or the app state to see what it did.",
                }));
        }
        let Some(next) = menu_of(&found) else {
            return refuse(
                "menu_path_unavailable",
                format!("entry {label:?} has no submenu"),
                json!({}),
            );
        };
        menu = next;
    }
    unreachable!("path is non-empty")
}

#[async_trait]
impl Tool for InvokeMenuExtraTool {
    fn def(&self) -> &ToolDef {
        def()
    }

    async fn invoke(&self, args: Value) -> ToolResult {
        let Some(pid) = args
            .get("pid")
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
        else {
            return ToolResult::error("invoke_menu_extra: `pid` must be an integer");
        };
        let item = args.opt_str("item").filter(|s| !s.trim().is_empty());
        let path = args.get("path").and_then(Value::as_array).map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        });
        if path.as_ref().is_some_and(|p| p.is_empty() || p.len() > 16) {
            return ToolResult::error("invoke_menu_extra: `path` needs 1 to 16 labels");
        }
        match tokio::task::spawn_blocking(move || run(pid, item, path)).await {
            Ok(result) => result,
            Err(error) => ToolResult::error(format!("invoke_menu_extra: task failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::labels_match;

    #[test]
    fn three_periods_match_the_ellipsis_label() {
        assert!(labels_match("Name Logins…", "Name Logins..."));
        assert!(labels_match("Name Logins…", " Name Logins… "));
    }

    #[test]
    fn labels_are_case_sensitive_and_exact() {
        assert!(!labels_match("Name Logins…", "name logins…"));
        assert!(!labels_match("Name Logins…", "Name"));
    }
}
