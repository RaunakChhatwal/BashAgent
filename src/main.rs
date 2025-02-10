#![allow(non_upper_case_globals)]

mod anthropic;
mod client;
mod common;

use std::sync::Arc;
use anyhow::{Context, Result};
use anthropic::{send_request, stream_response};
use common::{Cli, Exchange};

// send prompt to llm, run tools, send back tool results, repeat until no more tool use
async fn run_exchange(prompt: String, exchanges: &[Exchange]) -> Result<Exchange> {
    let mut exchange = Exchange { prompt, response: vec![] };
    let response = send_request(exchanges, &exchange).await?;
    let (mut message, mut tool_uses) = stream_response(response).await?;

    while !tool_uses.is_empty() {
        for tool_use in &mut tool_uses {
            tool_use.output = client::call_tool(&tool_use.name, &tool_use.input).await?;
        }
        exchange.response.push((message, tool_uses));
        (message, tool_uses) = stream_response(send_request(exchanges, &exchange).await?).await?;
    }

    if !message.is_empty() {
        exchange.response.push((message, vec![]));
    }

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
    let _: Cli = clap::Parser::parse();     // ensure argv is parseable into Cli

    let cancel = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(trigger_cancel(Arc::clone(&cancel)));

    let mut exchanges = vec![];
    loop {
        let Some(prompt) = common::input("> ").await.context("Failed to read prompt")? else {
            println!();
            return Ok(());
        };

        tokio::select! {
            _ = cancel.notified() => continue,
            exchange = run_exchange(prompt, &exchanges) => exchanges.push(exchange?)
        }
    }
}
