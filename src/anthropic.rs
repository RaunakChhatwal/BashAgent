use anyhow::{bail, Context, Result};
use eventsource_stream::{Event, Eventsource};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use serde_json::{json, Value};
use crate::common::{write, Config, Exchange, ToolUse};

fn serialize_assistant_response(message: &str, tool_use: &[ToolUse]) -> Value {
    let mut content_block = vec![];
    if !message.is_empty() {
        content_block.push(json!({ "type": "text", "text": message }));                
    }
    for ToolUse { name, id, input, .. } in tool_use {
        content_block.push(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input
        }));
    }

    json!({ "role": "assistant", "content": content_block })
}

fn serialize_tool_results(tool_use: &[ToolUse]) -> Value {
    let serialize_tool_result = |ToolUse { id, output: (content, is_error), .. }: &_| json!({
        "type": "tool_result",
        "tool_use_id": id,
        "content": content,
        "is_error": is_error
    });

    let tool_results = tool_use.into_iter().map(serialize_tool_result).collect::<Vec<_>>();
    json!({ "role": "user", "content": tool_results })
}

fn build_request_body(config: &Config, exchanges: &[Exchange], current: &Exchange)
-> serde_json::Value {
    let mut messages = vec![];
    for Exchange { prompt, response } in exchanges.into_iter().chain([current]) {
        messages.push(json!({ "role": "user", "content": prompt }));
        for (message, tool_use) in response {
            messages.push(serialize_assistant_response(message, tool_use));
            if !tool_use.is_empty() {
                messages.push(serialize_tool_results(tool_use));
            }
        }
    }

    return json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "temperature": config.temperature,
        "stream": true,
        "system": config.system_prompt,
        "tools": [crate::bash::bash], // , crate::text_editor::text_editor],
        "messages": messages
    });
}

pub async fn send_request(config: &Config, exchanges: &[Exchange], current: &Exchange)
-> Result<reqwest::Response> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert("x-api-key", HeaderValue::from_str(&config.api_key)?);
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

    let url = "https://api.anthropic.com/v1/messages";
    let body = build_request_body(config, exchanges, current).to_string();
    let request = reqwest::Client::new().post(url).headers(headers).body(body);

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

async fn stream_response_message(Event { event, data, .. }: Event, message: &mut String)
-> Result<Option<ToolUse>> {
    let response = serde_json::from_str::<Value>(&data).context("Data not valid JSON.")?;

    if response["content_block"]["type"].as_str() == Some("tool_use") {
        assert_eq!(event, "content_block_start",
            "The first encountered tool use block should be of type content_block_start.");
        return parse_tool_use_content_block_start(&response).map(Some);
    } else if event == "content_block_delta" {
        let tokens =
            response["delta"]["text"].as_str().context("Tokens not found in content block.")?;
        message.push_str(tokens);
        write(tokens).await.context("Failed to output tokens.")?;
    } else if event == "content_block_stop" {
        println!();
    }

    Ok(None)
}

fn stream_tool_use(
    Event { event, data, .. }: Event,
    partial_json: &mut String,
    prev_tool_use: &mut ToolUse
) -> Result<Option<ToolUse>> {
    let response = serde_json::from_str::<Value>(&data).context("Data not valid JSON.")?;

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

pub async fn stream_response(response: reqwest::Response) -> Result<(String, Vec<ToolUse>)> {
    let mut message = "".to_string();
    let mut tool_uses = vec![];
    let mut partial_json = "".to_string();
    let mut eventsource = response.bytes_stream().eventsource();

    while let Some(event) = eventsource.next().await {
        let event = event.context("Failed to fetch tokens.")?;
        if let Some(tool_use) = stream_response_message(event, &mut message).await? {
            tool_uses.push(tool_use);
            break;
        }
    }

    while let Some(event) = eventsource.next().await {
        let event = event.context("Failed to fetch tokens.")?;
        let prev_tool_use =
            tool_uses.last_mut().expect("The previous while loop appends to tool_uses.");
        if let Some(tool_use) = stream_tool_use(event, &mut partial_json, prev_tool_use)? {
            tool_uses.push(tool_use);
        }
    }

    Ok((message, tool_uses))
}
