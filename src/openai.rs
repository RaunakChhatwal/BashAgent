use anyhow::{bail, Context, Result};
use eventsource_stream::{Event, Eventsource};
use futures::{Stream, StreamExt, TryStreamExt};
use serde_json::{json, Value};
use crate::common::{bash_tool, text_editor_tool, write, Exchange, ModelParams, Tool, ToolUse};

fn serialize_assistant_response(message: &str, tool_uses: &[ToolUse]) -> Value {
    let tool_calls = tool_uses.iter().map(|ToolUse { name, id, input, .. }: &_| json!({
        "type": "function",
        "id": id,
        "function": { "name": name, "arguments": input.to_string() }
    })).collect::<Vec<_>>();

    let mut response = json!({ "role": "assistant", "content": message });
    if !tool_calls.is_empty() {
        response.as_object_mut().unwrap().insert("tool_calls".into(), Value::Array(tool_calls));
    }
    response
}

fn serialize_tool_result(ToolUse { id, output, .. }: &ToolUse) -> Value {
    let content = output.as_ref().map(|(content, _)| content.as_str());
    json!({
        "role": "tool",
        "tool_call_id": id,
        "content": content.unwrap_or("Operation cancelled by user"),
    })
}

fn serialize_tool(tool: Tool) -> Value {
    let input_schema =
        serde_json::from_str::<Value>(tool.input_schema).expect("Schema isn't valid json");

    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": input_schema
        },
        "strict": true
    })
}

fn build_request_body(exchanges: &[Exchange], current: &Exchange, params: &ModelParams) -> Value {
    let mut messages =
        vec![json!({ "role": "system", "content": include_str!("resources/system-prompt.txt") })];
    for Exchange { prompt, response } in exchanges.into_iter().chain([current]) {
        messages.push(json!({ "role": "user", "content": prompt }));
        for (message, tool_uses) in response {
            messages.push(serialize_assistant_response(message, tool_uses));
            for tool_use in tool_uses {
                messages.push(serialize_tool_result(tool_use));
            }
        }
    }

    let ModelParams { model, max_tokens, temperature, .. } = params;
    let mut response = json!({
        "model": model,
        "max_completion_tokens": max_tokens,
        "temperature": temperature,
        "stream": true,
        "tools": ([bash_tool, text_editor_tool]).map(serialize_tool),
        "messages": messages
    });
    if model == "o1" || model.starts_with("o3") {
        response.as_object_mut().unwrap().insert("reasoning_effort".into(), json!("high"));
    }
    response
}

pub async fn send_request(
    exchanges: &[Exchange], current: &Exchange, params: &ModelParams
) -> Result<reqwest::Response> {
    let url = "https://api.openai.com/v1/chat/completions";
    let api_key =
        std::env::var("OPENAI_API_KEY").context("Environment variable OPENAI_API_KEY not set.")?;
    let body = build_request_body(exchanges, current, params);
    let request = reqwest::Client::new().post(url).bearer_auth(api_key).json(&body);

    let response = request.send().await?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        let message = response.text().await.unwrap_or_else(|error| format!("{error:?}"));
        bail!("Failed with status code: {status}: {message}");
    }
    return Ok(response);
}

fn parse_tool_call(tool_call: &Value) -> Result<ToolUse> {
    let name = tool_call["function"]["name"].as_str().context("Tool name not found.")?.into();
    let id = tool_call["id"].as_str().context("Tool call id not found.")?.into();
    Ok(ToolUse { name, id, ..Default::default() })
}

async fn stream_response_message(event: Event, message: &mut String) -> Result<Option<ToolUse>> {
    let Event { event, data, .. } = event;
    if data.trim() == "[DONE]" {
        return Ok(None);
    }
    let response = serde_json::from_str::<Value>(&data).context("Data not valid JSON.")?;
    if event == "error" {
        bail!("{}", response["message"].as_str().unwrap_or("Failed to fetch response chunk"));
    }

    if let Some(tool_call) = response["choices"][0]["delta"]["tool_calls"].get(0) {
        return parse_tool_call(tool_call).map(Some);
    } else if let Some(tokens) = response["choices"][0]["delta"]["content"].as_str() {
        message.push_str(tokens);
        write(tokens).await.context("Failed to print tokens.")?;
    } else {
        print!("\n\n");
    }

    Ok(None)
}

fn stream_tool_call(event: Event, partial_json: &mut String) -> Result<Option<ToolUse>> {
    let Event { event, data, .. } = event;
    if data.trim() == "[DONE]" {
        return Ok(None);
    }
    let response = serde_json::from_str::<Value>(&data).context("Data not valid JSON.")?;
    if event == "error" {
        bail!("{}", response["message"].as_str().unwrap_or("Failed to fetch response chunk"));
    }

    let Some(tool_call_or_args) = response["choices"][0]["delta"]["tool_calls"].get(0) else {
        return Ok(None);
    };

    if let Ok(tool_call) = parse_tool_call(tool_call_or_args) {
        Ok(Some(tool_call))
    } else {
        let args = tool_call_or_args["function"]["arguments"].as_str()
            .context("Tool call arguments not found")?;
        partial_json.push_str(args);
        Ok(None)
    }
}

async fn stream_tool_calls(
    mut eventsource: impl Stream<Item = Result<Event>> + Unpin,
    tool_use: ToolUse,
    tool_uses: &mut Vec<ToolUse>
) -> Result<()> {
    tool_uses.push(tool_use);
    let mut prev_tool_use = tool_uses.last_mut().unwrap();
    let mut partial_json = "".to_string();

    while let Some(event) = eventsource.next().await {
        let event = event.context("Failed to fetch tokens.")?;
        let Some(tool_use) = stream_tool_call(event, &mut partial_json)? else {
            continue;
        };

        prev_tool_use.input =
            serde_json::from_str(&partial_json).context("Tool input not valid JSON.")?;
        partial_json.clear();
        tool_uses.push(tool_use);
        prev_tool_use = tool_uses.last_mut().unwrap();
    }

    prev_tool_use.input =
        serde_json::from_str(&partial_json).context("Tool input not valid JSON.")?;
    return Ok(());
}

// first stream the message and print the tokens, then stream the tool uses
pub async fn stream_response(
    response: reqwest::Response, message: &mut String, tool_uses: &mut Vec<ToolUse>
) -> Result<()> {
    let mut eventsource = response.bytes_stream().eventsource();

    while let Some(event) = eventsource.next().await {
        let event = event.context("Failed to fetch tokens.")?;
        if let Some(tool_use) = stream_response_message(event, message).await? {
            stream_tool_calls(eventsource.map_err(Into::into), tool_use, tool_uses).await?;
            break;
        }
    }

    return Ok(());
}