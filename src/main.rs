#![allow(non_upper_case_globals)]

mod anthropic;
mod client;
mod common;

use std::{mem::replace, sync::Arc};
use anyhow::{Context, Result};
use anthropic::{send_request, stream_response};
use common::{Cli, Exchange, ToolUse};

#[derive(Default)]
struct RunExchangeTask<'a> {
    exchanges: &'a [Exchange],
    current: Exchange,
    message: String,
    tool_uses: Vec<ToolUse>
}

impl<'a> RunExchangeTask<'a> {
    fn new(prompt: String, exchanges: &'a [Exchange]) -> Self {
        let current = Exchange { prompt, response: vec![] };
        RunExchangeTask { exchanges, current, ..Default::default() }
    }

    async fn run(&mut self) -> Result<()> {
        let Self { exchanges, current, message, tool_uses } = self;
        let response = send_request(exchanges, current).await?;
        stream_response(response, message, tool_uses).await?;

        while !tool_uses.is_empty() {
            for tool_use in tool_uses.iter_mut() {
                tool_use.output = Some(client::call_tool(&tool_use.name, &tool_use.input).await?);
            }

            current.response.push((replace(message, "".into()), replace(tool_uses, vec![])));
            let response = send_request(exchanges, current).await?;
            stream_response(response, message, tool_uses).await?;
        }

        Ok(())
    }

    fn collect(self) -> Exchange {
        let Self { mut current, message, tool_uses, .. } = self;
        current.response.push((message, tool_uses));
        return current;
    }
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
        let prompt_or_eof = tokio::select! {
            _ = cancel.notified() => continue,
            prompt = common::input("> ") => prompt.context("Failed to read prompt")?
        };

        let Some(prompt) = prompt_or_eof else {
            // exit on EOF
            println!();
            std::process::exit(0);
        };

        let mut task = RunExchangeTask::new(prompt, &exchanges);
        tokio::select! {
            _ = cancel.notified() => (),
            result = task.run() => result.context("Failed to run new exchange")?
        }
        exchanges.push(task.collect());
    }
}
