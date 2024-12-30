#![allow(non_upper_case_globals)]

use std::sync::Arc;
use anyhow::{Context, Result};
use serde_json::{Map, Value};
use crate::common::ToolError::{self, ExecutionError, ToolUseError};

mod anthropic;
use anthropic::{send_request, stream_response};

pub mod bash_agent {
    tonic::include_proto!("bash_agent");
}
use bash_agent::{tool_runner_client::ToolRunnerClient, BashRequest, BashResponse};

mod common;
use common::{Config, Exchange};

async fn trigger_cancel(cancel: Arc<tokio::sync::Notify>) {
    loop {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Failed to listen for Ctrl+C: {error}.");
            break;
        }
        cancel.notify_waiters();
    }
}

async fn call_bash_tool(input: &Map<String, Value>) -> Result<String, ToolError> {
    let Some(command) = input.get("command").map(Value::as_str).flatten() else {
        crate::bail_tool_use!("The \"command\" argument is required and must be a string.");
    };

    let mut client = ToolRunnerClient::connect("http://[::1]:50051").await
        .context("Failed to connect to GRPC server.").map_err(ExecutionError)?;
    let request = tonic::Request::new(BashRequest { command: command.into() });
    let BashResponse { stdout, stderr, status_code } = client.run_bash_tool(request).await
        .context("Error running bash tool.").map_err(ToolUseError)?.into_inner();
    // let (output, error, status) = run_command(command, ssh_session).await.map_err(ExecutionError)?;
    let mut content = "".to_string();
    if !stdout.is_empty() {
        content.push_str("\n\nstdout:\n");
        content.push_str(&stdout);
    }

    if !stderr.is_empty() {
        content.push_str("\n\nstderr:\n");
        content.push_str(&stderr);
    }

    match status_code {
        None => crate::bail_tool_use!("The command did not exit cleanly.{}", content),
        Some(0) => Ok(content.trim_start().into()),
        Some(status) => crate::bail_tool_use!("Command exited with status {status}:{}", content)
    }
}

async fn call_text_editor_tool(input: &Map<String, Value>) -> Result<String, ToolError> {
    todo!()
}

async fn run_tool(name: &str, input: &Value) -> Result<String, ToolError> {
    let Some(input) = input.as_object() else {
        crate::bail_tool_use!("The argument(s) must be fields in a JSON object.");
    };

    match name {
        "bash" => call_bash_tool(input).await,
        "text_editor" => call_text_editor_tool(input).await,
        tool => crate::bail_tool_use!("Tool {tool} not available.")
    }
}

async fn run_exchange(config: &Config, prompt: String, exchanges: &[Exchange]) -> Result<Exchange> {
    let mut exchange = Exchange { prompt, response: vec![] };
    let response = send_request(&config, exchanges, &exchange).await?;
    let mut response = stream_response(response).await?;

    while !response.1.is_empty() {
        for tool_use in response.1.as_mut_slice() {
            tool_use.output = match run_tool(&tool_use.name, &tool_use.input).await {
                Ok(output) => (output, false),
                Err(ExecutionError(error)) => return Err(error.context("Error executing tool")),
                Err(ToolUseError(error)) => (error.to_string(), true)
            }
        }
        exchange.response.push(response.clone());
        response = stream_response(send_request(&config, exchanges, &exchange).await?).await?;
    }

    exchange.response.push(response);
    Ok(exchange)
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = tokio::fs::read_to_string("config.json").await.context("Couldn't load config.")?;
    let config = serde_json::from_str::<Config>(&config).context("Couldn't parse config.")?;

    let cancel = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(trigger_cancel(Arc::clone(&cancel)));

    let mut exchanges = vec![];
    loop {
        let Some(prompt) = common::input("> ").await.context("Failed to read prompt.")? else {
            println!();
            break;
        };

        tokio::select! {
            _ = cancel.notified() => continue,
            exchange = run_exchange(&config, prompt, &exchanges) => exchanges.push(exchange?)
        }
    }

    Ok(())
}
