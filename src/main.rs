#![allow(non_upper_case_globals)]

mod anthropic;
mod client;
mod common;

use std::sync::Arc;
use anyhow::{Error, Context, Result};
use anthropic::{send_request, stream_response};
use common::{Config, Exchange};

async fn run_exchange(config: &Config, prompt: String, exchanges: &[Exchange]) -> Result<Exchange> {
    let mut exchange = Exchange { prompt, response: vec![] };
    let response = send_request(&config, exchanges, &exchange).await?;
    let mut response = stream_response(response).await?;

    while !response.1.is_empty() {
        for tool_use in response.1.as_mut_slice() {
            println!("\nRunning tool {} with input:\n{}", &tool_use.name, &tool_use.input);
            let result = client::call_tool(&tool_use.name, &tool_use.input).await;
            tool_use.output = match result.map_err(Error::downcast::<tonic::transport::Error>) {
                Ok(output) => (output, false),
                Err(Ok(error)) => return Err(error.into()),
                Err(Err(error)) => (format!("{error:?}"), true)
            };
            println!("\nOutput:\n{}", &tool_use.output.0);
        }
        exchange.response.push(response.clone());
        response = stream_response(send_request(&config, exchanges, &exchange).await?).await?;
    }

    exchange.response.push(response);
    Ok(exchange)
}

async fn trigger_cancel(cancel: Arc<tokio::sync::Notify>) {
    loop {
        if let Err(error) = tokio::signal::ctrl_c().await {
            eprintln!("Failed to listen for Ctrl+C: {error}");
            break;
        }
        cancel.notify_waiters();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = tokio::fs::read_to_string("config.json").await.context("Couldn't load config")?;
    let config = serde_json::from_str::<Config>(&config).context("Couldn't parse config")?;

    let cancel = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(trigger_cancel(Arc::clone(&cancel)));

    let mut exchanges = vec![];
    loop {
        let Some(prompt) = common::input("> ").await.context("Failed to read prompt")? else {
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
