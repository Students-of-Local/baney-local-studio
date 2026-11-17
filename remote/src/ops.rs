use local_studio_core::Studio;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn arg<T: DeserializeOwned>(args: &Value, key: &str) -> Result<T, String> {
    let value = args.get(key).cloned().unwrap_or(Value::Null);
    serde_json::from_value(value).map_err(|error| format!("args.{key}: {error}"))
}

pub fn idempotency_key(op: &str, args: &Value) -> Option<String> {
    let key = match op {
        "send" => "request",
        "start_thread" | "start_threads" => "draft",
        _ => return None,
    };
    args.get(key).and_then(Value::as_str).map(|value| format!("{op}:{value}"))
}

pub fn request_of(args: &Value) -> Option<String> {
    args.get("request").or_else(|| args.get("draft")).and_then(Value::as_str).map(str::to_string)
}

#[allow(clippy::too_many_lines)]
pub fn dispatch(studio: &Studio, op: &str, a: &Value) -> Result<(), String> {
    match op {
        "add_project" => studio.add_project(arg(a, "path")?),
        "remove_project" => studio.remove_project(arg(a, "project")?),
        "rename_project" => studio.rename_project(arg(a, "project")?, arg(a, "name")?),
        "set_project_icon" => studio.set_project_icon(arg(a, "project")?, arg(a, "icon")?),
        "reorder_projects" => studio.reorder_projects(arg(a, "projects")?),
        "start_thread" => {
            studio.start_thread(arg(a, "draft")?, arg(a, "project")?, arg(a, "prompt")?, arg(a, "settings")?);
        }
        "start_threads" => studio.start_threads(
            arg(a, "draft")?,
            arg(a, "project")?,
            arg(a, "prompt")?,
            arg(a, "settings")?,
            arg(a, "models")?,
        ),
        "send" => studio.send(arg(a, "request")?, arg(a, "thread")?, arg(a, "prompt")?, arg(a, "delivery")?),
        "abort" => studio.abort(arg(a, "thread")?),
        "send_queued_now" => studio.send_queued_now(arg(a, "thread")?, arg(a, "queued")?),
        "remove_queued" => studio.remove_queued(arg(a, "thread")?, arg(a, "queued")?),
        "answer_approval" => studio.answer_approval(arg(a, "thread")?, arg(a, "request")?, arg(a, "answer")?),
        "rename_thread" => studio.rename_thread(arg(a, "thread")?, arg(a, "title")?),
        "delete_thread" => studio.delete_thread(arg(a, "thread")?),
        "restart_daemon" => studio.restart_daemon(arg::<Option<bool>>(a, "force")?.unwrap_or(false)),
        "set_archived" => studio.set_archived(arg(a, "thread")?, arg(a, "archived")?),
        "set_pinned" => studio.set_pinned(arg(a, "thread")?, arg(a, "pinned")?),
        "set_settled" => studio.set_settled(arg(a, "thread")?, arg(a, "settled")?),
        "snooze" => studio.snooze(arg(a, "thread")?, arg(a, "until")?),
        "mark_unread" => studio.mark_unread(arg(a, "thread")?, arg(a, "unread")?),
        "edit_from_here" => studio.edit_from_here(arg(a, "thread")?, arg(a, "entry")?),
        "fork_thread" => studio.fork_thread(arg(a, "thread")?),
        "compact" => studio.compact(arg(a, "thread")?),
        "set_model" => studio.set_model(arg(a, "thread")?, arg(a, "provider")?, arg(a, "model")?),
        "set_thinking_level" => studio.set_thinking_level(arg(a, "thread")?, arg(a, "level")?),
        "set_access_mode" => studio.set_access_mode(arg(a, "thread")?, arg(a, "mode")?),
        "load_models" => studio.load_models(),
        "set_favorite_model" => studio.set_favorite_model(arg(a, "provider")?, arg(a, "model")?, arg(a, "favorite")?),
        "set_hidden_model" => studio.set_hidden_model(arg(a, "provider")?, arg(a, "model")?, arg(a, "hidden")?),
        "list_commands" => studio.list_commands(arg(a, "request")?, arg(a, "project")?),
        "list_skills" => studio.list_skills(arg(a, "request")?, arg(a, "project")?),
        "list_archived" => studio.list_archived(arg(a, "request")?),
        "search_threads" => studio.search_threads(arg(a, "request")?, arg(a, "query")?, arg(a, "limit")?),
        "reorder_pinned" => studio.reorder_pinned(arg(a, "threads")?),
        "set_auto_settle" => studio.set_auto_settle(arg(a, "thread")?, arg(a, "enabled")?),
        "regenerate_title" => studio.regenerate_title(arg(a, "thread")?),
        "set_service_tier" => studio.set_service_tier(arg(a, "thread")?, arg(a, "tier")?),
        "restart_thread" => studio.restart_thread(arg(a, "thread")?),
        "cancel_subagent" => studio.cancel_subagent(arg(a, "thread")?, arg(a, "child")?),
        "load_message_image" => {
            studio.load_message_image(arg(a, "request")?, arg(a, "thread")?, arg(a, "entry")?, arg(a, "index")?);
        }
        "git_status" => studio.git_status(arg(a, "project")?),
        "list_branches" => {
            studio.list_branches(arg(a, "request")?, arg(a, "project")?, arg(a, "query")?, arg(a, "limit")?);
        }
        "load_diff" => studio.load_diff(arg(a, "request")?, arg(a, "project")?, arg(a, "scope")?),
        "list_directory" => studio.list_directory(arg(a, "request")?, arg(a, "project")?, arg(a, "relative")?),
        "read_file" => studio.read_file(arg(a, "request")?, arg(a, "project")?, arg(a, "relative")?),
        "search_files" => {
            studio.search_files(arg(a, "request")?, arg(a, "project")?, arg(a, "query")?, arg(a, "limit")?);
        }
        "search_text" => studio.search_text(arg(a, "request")?, arg(a, "project")?, arg(a, "query")?, arg(a, "limit")?),
        "list_pull_requests" => studio.list_pull_requests(arg(a, "request")?, arg(a, "project")?),
        "load_pull_request" => studio.load_pull_request(arg(a, "request")?, arg(a, "project")?, arg(a, "number")?),
        "load_usage" => studio.load_usage(arg(a, "request")?, arg(a, "period")?),
        _ => return Err(format!("unknown op {op}")),
    }
    Ok(())
}
