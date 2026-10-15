//! Validated display projection of canonical paginated rollout records.
//!
//! The caller obtains bytes through the shared app-server's `fs/readFile` RPC and
//! owns archive, path, scope and epoch checks. This module performs no I/O and
//! never claims its output was returned by native `thread/read`. The conversion
//! follows Codex's CoreTurnItem -> ThreadItem mapping. Unsupported item variants
//! stay visible as raw archivedCoreItem records; unsupported temporal mutations
//! fail instead of displaying an incomplete history as complete.
//! Shell arguments use native shlex quoting; native secret-display redaction is
//! not reproduced. This explicitly marked archive view retains the source bytes'
//! original data and must not be presented as native ThreadItem output.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::path::Path;

#[derive(Debug)]
pub(super) struct RecoveredHistory {
    pub(super) turns: Vec<Value>,
    pub(super) history_mode: String,
}

#[derive(Debug)]
pub(super) struct HistoryError {
    line: usize,
    context: String,
    cause: Option<Box<dyn Error + Send + Sync>>,
}

impl HistoryError {
    fn invalid(line: usize, context: impl Into<String>) -> Self {
        Self {
            line,
            context: context.into(),
            cause: None,
        }
    }

    fn caused(line: usize, context: &str, cause: impl Error + Send + Sync + 'static) -> Self {
        Self {
            line,
            context: context.to_owned(),
            cause: Some(Box::new(cause)),
        }
    }
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "archived rollout line {}: {}", self.line, self.context)?;
        if let Some(cause) = &self.cause {
            write!(f, ": {cause}")?;
        }
        Ok(())
    }
}

impl Error for HistoryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.cause
            .as_ref()
            .map(|cause| cause.as_ref() as &(dyn Error + 'static))
    }
}

struct PendingTurn {
    value: Value,
    started_items: HashMap<String, String>,
    terminal: bool,
}

/// Recover closed canonical turns from a matching paginated rollout.
///
/// IDs are never synthesized. Missing/contradictory boundaries, incomplete
/// items, unknown record formats and unsupported history mutations are errors.
/// Raw Core fields lost by a display conversion are retained in sourcePayload.
pub(super) fn project(
    bytes: &[u8],
    expected_thread_id: &str,
    expected_cwd: &str,
) -> Result<RecoveredHistory, HistoryError> {
    if expected_thread_id.is_empty() || !Path::new(expected_cwd).is_absolute() {
        return Err(HistoryError::invalid(
            0,
            "expected thread ID and absolute cwd are required",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|cause| HistoryError::caused(0, "rollout bytes are not UTF-8", cause))?;
    let mut meta_seen = false;
    let mut turns = Vec::new();
    let mut active: Option<PendingTurn> = None;
    let mut turn_ids = HashSet::new();
    let mut item_ids: HashMap<String, (String, String)> = HashMap::new();
    let mut last_line = 0;
    let mut next_ordinal = 0_u64;
    for (index, raw) in text.lines().enumerate() {
        let line = index + 1;
        last_line = line;
        if raw.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(raw)
            .map_err(|cause| HistoryError::caused(line, "invalid JSON record", cause))?;
        object(&record, line, "record")?;
        let ordinal = record
            .get("ordinal")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                HistoryError::invalid(line, "paginated record ordinal is missing or invalid")
            })?;
        if ordinal != next_ordinal {
            return Err(HistoryError::invalid(
                line,
                "paginated ordinals are missing, duplicated or regressed",
            ));
        }
        next_ordinal = ordinal
            .checked_add(1)
            .ok_or_else(|| HistoryError::invalid(line, "rollout ordinal overflow"))?;
        let kind = string(&record, "type", line)?;
        let payload = record
            .get("payload")
            .ok_or_else(|| HistoryError::invalid(line, "record payload is missing"))?;
        match kind {
            "session_meta" => {
                if meta_seen || active.is_some() || !turns.is_empty() {
                    return Err(HistoryError::invalid(line, "duplicate or late SessionMeta"));
                }
                object(payload, line, "SessionMeta")?;
                if string(payload, "id", line)? != expected_thread_id
                    || local_path(string(payload, "cwd", line)?, line)? != expected_cwd
                {
                    return Err(HistoryError::invalid(
                        line,
                        "SessionMeta does not match the native thread ID/cwd",
                    ));
                }
                if string(payload, "history_mode", line)? != "paginated" {
                    return Err(HistoryError::invalid(
                        line,
                        "only canonical paginated history is supported",
                    ));
                }
                if ["history_base", "subagent_history_start_ordinal"]
                    .iter()
                    .any(|key| payload.get(*key).is_some_and(|value| !value.is_null()))
                {
                    return Err(HistoryError::invalid(line, "inherited/subagent rollout prefixes require native history materialization"));
                }
                meta_seen = true;
            }
            "event_msg" => {
                if !meta_seen {
                    return Err(HistoryError::invalid(line, "event precedes SessionMeta"));
                }
                object(payload, line, "event payload")?;
                match string(payload, "type", line)? {
                    "task_started" | "turn_started" => {
                        if let Some(previous) = active.take() {
                            if !previous.terminal {
                                return Err(HistoryError::invalid(
                                    line,
                                    "new turn started before the previous turn closed",
                                ));
                            }
                            finish(previous, &mut turns, line)?;
                        }
                        let id = identifier(payload, "turn_id", line)?;
                        validate_attribution(payload, id, line)?;
                        if !turn_ids.insert(id.to_owned()) {
                            return Err(HistoryError::invalid(line, "duplicate turn ID"));
                        }
                        active = Some(PendingTurn {
                            value: json!({"id":id,"items":[],"itemsView":"full","status":"inProgress",
                                "error":null,"startedAt":optional_integer(payload,"started_at",line)?,
                                "completedAt":null,"durationMs":null,"sourceStartRecord":record}),
                            started_items: HashMap::new(),
                            terminal: false,
                        });
                    }
                    "item_started" | "item_completed" => {
                        let turn_id = identifier(payload, "turn_id", line)?;
                        if string(payload, "thread_id", line)? != expected_thread_id {
                            return Err(HistoryError::invalid(
                                line,
                                "item thread identity contradicts SessionMeta",
                            ));
                        }
                        let item = payload.get("item").ok_or_else(|| {
                            HistoryError::invalid(line, "canonical item is missing")
                        })?;
                        object(item, line, "canonical item")?;
                        let id = identifier(item, "id", line)?;
                        let item_type = identifier(item, "type", line)?;
                        if string(payload, "type", line)? == "item_started" {
                            let turn = active
                                .as_mut()
                                .filter(|turn| {
                                    !turn.terminal && turn.value["id"].as_str() == Some(turn_id)
                                })
                                .ok_or_else(|| {
                                    HistoryError::invalid(
                                        line,
                                        "item_started has no matching active turn",
                                    )
                                })?;
                            if item_ids.contains_key(id)
                                || turn
                                    .started_items
                                    .insert(id.to_owned(), item_type.to_owned())
                                    .is_some()
                            {
                                return Err(HistoryError::invalid(
                                    line,
                                    "duplicate item_started ID",
                                ));
                            }
                        } else {
                            if let Some(started_type) = active
                                .as_mut()
                                .filter(|turn| turn.value["id"].as_str() == Some(turn_id))
                                .and_then(|turn| turn.started_items.remove(id))
                            {
                                if started_type != item_type {
                                    return Err(HistoryError::invalid(
                                        line,
                                        "completed item type differs from item_started",
                                    ));
                                }
                            }
                            if let Some((existing_turn, existing_type)) = item_ids.get(id) {
                                if existing_turn != turn_id || existing_type != item_type {
                                    return Err(HistoryError::invalid(
                                        line,
                                        "completed item ID reused across a different turn or type",
                                    ));
                                }
                            }
                            let target = if let Some(turn) = active
                                .as_mut()
                                .filter(|turn| turn.value["id"].as_str() == Some(turn_id))
                            {
                                &mut turn.value
                            } else {
                                turns.iter_mut().find(|turn: &&mut Value|turn["id"].as_str()==Some(turn_id))
                                    .ok_or_else(||HistoryError::invalid(line,"completed item targets an unknown or rolled-back turn identity"))?
                            };
                            let mut converted = convert_item(item, line)?;
                            let items = target["items"].as_array_mut().expect("created array");
                            if let Some(existing) = items
                                .iter_mut()
                                .find(|value| value["id"].as_str() == Some(id))
                            {
                                let mut sources = existing
                                    .get("sourceCompletionRecords")
                                    .and_then(Value::as_array)
                                    .cloned()
                                    .unwrap_or_default();
                                sources.push(record.clone());
                                converted["sourceCompletionRecords"] = json!(sources);
                                *existing = converted;
                            } else {
                                converted["sourceCompletionRecords"] = json!([record]);
                                items.push(converted);
                            }
                            item_ids
                                .insert(id.to_owned(), (turn_id.to_owned(), item_type.to_owned()));
                        }
                    }
                    "task_complete" | "turn_complete" | "turn_aborted" => {
                        let turn = active.as_mut().ok_or_else(|| {
                            HistoryError::invalid(line, "terminal event has no active turn")
                        })?;
                        let id = identifier(payload, "turn_id", line)?;
                        validate_attribution(payload, id, line)?;
                        if turn.value["id"].as_str() != Some(id) {
                            return Err(HistoryError::invalid(
                                line,
                                "terminal event targets a different turn",
                            ));
                        }
                        if !turn.started_items.is_empty() {
                            return Err(HistoryError::invalid(
                                line,
                                "terminal event leaves incomplete canonical items",
                            ));
                        }
                        let aborted = string(payload, "type", line)? == "turn_aborted";
                        if aborted {
                            match string(payload, "reason", line)? {
                                "interrupted" | "replaced" | "review_ended" | "budget_limited" => {}
                                _ => {
                                    return Err(HistoryError::invalid(
                                        line,
                                        "unsupported turn abortion reason",
                                    ))
                                }
                            }
                        }
                        if turn.terminal {
                            // The native paginated store freezes the first
                            // terminal state. Keep subsequent durable metadata
                            // inspectable without changing that terminal result.
                            if turn.value.get("sourceAdditionalTerminalRecords").is_none() {
                                turn.value["sourceAdditionalTerminalRecords"] = json!([]);
                            }
                            turn.value["sourceAdditionalTerminalRecords"]
                                .as_array_mut()
                                .expect("created array")
                                .push(record.clone());
                            if !aborted {
                                finish(active.take().expect("active turn"), &mut turns, line)?;
                            }
                            continue;
                        }
                        let error = turn_error(payload.get("error"), line)?;
                        if aborted {
                            turn.value["status"] = json!("interrupted");
                        } else if !error.is_null() {
                            turn.value["status"] = json!("failed");
                        } else if !turn.terminal {
                            turn.value["status"] = json!("completed");
                        }
                        if !error.is_null() {
                            turn.value["error"] = error;
                        }
                        // Native paginated projection takes terminal timestamps
                        // directly from this event, including missing values.
                        turn.value["startedAt"] = optional_integer(payload, "started_at", line)?;
                        turn.value["completedAt"] =
                            optional_integer(payload, "completed_at", line)?;
                        turn.value["durationMs"] =
                            optional_nonnegative_integer(payload, "duration_ms", line)?;
                        if aborted {
                            turn.value["sourceAbortRecord"] = record.clone();
                        } else {
                            turn.value["sourceTerminalRecord"] = record.clone();
                        }
                        turn.terminal = true;
                        if !aborted {
                            finish(active.take().expect("active turn"), &mut turns, line)?;
                        }
                    }
                    // These durable events do not change displayed turn items in
                    // native paginated projection. Settings are read separately.
                    "token_count" | "thread_settings_applied" | "thread_goal_updated" => {}
                    "thread_rolled_back" => {
                        // Legacy migration removes these markers before writing
                        // canonical paginated history. Applying the legacy
                        // last-N lifecycle rule would change paginated history.
                        return Err(HistoryError::invalid(
                            line,
                            "mixed legacy rollback marker requires native history materialization",
                        ));
                    }
                    other => {
                        return Err(HistoryError::invalid(
                            line,
                            format!("unsupported canonical event mutation: {other}"),
                        ))
                    }
                }
            }
            // Paginated ItemCompleted events are authoritative. Provider response
            // records must not create duplicate display items or invented IDs.
            "response_item" => {
                object(payload, line, "response item")?;
                if matches!(
                    payload.get("type").and_then(Value::as_str),
                    Some("compaction" | "context_compaction" | "compaction_trigger")
                ) {
                    return Err(HistoryError::invalid(
                        line,
                        "unsupported response compaction mutation",
                    ));
                }
            }
            "compacted" => {
                object(payload, line, "compaction payload")?;
                string(payload, "message", line)?;
                let turn = active
                    .as_mut()
                    .filter(|turn| !turn.terminal)
                    .ok_or_else(|| {
                        HistoryError::invalid(
                            line,
                            "implicit legacy compaction has no canonical active turn",
                        )
                    })?;
                if turn.value.get("sourceCompactions").is_none() {
                    turn.value["sourceCompactions"] = json!([]);
                }
                turn.value["sourceCompactions"]
                    .as_array_mut()
                    .expect("created array")
                    .push(record.clone());
            }
            "turn_context"
            | "token_usage_record"
            | "world_state"
            | "retained_context"
            | "security_risk_score"
            | "inter_agent_communication"
            | "inter_agent_communication_metadata" => {
                object(payload, line, "context payload")?;
            }
            other => {
                return Err(HistoryError::invalid(
                    line,
                    format!("unsupported rollout record: {other}"),
                ))
            }
        }
        if !meta_seen {
            return Err(HistoryError::invalid(line, "record precedes SessionMeta"));
        }
    }
    if let Some(turn) = active {
        if !turn.terminal {
            return Err(HistoryError::invalid(
                last_line,
                "rollout ends with an incomplete turn",
            ));
        }
        finish(turn, &mut turns, last_line)?;
    }
    if !meta_seen || turns.is_empty() {
        return Err(HistoryError::invalid(
            last_line,
            "rollout has no closed canonical turns",
        ));
    }
    Ok(RecoveredHistory {
        turns,
        history_mode: "paginated".to_owned(),
    })
}

fn finish(turn: PendingTurn, turns: &mut Vec<Value>, line: usize) -> Result<(), HistoryError> {
    if !turn.started_items.is_empty() {
        return Err(HistoryError::invalid(
            line,
            "turn contains incomplete canonical items",
        ));
    }
    if turn.value["items"].as_array().is_none_or(Vec::is_empty)
        && turn.value["status"] != "interrupted"
        && turn.value["status"] != "failed"
    {
        return Err(HistoryError::invalid(
            line,
            "canonical turn contains no completed items",
        ));
    }
    turns.push(turn.value);
    Ok(())
}

fn validate_attribution(payload: &Value, id: &str, line: usize) -> Result<(), HistoryError> {
    if let Some(attribution) = payload.get("turn_attribution") {
        object(attribution, line, "turn attribution")?;
        if identifier(attribution, "turn_id", line)? != id {
            return Err(HistoryError::invalid(
                line,
                "turn attribution contradicts event turn ID",
            ));
        }
    }
    Ok(())
}

fn turn_error(error: Option<&Value>, line: usize) -> Result<Value, HistoryError> {
    let Some(error) = error.filter(|value| !value.is_null()) else {
        return Ok(Value::Null);
    };
    object(error, line, "terminal error")?;
    Ok(json!({"message":string(error,"message",line)?,
        "codexErrorInfo":error.get("codex_error_info").cloned().unwrap_or(Value::Null),
        "additionalDetails":error.get("additional_details").cloned().unwrap_or(Value::Null),
        "misalignment":error.get("misalignment").cloned().unwrap_or(Value::Null)}))
}

fn convert_item(item: &Value, line: usize) -> Result<Value, HistoryError> {
    let id = identifier(item, "id", line)?;
    let kind = identifier(item, "type", line)?;
    let mut output = match kind {
        "UserMessage" => {
            let content = array(item, "content", line)?
                .iter()
                .map(|input| convert_input(input, line))
                .collect::<Result<Vec<_>, _>>()?;
            json!({"type":"userMessage","id":id,"clientId":optional_string(item,"client_id",line)?,"content":content})
        }
        "AgentMessage" => {
            let mut text = String::new();
            for part in array(item, "content", line)? {
                if string(part, "type", line)? != "Text" {
                    return Ok(raw_item(item, id));
                }
                text.push_str(string(part, "text", line)?);
            }
            let memory = match item.get("memory_citation").filter(|value| !value.is_null()) {
                Some(value) => {
                    let mut memory = object(value, line, "memory citation")?.clone();
                    if let Some(ids) = memory.remove("rolloutIds") {
                        memory.insert("threadIds".to_owned(), ids);
                    }
                    Value::Object(memory)
                }
                None => Value::Null,
            };
            json!({"type":"agentMessage","id":id,"text":text,
                "phase":optional_string(item,"phase",line)?,"memoryCitation":memory,
                "delivery":item.get("delivery").cloned().unwrap_or(Value::Null),
                "questions":item.get("questions").cloned().unwrap_or(Value::Null),"sourcePayload":item})
        }
        "Reasoning" => json!({"type":"reasoning","id":id,
            "summary":strings(item,"summary_text",line)?,
            "content":if item.get("raw_content").is_some(){strings(item,"raw_content",line)?}else{Vec::new()}}),
        "Plan" => json!({"type":"plan","id":id,"text":string(item,"text",line)?}),
        "CommandExecution" => convert_command(item, id, line)?,
        "FileChange" => convert_file_changes(item, id, line)?,
        "WebSearch" => {
            let mut action = object(
                item.get("action")
                    .ok_or_else(|| HistoryError::invalid(line, "web search action missing"))?,
                line,
                "web search action",
            )?
            .clone();
            let action_kind = action
                .get("type")
                .and_then(Value::as_str)
                .ok_or_else(|| HistoryError::invalid(line, "web search action type missing"))?;
            let mapped = match action_kind {
                "search" => "search",
                "open_page" => "openPage",
                "find_in_page" => "findInPage",
                "other" => "other",
                _ => return Ok(raw_item(item, id)),
            };
            action.insert("type".to_owned(), json!(mapped));
            let results = item.get("results").cloned().unwrap_or(Value::Null);
            if !results.is_null() && !results.is_array() {
                return Err(HistoryError::invalid(
                    line,
                    "web search results must be an array or null",
                ));
            }
            json!({"type":"webSearch","id":id,"query":string(item,"query",line)?,"action":action,"results":results})
        }
        "DynamicToolCall" => json!({"type":"dynamicToolCall","id":id,
            "namespace":optional_string(item,"namespace",line)?,"tool":string(item,"tool",line)?,
            "arguments":item.get("arguments").ok_or_else(|| HistoryError::invalid(line,"dynamic tool arguments missing"))?,
            "status":status(item,line)?,"contentItems":item.get("content_items").cloned().unwrap_or(Value::Null),
            "success":item.get("success").cloned().unwrap_or(Value::Null),
            "durationMs":duration_ms(item.get("duration"),line)?,"sourcePayload":item}),
        "ContextCompaction" => json!({"type":"contextCompaction","id":id}),
        // CamelCase API records are already projected. Retain arbitrary future
        // fields and opaque nested JSON without recursive key rewriting.
        "userMessage" | "agentMessage" | "reasoning" | "plan" | "commandExecution"
        | "fileChange" | "webSearch" | "dynamicToolCall" | "contextCompaction" => {
            validate_api_item(item, line)?;
            return Ok(item.clone());
        }
        _ => return Ok(raw_item(item, id)),
    };
    let known_fields: &[&str] = match kind {
        "UserMessage" => &["type", "id", "client_id", "content"],
        "Reasoning" => &["type", "id", "summary_text", "raw_content"],
        "Plan" => &["type", "id", "text"],
        "WebSearch" => &["type", "id", "query", "action", "results"],
        "ContextCompaction" => &["type", "id"],
        _ => &[],
    };
    if !known_fields.is_empty()
        && object(item, line, "item")?
            .keys()
            .any(|key| !known_fields.contains(&key.as_str()))
    {
        output["sourcePayload"] = item.clone();
    }
    Ok(output)
}

fn raw_item(item: &Value, id: &str) -> Value {
    json!({"type":"archivedCoreItem","id":id,"sourcePayload":item})
}

fn validate_api_item(item: &Value, line: usize) -> Result<(), HistoryError> {
    match string(item, "type", line)? {
        "userMessage" => {
            array(item, "content", line)?;
        }
        "agentMessage" | "plan" => {
            string(item, "text", line)?;
        }
        "reasoning" => {
            strings(item, "summary", line)?;
            strings(item, "content", line)?;
        }
        "commandExecution" => {
            string(item, "command", line)?;
            local_path(string(item, "cwd", line)?, line)?;
            match string(item, "status", line)? {
                "inProgress" | "completed" | "failed" | "declined" => {}
                _ => {
                    return Err(HistoryError::invalid(
                        line,
                        "unsupported API command status",
                    ))
                }
            }
            array(item, "commandActions", line)?;
            optional_string(item, "aggregatedOutput", line)?;
            optional_integer(item, "exitCode", line)?;
        }
        "fileChange" => {
            array(item, "changes", line)?;
            string(item, "status", line)?;
        }
        "webSearch" => {
            string(item, "query", line)?;
        }
        "dynamicToolCall" => {
            string(item, "tool", line)?;
            string(item, "status", line)?;
            if item.get("arguments").is_none() {
                return Err(HistoryError::invalid(
                    line,
                    "dynamic tool arguments missing",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn convert_input(input: &Value, line: usize) -> Result<Value, HistoryError> {
    let mut output = object(input, line, "user input")?.clone();
    let kind = string(input, "type", line)?;
    match kind {
        "text" => {
            let text = string(input, "text", line)?;
            let mut elements = Vec::new();
            if let Some(value) = input.get("text_elements") {
                for element in value
                    .as_array()
                    .ok_or_else(|| HistoryError::invalid(line, "text_elements must be an array"))?
                {
                    let mut mapped = object(element, line, "text element")?.clone();
                    let range = element
                        .get("byte_range")
                        .ok_or_else(|| HistoryError::invalid(line, "text byte range missing"))?;
                    let start = range
                        .get("start")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| HistoryError::invalid(line, "invalid text range start"))?;
                    let end = range
                        .get("end")
                        .and_then(Value::as_u64)
                        .ok_or_else(|| HistoryError::invalid(line, "invalid text range end"))?;
                    if start > end
                        || end > text.len() as u64
                        || !text.is_char_boundary(start as usize)
                        || !text.is_char_boundary(end as usize)
                    {
                        return Err(HistoryError::invalid(
                            line,
                            "text element range is outside UTF-8 text",
                        ));
                    }
                    mapped.remove("byte_range");
                    mapped.insert("byteRange".to_owned(), range.clone());
                    elements.push(Value::Object(mapped));
                }
            }
            output.insert("text_elements".to_owned(), json!(elements));
        }
        "image" => {
            if let Some(url) = output.remove("image_url") {
                if !url.is_string() {
                    return Err(HistoryError::invalid(line, "image_url must be a string"));
                }
                output.insert("url".to_owned(), url);
            } else if let Some(id) = output.remove("file_id") {
                if !id.is_string() {
                    return Err(HistoryError::invalid(line, "file_id must be a string"));
                }
                output.insert("fileId".to_owned(), id);
            } else {
                return Err(HistoryError::invalid(line, "image reference missing"));
            }
        }
        "audio" => {
            output.insert("url".to_owned(), json!(string(input, "audio_url", line)?));
            output.remove("audio_url");
        }
        "local_image" | "local_audio" => {
            string(input, "path", line)?;
            output.insert(
                "type".to_owned(),
                json!(if kind == "local_image" {
                    "localImage"
                } else {
                    "localAudio"
                }),
            );
        }
        "skill" | "mention" => {
            string(input, "name", line)?;
            string(input, "path", line)?;
        }
        _ => {} // Future structured input is retained for the escaped unknown renderer.
    }
    Ok(Value::Object(output))
}

fn convert_command(item: &Value, id: &str, line: usize) -> Result<Value, HistoryError> {
    let argv = strings(item, "command", line)?;
    if argv.is_empty() {
        return Err(HistoryError::invalid(line, "command argv is empty"));
    }
    let command = shlex::try_join(argv.iter().map(String::as_str)).map_err(|cause| {
        HistoryError::caused(line, "command argv cannot be shell quoted", cause)
    })?;
    let cwd = local_path(string(item, "cwd", line)?, line)?;
    let mut actions = Vec::new();
    for parsed in array(item, "parsed_cmd", line)? {
        let command = string(parsed, "cmd", line)?;
        actions.push(match string(parsed,"type",line)? {
            "read" => {
                let path = string(parsed,"path",line)?;
                let resolved = join_local_path(&cwd,path,line)?;
                json!({"type":"read","command":command,"name":string(parsed,"name",line)?,"path":resolved})
            }
            "list_files" => json!({"type":"listFiles","command":command,"path":optional_string(parsed,"path",line)?}),
            "search" => json!({"type":"search","command":command,"query":optional_string(parsed,"query",line)?,"path":optional_string(parsed,"path",line)?}),
            "unknown" => json!({"type":"unknown","command":command}),
            _ => return Err(HistoryError::invalid(line,"unsupported parsed command action")),
        });
    }
    let source = match string(item, "source", line)? {
        "agent" => "agent",
        "user_shell" => "userShell",
        "unified_exec_startup" => "unifiedExecStartup",
        "unified_exec_interaction" => "unifiedExecInteraction",
        _ => {
            return Err(HistoryError::invalid(
                line,
                "unsupported command execution source",
            ))
        }
    };
    let aggregated = optional_string(item, "aggregated_output", line)?;
    let aggregated = if aggregated.as_str() == Some("") {
        Value::Null
    } else {
        aggregated
    };
    let exit_code = optional_integer(item, "exit_code", line)?;
    if exit_code
        .as_i64()
        .is_some_and(|number| i32::try_from(number).is_err())
    {
        return Err(HistoryError::invalid(
            line,
            "command exit_code is outside i32 range",
        ));
    }
    Ok(
        json!({"type":"commandExecution","id":id,"command":command,"cwd":cwd,
        "status":status(item,line)?,"source":source,"commandActions":actions,
        "aggregatedOutput":aggregated,"exitCode":exit_code,
        "processId":optional_string(item,"process_id",line)?,"durationMs":duration_ms(item.get("duration"),line)?,
        "modelContext":item.get("model_context").cloned().unwrap_or(Value::Null),
        "sandboxType":item.get("sandbox_type").cloned().unwrap_or(Value::Null),
        "pluginId":optional_string(item,"plugin_id",line)?,"scriptPath":optional_string(item,"script_path",line)?,
        "sourcePayload":item}),
    )
}

fn convert_file_changes(item: &Value, id: &str, line: usize) -> Result<Value, HistoryError> {
    let changes = object(
        item.get("changes")
            .ok_or_else(|| HistoryError::invalid(line, "file changes missing"))?,
        line,
        "file changes",
    )?;
    let mut paths = changes.keys().collect::<Vec<_>>();
    paths.sort();
    let mut converted = Vec::new();
    for path in paths {
        let change = &changes[path];
        let (kind, diff) = match string(change, "type", line)? {
            "add" | "delete" => (
                json!({"type":string(change,"type",line)?}),
                string(change, "content", line)?.to_owned(),
            ),
            "update" => {
                let moved = optional_string(change, "move_path", line)?;
                let diff = string(change, "unified_diff", line)?;
                let diff = match moved.as_str() {
                    Some(path) => format!("{diff}\n\nMoved to: {path}"),
                    None => diff.to_owned(),
                };
                (json!({"type":"update","move_path":moved}), diff)
            }
            _ => return Err(HistoryError::invalid(line, "unsupported file change kind")),
        };
        converted.push(json!({"path":path,"kind":kind,"diff":diff}));
    }
    let status = if item.get("status").is_none_or(Value::is_null) {
        "inProgress"
    } else {
        status(item, line)?
    };
    Ok(
        json!({"type":"fileChange","id":id,"status":status,"changes":converted,"sourcePayload":item}),
    )
}

fn status(item: &Value, line: usize) -> Result<&str, HistoryError> {
    match string(item, "status", line)? {
        "in_progress" => Ok("inProgress"),
        "completed" => Ok("completed"),
        "failed" => Ok("failed"),
        "declined" => Ok("declined"),
        _ => Err(HistoryError::invalid(
            line,
            "unsupported canonical item status",
        )),
    }
}

fn duration_ms(duration: Option<&Value>, line: usize) -> Result<Value, HistoryError> {
    let Some(duration) = duration.filter(|value| !value.is_null()) else {
        return Ok(Value::Null);
    };
    let seconds = duration
        .get("secs")
        .and_then(Value::as_u64)
        .ok_or_else(|| HistoryError::invalid(line, "invalid duration seconds"))?;
    let nanos = duration
        .get("nanos")
        .and_then(Value::as_u64)
        .ok_or_else(|| HistoryError::invalid(line, "invalid duration nanoseconds"))?;
    if nanos >= 1_000_000_000 {
        return Err(HistoryError::invalid(
            line,
            "duration nanoseconds out of range",
        ));
    }
    let millis = seconds
        .checked_mul(1000)
        .and_then(|value| value.checked_add(nanos / 1_000_000))
        .and_then(|value| i64::try_from(value).ok())
        .ok_or_else(|| HistoryError::invalid(line, "duration milliseconds overflow"))?;
    Ok(json!(millis))
}

fn local_path(input: &str, line: usize) -> Result<String, HistoryError> {
    let path = if let Some(encoded) = input.strip_prefix("file://") {
        if !encoded.starts_with('/') || encoded.contains(['?', '#']) {
            return Err(HistoryError::invalid(
                line,
                "only local absolute file URIs are supported",
            ));
        }
        let mut bytes = Vec::new();
        let raw = encoded.as_bytes();
        let mut index = 0;
        while index < raw.len() {
            if raw[index] == b'%' {
                let pair = raw
                    .get(index + 1..index + 3)
                    .ok_or_else(|| HistoryError::invalid(line, "incomplete file URI escape"))?;
                let digit = |byte: u8| (byte as char).to_digit(16);
                let high = digit(pair[0])
                    .ok_or_else(|| HistoryError::invalid(line, "invalid file URI escape"))?;
                let low = digit(pair[1])
                    .ok_or_else(|| HistoryError::invalid(line, "invalid file URI escape"))?;
                bytes.push((high * 16 + low) as u8);
                index += 3;
            } else {
                bytes.push(raw[index]);
                index += 1;
            }
        }
        String::from_utf8(bytes)
            .map_err(|cause| HistoryError::caused(line, "file URI path is not UTF-8", cause))?
    } else {
        input.to_owned()
    };
    if !Path::new(&path).is_absolute() || path.contains('\0') {
        return Err(HistoryError::invalid(
            line,
            "path must be absolute and contain no NUL",
        ));
    }
    Ok(path)
}

// Match PathUri::join for validated local POSIX paths. Resolution is lexical:
// no symlinks or files on the monitor host are read.
fn join_local_path(cwd: &str, path: &str, line: usize) -> Result<String, HistoryError> {
    if path.contains('\0') {
        return Err(HistoryError::invalid(line, "parsed read path contains NUL"));
    }
    let mut segments = if path.starts_with('/') {
        Vec::new()
    } else {
        cwd.split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
    };
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            part => segments.push(part),
        }
    }
    Ok(format!("/{}", segments.join("/")))
}

fn object<'a>(
    value: &'a Value,
    line: usize,
    name: &str,
) -> Result<&'a Map<String, Value>, HistoryError> {
    value
        .as_object()
        .ok_or_else(|| HistoryError::invalid(line, format!("{name} must be an object")))
}
fn string<'a>(value: &'a Value, key: &str, line: usize) -> Result<&'a str, HistoryError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| HistoryError::invalid(line, format!("{key} must be a string")))
}
fn identifier<'a>(value: &'a Value, key: &str, line: usize) -> Result<&'a str, HistoryError> {
    let id = string(value, key, line)?;
    if id.is_empty() {
        return Err(HistoryError::invalid(
            line,
            format!("{key} must not be empty"),
        ));
    }
    Ok(id)
}
fn array<'a>(value: &'a Value, key: &str, line: usize) -> Result<&'a Vec<Value>, HistoryError> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| HistoryError::invalid(line, format!("{key} must be an array")))
}
fn strings(value: &Value, key: &str, line: usize) -> Result<Vec<String>, HistoryError> {
    array(value, key, line)?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                HistoryError::invalid(line, format!("{key} entries must be strings"))
            })
        })
        .collect()
}
fn optional_string(value: &Value, key: &str, line: usize) -> Result<Value, HistoryError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(value) if value.is_string() => Ok(value.clone()),
        _ => Err(HistoryError::invalid(
            line,
            format!("{key} must be a string or null"),
        )),
    }
}
fn optional_integer(value: &Value, key: &str, line: usize) -> Result<Value, HistoryError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(Value::Null),
        Some(value) if value.as_i64().is_some() => Ok(value.clone()),
        _ => Err(HistoryError::invalid(
            line,
            format!("{key} must be an integer or null"),
        )),
    }
}
fn optional_nonnegative_integer(
    value: &Value,
    key: &str,
    line: usize,
) -> Result<Value, HistoryError> {
    let number = optional_integer(value, key, line)?;
    if number.as_i64().is_some_and(|number| number < 0) {
        return Err(HistoryError::invalid(
            line,
            format!("{key} must not be negative"),
        ));
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(payload: Value) -> Value {
        json!({"type":"event_msg","payload":payload})
    }
    fn started(id: &str) -> Value {
        event(json!({"type":"task_started","turn_id":id,"started_at":11}))
    }
    fn completed(id: &str) -> Value {
        event(
            json!({"type":"task_complete","turn_id":id,"started_at":11,"completed_at":13,"duration_ms":2000}),
        )
    }
    fn item(turn: &str, item: Value) -> Value {
        event(json!({"type":"item_completed","thread_id":"thread-1","turn_id":turn,"item":item}))
    }
    fn records(mut body: Vec<Value>) -> Vec<u8> {
        body.insert(0,json!({"type":"session_meta","payload":{"id":"thread-1","cwd":"/workspace","history_mode":"paginated"}}));
        for (ordinal, record) in body.iter_mut().enumerate() {
            record["ordinal"] = json!(ordinal);
            record["timestamp"] = json!("2026-10-10T00:00:00Z");
        }
        body.iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes()
    }
    fn recover(body: Vec<Value>) -> RecoveredHistory {
        project(&records(body), "thread-1", "/workspace").unwrap()
    }
    fn command() -> Value {
        json!({"type":"CommandExecution","id":"cmd-1","command":["/bin/zsh","-lc","cat check.txt"],"cwd":"file:///workspace",
            "parsed_cmd":[{"type":"read","cmd":"cat check.txt","name":"check.txt","path":"check.txt"}],
            "source":"unified_exec_startup","status":"completed","aggregated_output":"marker\n","exit_code":0,
            "duration":{"secs":0,"nanos":7875},"process_id":"123"})
    }
    fn body(item_value: Value) -> Vec<Value> {
        vec![
            started("turn-1"),
            item("turn-1", item_value),
            completed("turn-1"),
        ]
    }
    fn rejects(body: Vec<Value>, context: &str) {
        let error = project(&records(body), "thread-1", "/workspace").unwrap_err();
        assert!(error.to_string().contains(context), "{error}");
    }

    #[test]
    fn canonical_command_keeps_native_ids_exact_safe_quoting_output_and_duration() {
        let recovered = recover(body(command()));
        assert_eq!(recovered.history_mode, "paginated");
        let turn = &recovered.turns[0];
        assert_eq!(turn["id"], "turn-1");
        assert_eq!(turn["status"], "completed");
        assert_eq!(turn["startedAt"], 11);
        assert_eq!(turn["completedAt"], 13);
        let item = &turn["items"][0];
        assert_eq!(item["id"], "cmd-1");
        assert_eq!(item["command"], "/bin/zsh -lc 'cat check.txt'");
        assert_eq!(item["cwd"], "/workspace");
        assert_eq!(
            item["commandActions"],
            json!([{"type":"read","command":"cat check.txt","name":"check.txt","path":"/workspace/check.txt"}])
        );
        assert_eq!(item["source"], "unifiedExecStartup");
        assert_eq!(item["aggregatedOutput"], "marker\n");
        assert_eq!(item["exitCode"], 0);
        assert_eq!(item["durationMs"], 0);
        assert_eq!(item["sourcePayload"], command());
    }

    #[test]
    fn empty_command_output_stays_native_null_and_argv_handles_quotes() {
        let mut value = command();
        value["aggregated_output"] = json!("");
        value["command"] = json!(["printf", "a'b", ""]);
        let recovered = recover(body(value));
        assert!(recovered.turns[0]["items"][0]["aggregatedOutput"].is_null());
        assert_eq!(
            recovered.turns[0]["items"][0]["command"],
            shlex::try_join(["printf", "a'b", ""]).unwrap()
        );
    }

    #[test]
    fn user_inputs_and_agent_phase_preserve_boundaries_and_unknown_content() {
        let input = json!({"type":"UserMessage","id":"user-1","client_id":"client-1","content":[
            {"type":"text","text":"héllo","text_elements":[{"byte_range":{"start":1,"end":3},"placeholder":"é"}]},
            {"type":"image","image_url":"data:image/png;base64,AAAA"},
            {"type":"audio","audio_url":"data:audio/wav;base64,AAAA"},
            {"type":"future_input","data":{"snake_case":7}}]});
        let agent = json!({"type":"AgentMessage","id":"agent-1","content":[{"type":"Text","text":"A"},{"type":"Text","text":"B"}],"phase":"final_answer"});
        let recovered = recover(vec![
            started("turn-1"),
            item("turn-1", input.clone()),
            item("turn-1", agent.clone()),
            completed("turn-1"),
        ]);
        let items = &recovered.turns[0]["items"];
        assert_eq!(items[0]["clientId"], "client-1");
        assert_eq!(
            items[0]["content"][0]["text_elements"][0]["byteRange"],
            json!({"start":1,"end":3})
        );
        assert_eq!(items[0]["content"][1]["url"], "data:image/png;base64,AAAA");
        assert_eq!(items[0]["content"][3], input["content"][3]);
        assert_eq!(items[1]["text"], "AB");
        assert_eq!(items[1]["phase"], "final_answer");
        assert_eq!(items[1]["sourcePayload"], agent);
    }

    #[test]
    fn file_changes_empty_add_delete_move_and_raw_execution_fields_survive() {
        let value = json!({"type":"FileChange","id":"patch-1","status":"completed","stdout":"done","changes":{
            "/workspace/z":{"type":"delete","content":""},"/workspace/a":{"type":"add","content":""},
            "/workspace/m":{"type":"update","unified_diff":"-old\n+new","move_path":"/workspace/new"}}});
        let recovered = recover(body(value.clone()));
        let item = &recovered.turns[0]["items"][0];
        assert_eq!(
            item["changes"][0],
            json!({"path":"/workspace/a","kind":{"type":"add"},"diff":""})
        );
        assert_eq!(item["changes"][2]["kind"]["type"], "delete");
        assert_eq!(item["changes"][1]["kind"]["move_path"], "/workspace/new");
        assert_eq!(
            item["changes"][1]["diff"],
            "-old\n+new\n\nMoved to: /workspace/new"
        );
        assert_eq!(item["sourcePayload"], value);
    }

    #[test]
    fn reasoning_web_results_plan_and_future_items_retain_opaque_values() {
        let future = json!({"type":"FutureTool","id":"future-1","unknown":{"raw_key":"<img src=x onerror=alert(1)>"}});
        let results = json!([{"title":"opaque","url":"https://example.test","snippet":"<script>x</script>","future_key":{"snake_case":9}}]);
        let recovered = recover(vec![
            started("turn-1"),
            item(
                "turn-1",
                json!({"type":"Reasoning","id":"r","summary_text":["summary"],"raw_content":["detail"]}),
            ),
            item(
                "turn-1",
                json!({"type":"WebSearch","id":"w","query":"query","action":{"type":"open_page","url":"https://example.test"},"results":results}),
            ),
            item("turn-1", json!({"type":"Plan","id":"p","text":"plan"})),
            item("turn-1", future.clone()),
            completed("turn-1"),
        ]);
        let items = &recovered.turns[0]["items"];
        assert_eq!(items[0]["summary"], json!(["summary"]));
        assert_eq!(items[0]["content"], json!(["detail"]));
        assert_eq!(items[1]["action"]["type"], "openPage");
        assert_eq!(items[1]["results"], results);
        assert_eq!(items[3]["type"], "archivedCoreItem");
        assert_eq!(items[3]["id"], "future-1");
        assert_eq!(items[3]["sourcePayload"], future);
    }

    #[test]
    fn interrupted_and_failed_terminal_events_keep_original_error_and_status() {
        let aborted = event(
            json!({"type":"turn_aborted","turn_id":"turn-1","reason":"interrupted","completed_at":12,"error":{"message":"stopped","codex_error_info":"other"}}),
        );
        let recovered = recover(vec![started("turn-1"), item("turn-1", command()), aborted]);
        assert_eq!(recovered.turns[0]["status"], "interrupted");
        assert_eq!(recovered.turns[0]["error"]["message"], "stopped");
        let mut failure = completed("turn-1");
        failure["payload"]["error"] = json!({"message":"failed","codex_error_info":"other"});
        let recovered = recover(vec![started("turn-1"), item("turn-1", command()), failure]);
        assert_eq!(recovered.turns[0]["status"], "failed");
        assert_eq!(recovered.turns[0]["error"]["message"], "failed");
    }

    #[test]
    fn scope_meta_format_and_identity_mismatches_are_explicit_errors() {
        assert!(
            project(&records(body(command())), "other-thread", "/workspace")
                .unwrap_err()
                .to_string()
                .contains("SessionMeta")
        );
        assert!(project(&records(body(command())), "thread-1", "/other")
            .unwrap_err()
            .to_string()
            .contains("SessionMeta"));
        let mut wrong = item("other-turn", command());
        rejects(
            vec![started("turn-1"), wrong.clone(), completed("turn-1")],
            "identity",
        );
        wrong["payload"]["turn_id"] = json!("turn-1");
        wrong["payload"]["thread_id"] = json!("other-thread");
        rejects(
            vec![started("turn-1"), wrong, completed("turn-1")],
            "identity",
        );
        rejects(
            vec![
                started("turn-1"),
                item("turn-1", command()),
                completed("other-turn"),
            ],
            "different turn",
        );
        let mut started = started("turn-1");
        started["payload"]["turn_attribution"] = json!({"turn_id":"other"});
        rejects(
            vec![started, item("turn-1", command()), completed("turn-1")],
            "attribution",
        );
    }

    #[test]
    fn invalid_json_utf8_and_unknown_history_formats_keep_causes() {
        let error = project(b"{", "thread-1", "/workspace").unwrap_err();
        assert!(error.source().is_some());
        assert!(error.to_string().contains("line 1"));
        assert!(project(&[255], "thread-1", "/workspace")
            .unwrap_err()
            .source()
            .is_some());
        rejects(
            vec![json!({"type":"future_record","payload":{}})],
            "unsupported rollout",
        );
        let bytes = String::from_utf8(records(body(command())))
            .unwrap()
            .replace("paginated", "future");
        assert!(project(bytes.as_bytes(), "thread-1", "/workspace")
            .unwrap_err()
            .to_string()
            .contains("paginated"));
    }

    #[test]
    fn ambiguous_duplicates_incomplete_items_and_temporal_mutations_fail() {
        rejects(
            vec![started("turn-1"), item("turn-1", command())],
            "incomplete turn",
        );
        rejects(
            vec![started("turn-1"), completed("turn-1")],
            "no completed items",
        );
        let conflicting = json!({"type":"Plan","id":"cmd-1","text":"different type"});
        rejects(
            vec![
                started("turn-1"),
                item("turn-1", command()),
                item("turn-1", conflicting),
                completed("turn-1"),
            ],
            "different turn or type",
        );
        rejects(
            vec![started("turn-1"), started("turn-2")],
            "before the previous",
        );
        let incomplete = event(
            json!({"type":"item_started","thread_id":"thread-1","turn_id":"turn-1","item":command()}),
        );
        rejects(
            vec![started("turn-1"), incomplete, completed("turn-1")],
            "incomplete canonical items",
        );
        for mutation in ["future_history_mutation", "context_compacted"] {
            let mut records = body(command());
            records.push(event(json!({"type":mutation,"num_turns":1})));
            rejects(records, "unsupported canonical event");
        }
        rejects(
            vec![json!({"type":"compacted","payload":{"message":"checkpoint"}})],
            "no canonical active turn",
        );
    }

    #[test]
    fn path_encoding_duration_and_text_ranges_are_validated() {
        let mut value = command();
        value["cwd"] = json!("file:///workspace/space%20and%20%C3%A9");
        assert_eq!(
            recover(body(value)).turns[0]["items"][0]["cwd"],
            "/workspace/space and é"
        );
        for bad_path in [
            "relative",
            "file://remote/workspace",
            "file:///bad%xx",
            "file:///bad%00",
            "file:///bad%FF",
        ] {
            let mut value = command();
            value["cwd"] = json!(bad_path);
            assert!(
                project(&records(body(value)), "thread-1", "/workspace").is_err(),
                "{bad_path}"
            );
        }
        let mut value = command();
        value["duration"]["nanos"] = json!(1_000_000_000);
        rejects(body(value), "nanoseconds");
        let mut value = command();
        value["duration"]["secs"] = json!(u64::MAX);
        rejects(body(value), "overflow");
        let bad_input = json!({"type":"UserMessage","id":"u","content":[{"type":"text","text":"é","text_elements":[{"byte_range":{"start":0,"end":1}}]}]});
        rejects(body(bad_input), "UTF-8 text");
        let mut value = command();
        value["parsed_cmd"][0]["path"] = json!("../another/./check.txt");
        assert_eq!(
            recover(body(value)).turns[0]["items"][0]["commandActions"][0]["path"],
            "/another/check.txt"
        );
    }

    #[test]
    fn api_camel_case_payloads_are_preserved_without_rewriting_opaque_json() {
        let value = json!({"type":"dynamicToolCall","id":"api-1","tool":"tool","status":"completed","arguments":{"snake_case":7},"futureMetadata":{"x":1}});
        let recovered = recover(body(value.clone()));
        let mut converted = recovered.turns[0]["items"][0].clone();
        converted
            .as_object_mut()
            .unwrap()
            .remove("sourceCompletionRecords");
        assert_eq!(converted, value);
        rejects(
            body(json!({"type":"agentMessage","id":"missing-text"})),
            "text must be a string",
        );
    }

    #[test]
    fn parent_history_and_subagent_exclusion_cannot_silently_show_wrong_turns() {
        for (field, value) in [
            (
                "history_base",
                json!({"thread_id":"parent","rollout_ordinal":5}),
            ),
            ("subagent_history_start_ordinal", json!(4)),
        ] {
            let mut lines = String::from_utf8(records(body(command())))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            lines[0]["payload"][field] = value;
            let bytes = lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(project(bytes.as_bytes(), "thread-1", "/workspace")
                .unwrap_err()
                .to_string()
                .contains("prefixes"));
        }
    }

    #[test]
    fn repeated_reasoning_snapshots_upsert_in_first_position_with_full_lifecycle() {
        let first = json!({"type":"Reasoning","id":"reasoning-1","summary_text":["first"],"raw_content":[]});
        let second = json!({"type":"Reasoning","id":"reasoning-1","summary_text":["first","second"],"raw_content":["detail"]});
        let mut update = item("turn-1", second);
        update["payload"]["started_at_ms"] = json!(12000);
        update["payload"]["completed_at_ms"] = json!(12999);
        let recovered = recover(vec![
            started("turn-1"),
            item("turn-1", first),
            item("turn-1", command()),
            update,
            completed("turn-1"),
        ]);
        let items = recovered.turns[0]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "reasoning-1");
        assert_eq!(items[0]["summary"], json!(["first", "second"]));
        assert_eq!(
            items[0]["sourceCompletionRecords"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            items[0]["sourceCompletionRecords"][1]["payload"]["completed_at_ms"],
            12999
        );
    }

    #[test]
    fn background_command_completion_updates_its_prior_turn_after_new_turn_started() {
        let mut pending = command();
        pending["status"] = json!("in_progress");
        pending["aggregated_output"] = Value::Null;
        pending["exit_code"] = Value::Null;
        let recovered = recover(vec![
            started("turn-1"),
            item("turn-1", pending),
            completed("turn-1"),
            started("turn-2"),
            item("turn-2", json!({"type":"Plan","id":"plan-2","text":"next"})),
            item("turn-1", command()),
            completed("turn-2"),
        ]);
        assert_eq!(recovered.turns.len(), 2);
        assert_eq!(recovered.turns[0]["items"].as_array().unwrap().len(), 1);
        assert_eq!(recovered.turns[0]["items"][0]["status"], "completed");
        assert_eq!(
            recovered.turns[0]["items"][0]["aggregatedOutput"],
            "marker\n"
        );
        assert_eq!(recovered.turns[1]["items"][0]["id"], "plan-2");
        rejects(
            vec![
                started("turn-1"),
                item("turn-1", command()),
                completed("turn-1"),
                started("turn-2"),
                item("turn-2", command()),
                completed("turn-2"),
            ],
            "different turn or type",
        );
    }

    #[test]
    fn mixed_legacy_rollback_cannot_apply_a_different_lifecycle_history_rule() {
        let second = json!({"type":"Plan","id":"plan-2","text":"removed"});
        let mut records = body(command());
        records.extend([
            started("turn-2"),
            item("turn-2", second.clone()),
            completed("turn-2"),
            event(json!({"type":"thread_rolled_back","num_turns":1})),
        ]);
        rejects(records.clone(), "mixed legacy rollback marker");
        records.push(item("turn-2", second));
        rejects(records, "mixed legacy rollback marker");
        let mut records = body(command());
        records.extend([
            started("turn-2"),
            event(json!({"type":"thread_rolled_back","num_turns":1})),
        ]);
        rejects(records, "mixed legacy rollback marker");
    }

    #[test]
    fn explicit_compaction_item_and_checkpoint_preserve_items_without_invented_ids() {
        let checkpoint =
            json!({"type":"compacted","payload":{"message":"checkpoint","replacement_history":[]}});
        let recovered = recover(vec![
            started("turn-1"),
            item(
                "turn-1",
                json!({"type":"ContextCompaction","id":"compact-1"}),
            ),
            checkpoint,
            completed("turn-1"),
        ]);
        assert_eq!(recovered.turns[0]["items"][0]["id"], "compact-1");
        assert_eq!(recovered.turns[0]["items"][0]["type"], "contextCompaction");
        assert_eq!(
            recovered.turns[0]["sourceCompactions"][0]["payload"]["message"],
            "checkpoint"
        );
    }

    #[test]
    fn paginated_ordinals_reject_regression_duplicates_and_missing_records() {
        let bytes = records(body(command()));
        let original = String::from_utf8(bytes)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        for bad in [Value::Null, json!(0), json!(5)] {
            let mut lines = original.clone();
            lines[2]["ordinal"] = bad;
            let bytes = lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(project(bytes.as_bytes(), "thread-1", "/workspace")
                .unwrap_err()
                .to_string()
                .contains("ordinal"));
        }
    }

    #[test]
    fn empty_failed_or_interrupted_turn_keeps_terminal_error_instead_of_disappearing() {
        let mut terminal = completed("turn-1");
        terminal["payload"]["error"] = json!({"message":"provider failed"});
        let recovered = recover(vec![started("turn-1"), terminal]);
        assert_eq!(recovered.turns[0]["status"], "failed");
        assert_eq!(recovered.turns[0]["items"], json!([]));
        let recovered = recover(vec![
            started("turn-1"),
            event(json!({"type":"turn_aborted","turn_id":"turn-1","reason":"interrupted"})),
        ]);
        assert_eq!(recovered.turns[0]["status"], "interrupted");
    }

    #[test]
    fn first_terminal_status_is_frozen_and_terminal_timestamp_absence_is_preserved() {
        let aborted = event(
            json!({"type":"turn_aborted","turn_id":"turn-1","reason":"interrupted","started_at":11,"completed_at":12}),
        );
        let recovered = recover(vec![
            started("turn-1"),
            item("turn-1", command()),
            aborted,
            completed("turn-1"),
        ]);
        assert_eq!(recovered.turns[0]["status"], "interrupted");
        assert_eq!(recovered.turns[0]["completedAt"], 12);
        assert_eq!(
            recovered.turns[0]["sourceAdditionalTerminalRecords"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let mut terminal = completed("turn-1");
        terminal["payload"]
            .as_object_mut()
            .unwrap()
            .remove("started_at");
        let recovered = recover(vec![started("turn-1"), item("turn-1", command()), terminal]);
        assert!(recovered.turns[0]["startedAt"].is_null());
    }
}
