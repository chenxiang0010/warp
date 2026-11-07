//! Direct client-side inference for user-configured custom model endpoints.
//!
//! Warp normally proxies custom endpoints through its own server, which makes the agent
//! request depend on a Warp account. This build has none, so when the selected model belongs
//! to a user-configured endpoint we drive the request locally instead: the conversation is
//! converted into an OpenAI-compatible chat completion, and the streamed response is
//! translated back into the `ClientAction`s the native conversation model consumes. Tool
//! calls are emitted as `ToolCall` messages; the client executes them and sends their
//! results in the next request, exactly as it does for a server-driven conversation.

use std::time::SystemTime;

use futures::StreamExt;
use prost_types::Timestamp;
use serde_json::{Value, json};
use uuid::Uuid;
use warp_multi_agent_api as api;
use warp_multi_agent_api::client_action::{AddMessagesToTask, AppendToMessageContent, CreateTask};
use warp_multi_agent_api::message::tool_call::{
    ApplyFileDiffs, FileGlobV2, Grep, ReadFiles, RunShellCommand, Tool,
};
use warp_multi_agent_api::message::{
    AgentOutput, AgentReasoning, Message as MessageKind, ToolCall,
};
use warp_multi_agent_api::response_event::{StreamFinished, StreamInit, stream_finished};
use warp_multi_agent_api::{
    ClientAction, Message, ResponseEvent, Task, client_action, response_event,
};

use super::{ConvertToAPITypeError, RequestParams, ResponseStream};
use crate::ai::agent::AIAgentContext;
use crate::server::server_api::AIApiError;

const AGENT_OUTPUT_TEXT_PATH: &str = "agent_output.text";
const AGENT_REASONING_TEXT_PATH: &str = "agent_reasoning.reasoning";

/// A user-configured custom endpoint selected for this request.
pub(crate) struct LocalEndpoint {
    base_url: String,
    api_key: String,
    model: String,
    schema: i32,
    config_key: String,
}

/// Returns the custom endpoint backing `params`' selected model, if any.
pub(crate) fn endpoint_for_request(params: &RequestParams) -> Option<LocalEndpoint> {
    let providers = params.custom_model_providers.as_ref()?;
    let selected = params.model.as_str();
    // `auto` is the default selection and routes over whatever the user has configured. This
    // build only offers custom endpoints, so resolve it to the first configured model.
    let fallback_to_first = selected == "auto" || selected.is_empty();
    providers.providers.iter().find_map(|provider| {
        let model = provider
            .models
            .iter()
            .find(|model| model.config_key == selected)
            .or_else(|| fallback_to_first.then(|| provider.models.first()).flatten())?;
        Some(LocalEndpoint {
            base_url: provider.base_url.trim_end_matches('/').to_owned(),
            api_key: provider.api_key.clone(),
            model: model.slug.clone(),
            schema: provider.schema,
            config_key: model.config_key.clone(),
        })
    })
}

/// Streams a locally-driven agent turn against `endpoint`.
pub(crate) fn generate_local_output(
    endpoint: LocalEndpoint,
    params: RequestParams,
    cancellation_rx: futures::channel::oneshot::Receiver<()>,
) -> Result<ResponseStream, ConvertToAPITypeError> {
    let stream = async_stream::try_stream! {
        let request_id = Uuid::new_v4().to_string();
        let conversation_id = params
            .conversation_token
            .as_ref()
            .map(|token| token.as_str().to_owned())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let run_id = Uuid::new_v4().to_string();

        yield ResponseEvent {
            r#type: Some(response_event::Type::Init(StreamInit {
                conversation_id,
                request_id: request_id.clone(),
                run_id,
            })),
        };

        let task_id = match params.tasks.last() {
            Some(task) => task.id.clone(),
            None => {
                let id = Uuid::new_v4().to_string();
                yield action_event(vec![client_action(
                    client_action::Action::CreateTask(CreateTask {
                        task: Some(Task {
                            id: id.clone(),
                            ..Default::default()
                        }),
                    }),
                )]);
                id
            }
        };

        let openai_chat_completions = api::request::settings::custom_model_providers::CustomEndpointSchema::OpenaiChatCompletions as i32;
        if endpoint.schema != openai_chat_completions {
            Err(local_error(format!(
                "The custom endpoint '{}' uses an API schema this build cannot call directly. Select OpenAI Chat Completions for the endpoint.",
                endpoint.config_key
            )))?;
        }

        let body = json!({
            "model": endpoint.model,
            "messages": build_messages(&params),
            "stream": true,
            "tools": tool_definitions(),
            "tool_choice": "auto",
        });

        let response = reqwest::Client::new()
            .post(format!("{}/chat/completions", endpoint.base_url))
            .header("Authorization", format!("Bearer {}", endpoint.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|error| local_error(format!("Request to the custom endpoint failed: {error}")))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            Err(local_error(format!(
                "The custom endpoint returned {status}: {body}"
            )))?;
            return;
        }

        let mut upstream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::new();
        let mut open_message: Option<OpenMessage> = None;
        let mut tool_calls: Vec<PendingToolCall> = Vec::new();
        let mut saw_done = false;

        while let Some(chunk) = upstream.next().await {
            let chunk = chunk.map_err(|error| {
                local_error(format!("Failed while reading the custom endpoint stream: {error}"))
            })?;
            buffer.extend_from_slice(&chunk);

            while let Some(line_end) = buffer.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = buffer.drain(..=line_end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    saw_done = true;
                    break;
                }
                let Ok(value) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let Some(choice) = value.get("choices").and_then(|choices| choices.get(0)) else {
                    continue;
                };
                let Some(delta) = choice.get("delta") else {
                    continue;
                };

                if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str)
                    && !reasoning.is_empty()
                {
                    for event in stream_text(
                        &mut open_message,
                        &task_id,
                        &request_id,
                        StreamKind::Reasoning,
                        reasoning,
                    ) {
                        yield event;
                    }
                }

                if let Some(content) = delta.get("content").and_then(Value::as_str)
                    && !content.is_empty()
                {
                    for event in stream_text(
                        &mut open_message,
                        &task_id,
                        &request_id,
                        StreamKind::Output,
                        content,
                    ) {
                        yield event;
                    }
                }

                if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for call in calls {
                        let index = call
                            .get("index")
                            .and_then(Value::as_u64)
                            .unwrap_or_default() as usize;
                        while tool_calls.len() <= index {
                            tool_calls.push(PendingToolCall::default());
                        }
                        let pending = &mut tool_calls[index];
                        if let Some(id) = call.get("id").and_then(Value::as_str)
                            && !id.is_empty()
                        {
                            pending.id = id.to_owned();
                        }
                        if let Some(function) = call.get("function") {
                            if let Some(name) = function.get("name").and_then(Value::as_str)
                                && !name.is_empty()
                            {
                                pending.name = name.to_owned();
                            }
                            if let Some(arguments) =
                                function.get("arguments").and_then(Value::as_str)
                            {
                                pending.arguments.push_str(arguments);
                            }
                        }
                    }
                }
            }

            if saw_done {
                break;
            }
        }

        let produced_text = open_message.is_some();
        open_message = None;
        let _ = open_message;

        if !tool_calls.is_empty() {
            let mut messages = Vec::with_capacity(tool_calls.len());
            for pending in &tool_calls {
                let tool_call_id = if pending.id.is_empty() {
                    format!("call_{}", Uuid::new_v4())
                } else {
                    pending.id.clone()
                };
                let tool = build_tool(&pending.name, &pending.arguments)?;
                messages.push(new_message(
                    &task_id,
                    &request_id,
                    MessageKind::ToolCall(ToolCall {
                        tool_call_id,
                        tool: Some(tool),
                    }),
                ));
            }
            yield action_event(vec![client_action(
                client_action::Action::AddMessagesToTask(AddMessagesToTask {
                    task_id: task_id.clone(),
                    messages,
                }),
            )]);
        } else if !produced_text {
            // Nothing was streamed back (e.g. the model only produced a finish reason).
            // Surface it so the turn does not end with an empty response.
            let message = new_message(
                &task_id,
                &request_id,
                MessageKind::AgentOutput(AgentOutput {
                    text: "The model returned an empty response.".to_owned(),
                }),
            );
            yield action_event(vec![client_action(
                client_action::Action::AddMessagesToTask(AddMessagesToTask {
                    task_id: task_id.clone(),
                    messages: vec![message],
                }),
            )]);
        }

        yield ResponseEvent {
            r#type: Some(response_event::Type::Finished(StreamFinished {
                reason: Some(stream_finished::Reason::Done(stream_finished::Done {})),
                ..Default::default()
            })),
        };
    };

    Ok(Box::pin(stream.take_until(cancellation_rx)))
}

fn local_error(message: String) -> std::sync::Arc<AIApiError> {
    std::sync::Arc::new(AIApiError::Other(anyhow::anyhow!(message)))
}

fn action_event(actions: Vec<ClientAction>) -> ResponseEvent {
    ResponseEvent {
        r#type: Some(response_event::Type::ClientActions(
            response_event::ClientActions { actions },
        )),
    }
}

fn client_action(action: client_action::Action) -> ClientAction {
    ClientAction {
        action: Some(action),
    }
}

fn new_message(task_id: &str, request_id: &str, kind: MessageKind) -> Message {
    Message {
        id: Uuid::new_v4().to_string(),
        task_id: task_id.to_owned(),
        request_id: request_id.to_owned(),
        timestamp: Some(Timestamp::from(SystemTime::now())),
        message: Some(kind),
        ..Default::default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Output,
    Reasoning,
}

struct OpenMessage {
    kind: StreamKind,
    id: String,
}

fn stream_text(
    open_message: &mut Option<OpenMessage>,
    task_id: &str,
    request_id: &str,
    kind: StreamKind,
    text: &str,
) -> Vec<ResponseEvent> {
    let continues_open = open_message.as_ref().is_some_and(|open| open.kind == kind);
    if continues_open {
        let id = open_message
            .as_ref()
            .map(|open| open.id.clone())
            .unwrap_or_default();
        let (message, path) = match kind {
            StreamKind::Output => (
                MessageKind::AgentOutput(AgentOutput {
                    text: text.to_owned(),
                }),
                AGENT_OUTPUT_TEXT_PATH,
            ),
            StreamKind::Reasoning => (
                MessageKind::AgentReasoning(AgentReasoning {
                    reasoning: text.to_owned(),
                    finished_duration: None,
                }),
                AGENT_REASONING_TEXT_PATH,
            ),
        };
        return vec![action_event(vec![client_action(
            client_action::Action::AppendToMessageContent(AppendToMessageContent {
                task_id: task_id.to_owned(),
                message: Some(Message {
                    id,
                    task_id: task_id.to_owned(),
                    request_id: request_id.to_owned(),
                    message: Some(message),
                    ..Default::default()
                }),
                mask: Some(prost_types::FieldMask {
                    paths: vec![path.to_owned()],
                }),
            }),
        )])];
    }

    let (message, _) = match kind {
        StreamKind::Output => (
            MessageKind::AgentOutput(AgentOutput {
                text: text.to_owned(),
            }),
            AGENT_OUTPUT_TEXT_PATH,
        ),
        StreamKind::Reasoning => (
            MessageKind::AgentReasoning(AgentReasoning {
                reasoning: text.to_owned(),
                finished_duration: None,
            }),
            AGENT_REASONING_TEXT_PATH,
        ),
    };
    let message = new_message(task_id, request_id, message);
    *open_message = Some(OpenMessage {
        kind,
        id: message.id.clone(),
    });
    vec![action_event(vec![client_action(
        client_action::Action::AddMessagesToTask(AddMessagesToTask {
            task_id: task_id.to_owned(),
            messages: vec![message],
        }),
    )])]
}

#[derive(Default)]
struct PendingToolCall {
    id: String,
    name: String,
    arguments: String,
}

fn build_tool(name: &str, arguments: &str) -> Result<Tool, std::sync::Arc<AIApiError>> {
    let arguments: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    let string = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let boolean = |key: &str| arguments.get(key).and_then(Value::as_bool).unwrap_or(false);
    let integer = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_i64)
            .unwrap_or_default()
    };

    match name {
        "run_shell_command" =>
        {
            #[allow(deprecated)]
            Ok(Tool::RunShellCommand(RunShellCommand {
                command: string("command"),
                is_read_only: boolean("is_read_only"),
                is_risky: boolean("is_risky"),
                uses_pager: false,
                citations: vec![],
                risk_category: api::RiskCategory::Unspecified as i32,
                wait_until_complete_value: None,
            }))
        }
        "read_files" => {
            let files = arguments
                .get("files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|file| {
                            let name = file.get("name").and_then(Value::as_str)?;
                            let line_ranges = file
                                .get("line_ranges")
                                .and_then(Value::as_array)
                                .map(|ranges| {
                                    ranges
                                        .iter()
                                        .filter_map(|range| {
                                            Some(api::FileContentLineRange {
                                                start: range.get("start").and_then(Value::as_u64)?
                                                    as u32,
                                                end: range.get("end").and_then(Value::as_u64)?
                                                    as u32,
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            Some(api::message::tool_call::read_files::File {
                                name: name.to_owned(),
                                line_ranges,
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Ok(Tool::ReadFiles(ReadFiles { files }))
        }
        "grep" => {
            let queries = arguments
                .get("queries")
                .and_then(Value::as_array)
                .map(|queries| {
                    queries
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Ok(Tool::Grep(Grep {
                queries,
                path: string("path"),
            }))
        }
        "file_glob" => {
            let patterns = arguments
                .get("patterns")
                .and_then(Value::as_array)
                .map(|patterns| {
                    patterns
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Ok(Tool::FileGlobV2(FileGlobV2 {
                patterns,
                search_dir: string("search_dir"),
                max_matches: integer("max_matches") as i32,
                max_depth: integer("max_depth") as i32,
                min_depth: integer("min_depth") as i32,
            }))
        }
        "apply_file_diffs" => {
            let diffs = arguments
                .get("diffs")
                .and_then(Value::as_array)
                .map(|diffs| {
                    diffs
                        .iter()
                        .map(|diff| api::message::tool_call::apply_file_diffs::FileDiff {
                            file_path: diff
                                .get("file_path")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            search: diff
                                .get("search")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            replace: diff
                                .get("replace")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let new_files = arguments
                .get("new_files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .map(|file| api::message::tool_call::apply_file_diffs::NewFile {
                            file_path: file
                                .get("file_path")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            content: file
                                .get("content")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            allow_overwrite: file
                                .get("allow_overwrite")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let deleted_files = arguments
                .get("deleted_files")
                .and_then(Value::as_array)
                .map(|files| {
                    files
                        .iter()
                        .map(
                            |file| api::message::tool_call::apply_file_diffs::DeleteFile {
                                file_path: file
                                    .get("file_path")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            },
                        )
                        .collect()
                })
                .unwrap_or_default();
            Ok(Tool::ApplyFileDiffs(ApplyFileDiffs {
                summary: string("summary"),
                diffs,
                new_files,
                deleted_files,
                v4a_updates: vec![],
            }))
        }
        other => Err(local_error(format!(
            "The model requested an unsupported tool: {other}"
        ))),
    }
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "type": "function",
            "function": {
                "name": "run_shell_command",
                "description": "Run a shell command in the user's terminal and return its output. Use this to inspect the system, run builds and tests, and perform any command-line work.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The shell command to run."
                        },
                        "is_read_only": {
                            "type": "boolean",
                            "description": "True when the command only reads state and has no side effects."
                        },
                        "is_risky": {
                            "type": "boolean",
                            "description": "True when the command is destructive or otherwise risky."
                        }
                    },
                    "required": ["command"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "read_files",
                "description": "Read the contents of files on the user's machine.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": {
                                        "type": "string",
                                        "description": "Path to the file to read."
                                    },
                                    "line_ranges": {
                                        "type": "array",
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "start": {"type": "integer"},
                                                "end": {"type": "integer"}
                                            },
                                            "required": ["start", "end"]
                                        }
                                    }
                                },
                                "required": ["name"]
                            }
                        }
                    },
                    "required": ["files"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "grep",
                "description": "Search files for text patterns.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "queries": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "The search terms or patterns to look for."
                        },
                        "path": {
                            "type": "string",
                            "description": "The relative path to the file or directory to search in."
                        }
                    },
                    "required": ["queries", "path"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "file_glob",
                "description": "Find files whose names match glob patterns.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "patterns": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "The patterns to match file names against. Supports ?, *, []."
                        },
                        "search_dir": {
                            "type": "string",
                            "description": "The relative path to the directory to search in."
                        },
                        "max_matches": {
                            "type": "integer",
                            "description": "The maximum number of matches to return. Zero means no limit."
                        },
                        "max_depth": {
                            "type": "integer",
                            "description": "The maximum depth to search. Zero means no limit."
                        }
                    },
                    "required": ["patterns", "search_dir"]
                }
            }
        }),
        json!({
            "type": "function",
            "function": {
                "name": "apply_file_diffs",
                "description": "Edit files by replacing existing text, creating new files, or deleting files.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "summary": {
                            "type": "string",
                            "description": "A short summary of the changes."
                        },
                        "diffs": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_path": {"type": "string"},
                                    "search": {
                                        "type": "string",
                                        "description": "The exact content to replace."
                                    },
                                    "replace": {
                                        "type": "string",
                                        "description": "The content that replaces the search text."
                                    }
                                },
                                "required": ["file_path", "search", "replace"]
                            }
                        },
                        "new_files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_path": {"type": "string"},
                                    "content": {"type": "string"},
                                    "allow_overwrite": {"type": "boolean"}
                                },
                                "required": ["file_path", "content"]
                            }
                        },
                        "deleted_files": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "file_path": {"type": "string"}
                                },
                                "required": ["file_path"]
                            }
                        }
                    },
                    "required": ["summary"]
                }
            }
        }),
    ]
}

fn build_messages(params: &RequestParams) -> Vec<Value> {
    let mut messages = vec![json!({
        "role": "system",
        "content": system_prompt(params),
    })];

    for task in &params.tasks {
        for message in &task.messages {
            append_message(&mut messages, message);
        }
    }

    for input in &params.input {
        if let Some(text) = input_text(input) {
            messages.push(json!({"role": "user", "content": text}));
        }
    }

    messages
}

fn append_message(messages: &mut Vec<Value>, message: &Message) {
    let Some(kind) = message.message.as_ref() else {
        return;
    };
    match kind {
        MessageKind::UserQuery(query) => {
            if !query.query.trim().is_empty() {
                messages.push(json!({"role": "user", "content": query.query}));
            }
        }
        MessageKind::AgentOutput(output) => {
            if output.text.is_empty() {
                return;
            }
            match messages.last_mut() {
                Some(last) if last["role"] == "assistant" => {
                    let existing = last["content"].as_str().unwrap_or_default().to_owned();
                    let separator = if existing.is_empty() { "" } else { "\n\n" };
                    last["content"] =
                        Value::String(format!("{existing}{separator}{}", output.text));
                }
                _ => messages.push(json!({"role": "assistant", "content": output.text})),
            }
        }
        MessageKind::ToolCall(call) => {
            let Some(tool) = call.tool.as_ref() else {
                return;
            };
            let (name, arguments) = tool_call_arguments(tool);
            let tool_call = json!({
                "id": call.tool_call_id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": arguments.to_string(),
                },
            });
            match messages.last_mut() {
                Some(last) if last["role"] == "assistant" => {
                    if !last["tool_calls"].is_array() {
                        last["tool_calls"] = json!([]);
                    }
                    last["tool_calls"]
                        .as_array_mut()
                        .expect("just initialized")
                        .push(tool_call);
                }
                _ => messages.push(json!({
                    "role": "assistant",
                    "content": Value::Null,
                    "tool_calls": [tool_call],
                })),
            }
        }
        MessageKind::ToolCallResult(result) => {
            let content = result
                .result
                .as_ref()
                .map(tool_result_content)
                .unwrap_or_else(|| "The tool call did not produce a result.".to_owned());
            messages.push(json!({
                "role": "tool",
                "tool_call_id": result.tool_call_id,
                "content": content,
            }));
        }
        _ => {}
    }
}

fn tool_call_arguments(tool: &Tool) -> (String, Value) {
    match tool {
        Tool::RunShellCommand(command) => (
            "run_shell_command".to_owned(),
            json!({
                "command": command.command,
                "is_read_only": command.is_read_only,
                "is_risky": command.is_risky,
            }),
        ),
        Tool::ReadFiles(read_files) => (
            "read_files".to_owned(),
            json!({
                "files": read_files
                    .files
                    .iter()
                    .map(|file| json!({
                        "name": file.name,
                        "line_ranges": file
                            .line_ranges
                            .iter()
                            .map(|range| json!({"start": range.start, "end": range.end}))
                            .collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>(),
            }),
        ),
        Tool::Grep(grep) => (
            "grep".to_owned(),
            json!({"queries": grep.queries, "path": grep.path}),
        ),
        Tool::FileGlobV2(glob) => (
            "file_glob".to_owned(),
            json!({
                "patterns": glob.patterns,
                "search_dir": glob.search_dir,
                "max_matches": glob.max_matches,
                "max_depth": glob.max_depth,
                "min_depth": glob.min_depth,
            }),
        ),
        Tool::ApplyFileDiffs(diffs) => (
            "apply_file_diffs".to_owned(),
            json!({
                "summary": diffs.summary,
                "diffs": diffs
                    .diffs
                    .iter()
                    .map(|diff| json!({
                        "file_path": diff.file_path,
                        "search": diff.search,
                        "replace": diff.replace,
                    }))
                    .collect::<Vec<_>>(),
                "new_files": diffs
                    .new_files
                    .iter()
                    .map(|file| json!({
                        "file_path": file.file_path,
                        "content": file.content,
                        "allow_overwrite": file.allow_overwrite,
                    }))
                    .collect::<Vec<_>>(),
                "deleted_files": diffs
                    .deleted_files
                    .iter()
                    .map(|file| json!({"file_path": file.file_path}))
                    .collect::<Vec<_>>(),
            }),
        ),
        other => (
            "unknown_tool".to_owned(),
            json!({"tool": format!("{other:?}")}),
        ),
    }
}

fn tool_result_content(result: &api::message::tool_call_result::Result) -> String {
    use api::message::tool_call_result::Result as ToolResult;

    match result {
        ToolResult::RunShellCommand(result) => match &result.result {
            Some(api::run_shell_command_result::Result::CommandFinished(finished)) => {
                format!(
                    "exit_code: {}\noutput:\n{}",
                    finished.exit_code, finished.output
                )
            }
            Some(api::run_shell_command_result::Result::LongRunningCommandSnapshot(snapshot)) => {
                format!(
                    "The command is still running. Current output:\n{}",
                    snapshot.output
                )
            }
            Some(api::run_shell_command_result::Result::PermissionDenied(_)) => {
                "The command was denied permission to run.".to_owned()
            }
            Some(api::run_shell_command_result::Result::TerminalBusy(_)) => {
                "The terminal was busy and the command was not started.".to_owned()
            }
            None => "The command did not produce a result.".to_owned(),
        },
        ToolResult::ReadFiles(result) => match &result.result {
            Some(api::read_files_result::Result::TextFilesSuccess(success)) => {
                let mut output = String::new();
                for file in &success.files {
                    output.push_str(&format!("File: {}\n{}\n\n", file.file_path, file.content));
                }
                for failed in &success.failed_reads {
                    output.push_str(&format!(
                        "Failed to read {}: {}\n",
                        failed.path, failed.message
                    ));
                }
                output
            }
            Some(api::read_files_result::Result::Error(error)) => error.message.clone(),
            _ => "The files could not be read.".to_owned(),
        },
        ToolResult::Grep(result) => match &result.result {
            Some(api::grep_result::Result::Success(success)) => {
                let mut output = String::new();
                for file in &success.matched_files {
                    output.push_str(&format!("{}:\n", file.file_path));
                    for line in &file.matched_lines {
                        output.push_str(&format!("  line {}\n", line.line_number));
                    }
                }
                output
            }
            Some(api::grep_result::Result::Error(error)) => error.message.clone(),
            None => "No matches were found.".to_owned(),
        },
        ToolResult::FileGlobV2(result) => match &result.result {
            Some(api::file_glob_v2_result::Result::Success(success)) => success
                .matched_files
                .iter()
                .map(|file| file.file_path.clone())
                .collect::<Vec<_>>()
                .join("\n"),
            Some(api::file_glob_v2_result::Result::Error(error)) => error.message.clone(),
            None => "No files matched.".to_owned(),
        },
        ToolResult::ApplyFileDiffs(result) => match &result.result {
            Some(api::apply_file_diffs_result::Result::Success(success)) => {
                let mut output = String::from("Changes applied.");
                for file in &success.updated_files_v2 {
                    if let Some(file) = &file.file {
                        output.push_str(&format!("\nUpdated {}", file.file_path));
                    }
                }
                for file in &success.deleted_files {
                    output.push_str(&format!("\nDeleted {}", file.file_path));
                }
                output
            }
            Some(api::apply_file_diffs_result::Result::Error(error)) => error.message.clone(),
            None => "The changes could not be applied.".to_owned(),
        },
        ToolResult::Cancel(_) => "The tool call was cancelled.".to_owned(),
        other => format!("{other:?}"),
    }
}

fn input_text(input: &crate::ai::agent::AIAgentInput) -> Option<String> {
    use crate::ai::agent::AIAgentInput;

    match input {
        AIAgentInput::UserQuery { query, .. } => Some(query.clone()),
        AIAgentInput::AutoCodeDiffQuery { query, .. } => Some(query.clone()),
        AIAgentInput::CreateNewProject { query, .. } => Some(query.clone()),
        AIAgentInput::SummarizeConversation { prompt, .. } => Some(
            prompt
                .clone()
                .unwrap_or_else(|| "Summarize the conversation so far.".to_owned()),
        ),
        AIAgentInput::InvokeSkill {
            user_query, skill, ..
        } => Some(
            user_query
                .as_ref()
                .map(|query| query.query.clone())
                .unwrap_or_else(|| format!("Use the {} skill.", skill.name)),
        ),
        _ => None,
    }
}

fn system_prompt(params: &RequestParams) -> String {
    let mut prompt = String::from(
        "You are an AI agent running inside Warp, a terminal emulator. You help the user \
         with software engineering tasks by inspecting their machine and running commands.\n\n\
         Use the provided tools to gather information and make changes. Prefer running \
         commands over guessing. When you are done, reply with a concise summary of what you \
         did and what you found. Format code and paths as markdown.\n",
    );

    let mut environment = Vec::new();
    for input in &params.input {
        let Some(contexts) = input.context() else {
            continue;
        };
        for context in contexts {
            match context {
                AIAgentContext::Directory { pwd, home_dir, .. } => {
                    if let Some(pwd) = pwd {
                        environment.push(format!("Working directory: {pwd}"));
                    }
                    if let Some(home_dir) = home_dir {
                        environment.push(format!("Home directory: {home_dir}"));
                    }
                }
                AIAgentContext::ExecutionEnvironment(execution) => {
                    environment.push(format!("Operating system: {:?}", execution.os));
                    environment.push(format!("Shell: {}", execution.shell_name));
                    if let Some(version) = &execution.shell_version {
                        environment.push(format!("Shell version: {version}"));
                    }
                }
                AIAgentContext::CurrentTime { current_time } => {
                    environment.push(format!("Current time: {current_time}"));
                }
                AIAgentContext::Git { head, branch } => {
                    let mut git = format!("Git HEAD: {head}");
                    if let Some(branch) = branch {
                        git.push_str(&format!(" (branch {branch})"));
                    }
                    environment.push(git);
                }
                AIAgentContext::ProjectRules {
                    root_path,
                    active_rules,
                    ..
                } => {
                    environment.push(format!("Project rules from {root_path}:"));
                    for rule in active_rules {
                        if let ai::agent::action_result::AnyFileContent::StringContent(content) =
                            &rule.content
                        {
                            environment.push(content.clone());
                        }
                    }
                }
                AIAgentContext::File(file) => {
                    if let ai::agent::action_result::AnyFileContent::StringContent(content) =
                        &file.content
                    {
                        environment.push(format!("File {}:\n{}", file.file_name, content));
                    }
                }
                _ => {}
            }
        }
    }

    if !environment.is_empty() {
        prompt.push_str("\nEnvironment:\n");
        for line in environment {
            prompt.push_str(&format!("- {line}\n"));
        }
    }

    prompt
}
