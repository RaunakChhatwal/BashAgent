#![allow(non_upper_case_globals)]

use std::sync::Arc;
use anyhow::{bail, Context, Result};
use russh::keys::load_secret_key;
use serde_json::Value;
use crate::common::{SSHSession, ToolError::{self, ExecutionError, ToolUseError}};

mod anthropic;
use anthropic::{send_request, stream_response};

mod bash;

mod common;
use common::{Config, Exchange};

mod text_editor;

async fn trigger_cancel(cancel: Arc<tokio::sync::Notify>) {
    loop {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Failed to listen for Ctrl+C: {error}.");
            break;
        }
        cancel.notify_waiters();
    }
}

async fn run_tool(name: &str, input: &Value, ssh_session: &mut SSHSession)
-> Result<String, ToolError> {
    let Some(input) = input.as_object() else {
        crate::bail_tool_use!("The argument(s) must be fields in a JSON object.");
    };

    match name {
        "bash" => crate::bash::run_bash_tool(input, ssh_session).await,
        "text_editor" => crate::text_editor::run_text_editor_tool(input, ssh_session).await,
        tool => crate::bail_tool_use!("Tool {tool} not available.")
    }
}

async fn run_exchange(
    config: &Config,
    prompt: String,
    exchanges: &[Exchange],
    ssh_session: &mut common::SSHSession
) -> Result<Exchange> {
    let mut exchange = Exchange { prompt, response: vec![] };
    let response = send_request(&config, exchanges, &exchange).await?;
    let mut response = stream_response(response).await?;

    while !response.1.is_empty() {
        for tool_use in response.1.as_mut_slice() {
            tool_use.output = match run_tool(&tool_use.name, &tool_use.input, ssh_session).await {
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

    let mut ssh_session =
        russh::client::connect(Default::default(), "192.168.122.72:22", common::Client {}).await?;
    let key = load_secret_key("/home/raunak/.ssh/id_rsa", None).context("Unable to load key pair")?;
    if !ssh_session.authenticate_publickey("raunak", Arc::new(key)).await? {
        bail!("Authentication failure.");
    }

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
            new_exchange = run_exchange(&config, prompt, &exchanges, &mut ssh_session)
                => exchanges.push(new_exchange?)
        }
    }

    ssh_session.disconnect(russh::Disconnect::ByApplication, "", "English").await?;
    Ok(())
}
