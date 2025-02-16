use anyhow::{bail, Context, Result};
use eventsource_stream::{Event, Eventsource};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use crate::common::{bash_tool, text_editor_tool, write, Exchange, ModelParams, Tool, ToolUse};

fn serialize_assistant_response(message: &str, tool_uses: &[ToolUse]) -> Value {
    let mut content_block = vec![];
    if !message.is_empty() {
        content_block.push(json!({ "type": "text", "text": message }));                
    }
    for ToolUse { name, id, input, .. } in tool_uses {
        content_block.push(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input
        }));
    }

    json!({ "role": "assistant", "content": content_block })
}

fn serialize_tool_results(tool_uses: &[ToolUse]) -> Value {
    let mut tool_results = vec![];
    for ToolUse { id, output, .. } in tool_uses {
        let content = output.as_ref().map(|(content, _)| content.as_str());
        let is_error = output.as_ref().map(|(_, is_error)| is_error);
        tool_results.push(json!({
            "type": "tool_result",
            "tool_use_id": id,
            "content": content.unwrap_or("Operation cancelled by user"),
            "is_error": is_error.unwrap_or(&true)
        }));
    }

    json!({ "role": "user", "content": tool_results })
}

fn serialize_tool(tool: Tool) -> Value {
    let input_schema =
        serde_json::from_str::<Value>(tool.input_schema).expect("Schema wasn't valid json");

    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": input_schema
    })
}

fn build_request_body(exchanges: &[Exchange], current: &Exchange, params: &ModelParams) -> Value {
    let mut messages = vec![];
    for Exchange { prompt, response } in exchanges.into_iter().chain([current]) {
        messages.push(json!({ "role": "user", "content": prompt }));
        for (message, tool_uses) in response {
            messages.push(serialize_assistant_response(message, tool_uses));
            if !tool_uses.is_empty() {
                messages.push(serialize_tool_results(tool_uses));
            }
        }
    }

    let ModelParams { model, max_tokens, temperature, .. } = params;
    return json!({
        "model": model,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "stream": true,
        "system": include_str!("resources/system-prompt.txt"),
        "tools": ([bash_tool, text_editor_tool]).map(serialize_tool),
        "messages": messages
    });
}

pub async fn send_request(
    exchanges: &[Exchange], current: &Exchange, params: &ModelParams
) -> Result<reqwest::Response> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("Environment variable ANTHROPIC_API_KEY not set.")?;
    headers.insert("x-api-key", HeaderValue::from_str(&api_key)?);
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

    let url = "https://api.anthropic.com/v1/messages";
    let body = build_request_body(exchanges, current, params);
    let request = reqwest::Client::new().post(url).headers(headers).json(&body);

    let response = request.send().await?;
    let status = response.status();
    if status != reqwest::StatusCode::OK {
        let message = response.text().await.unwrap_or_else(|error| format!("{error:?}"));
        bail!("Failed with status code: {status}: {message}");
    }
    return Ok(response);
}

fn parse_tool_use_content_block_start(response: &Value) -> Result<ToolUse> {
    let name = response["content_block"]["name"].as_str().context("Tool name not found.")?.into();
    let id = response["content_block"]["id"].as_str().context("Tool use id not found.")?.into();
    Ok(ToolUse { name, id, ..Default::default() })
}

async fn stream_response_message(event: Event, message: &mut String) -> Result<Option<ToolUse>> {
    let Event { event, data, .. } = event;
    let response = serde_json::from_str::<Value>(&data).context("Data not valid JSON.")?;
    if event == "error" {
        bail!("{}", response["message"].as_str().unwrap_or("Failed to fetch response chunk"));
    }

    if response["content_block"]["type"].as_str() == Some("tool_use") {
        assert_eq!(event, "content_block_start",
            "The first encountered tool use block should be of type content_block_start.");
        return parse_tool_use_content_block_start(&response).map(Some);
    } else if event == "content_block_delta" {
        let tokens =
            response["delta"]["text"].as_str().context("Tokens not found in content block.")?;
        message.push_str(tokens);
        write(tokens).await.context("Failed to print tokens.")?;
    } else if event == "content_block_stop" {
        print!("\n\n");
    }

    Ok(None)
}

fn stream_tool_use(
    Event { event, data, .. }: Event, partial_json: &mut String, prev_tool_use: &mut ToolUse
) -> Result<Option<ToolUse>> {
    let response = serde_json::from_str::<Value>(&data).context("Data not valid JSON.")?;
    if event == "error" {
        bail!("{}", response["message"].as_str().unwrap_or("Failed to fetch response chunk"));
    }

    if event == "content_block_start"{
        partial_json.clear();
        return parse_tool_use_content_block_start(&response).map(Some);
    } else if event == "content_block_delta" {
        let fragment = response["delta"]["partial_json"].as_str().context("Tool input not found.")?;
        partial_json.push_str(fragment);
    } else if event == "content_block_stop" {
        prev_tool_use.input =
            serde_json::from_str(&partial_json).context("Tool input not valid JSON.")?;
    }

    Ok(None)
}

// first stream the message and print the tokens, then stream the tool uses
pub async fn stream_response(
    response: reqwest::Response, message: &mut String, tool_uses: &mut Vec<ToolUse>
) -> Result<()> {
    let mut partial_json = "".to_string();
    let mut eventsource = response.bytes_stream().eventsource();

    while let Some(event) = eventsource.next().await {
        let event = event.context("Failed to fetch tokens.")?;
        if let Some(tool_use) = stream_response_message(event, message).await? {
            tool_uses.push(tool_use);
            break;
        }
    }

    while let Some(event) = eventsource.next().await {
        let event = event.context("Failed to fetch tokens.")?;
        let prev_tool_use = tool_uses.last_mut().unwrap();
        if let Some(tool_use) = stream_tool_use(event, &mut partial_json, prev_tool_use)? {
            tool_uses.push(tool_use);
        }
    }

    return Ok(());
}