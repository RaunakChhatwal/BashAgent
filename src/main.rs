#![allow(non_upper_case_globals)]

mod anthropic;
mod client;
mod common;
mod openai;

use std::{mem::replace, sync::Arc};
use anyhow::{Context, Result};
use common::{Cli, Exchange, ModelParams, Provider, ToolUse};
use reqwest::Response;

struct RunExchangeTask<'a> {
    params: &'a ModelParams,
    exchanges: &'a [Exchange],
    current: Exchange,
    message: String,
    tool_uses: Vec<ToolUse>
}

async fn send_request(
    exchanges: &[Exchange], current: &Exchange, params: &ModelParams
) -> Result<reqwest::Response> {
    match params.provider {
        Provider::Anthropic => anthropic::send_request(exchanges, current, params).await,
        Provider::OpenAI => openai::send_request(exchanges, current, params).await
    }
}
 
async fn stream_response(
    response: Response, provider: &Provider, message: &mut String, tool_uses: &mut Vec<ToolUse>
) -> Result<()> {
    match provider {
        Provider::Anthropic => anthropic::stream_response(response, message, tool_uses).await,
        Provider::OpenAI => openai::stream_response(response, message, tool_uses).await
    }
}

impl<'a> RunExchangeTask<'a> {
    fn new(prompt: String, exchanges: &'a [Exchange], params: &'a ModelParams) -> Self {
        let current = Exchange { prompt, response: vec![] };
        RunExchangeTask { params, exchanges, current, message: "".into(), tool_uses: vec![] }
    }

    async fn run(&mut self) -> Result<()> {
        let Self { params, exchanges, current, message, tool_uses } = self;
        let response = send_request(exchanges, current, params).await?;
        stream_response(response, &params.provider, message, tool_uses).await?;

        while !tool_uses.is_empty() {
            for tool_use in tool_uses.iter_mut() {
                tool_use.output = Some(client::call_tool(&tool_use.name, &tool_use.input).await?);
            }

            current.response.push((replace(message, "".into()), replace(tool_uses, vec![])));
            let response = send_request(exchanges, current, params).await?;
            stream_response(response, &params.provider, message, tool_uses).await?;
        }

        return Ok(());
    }

    fn collect(self) -> Exchange {
        let Self { mut current, message, tool_uses, .. } = self;
        if !message.is_empty() || !tool_uses.is_empty() {
            current.response.push((message, tool_uses));
        }
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
    let Cli { model, temperature, max_tokens, .. } = clap::Parser::parse();
    let provider =
        model.anthropic.is_none().then(|| Provider::OpenAI).unwrap_or(Provider::Anthropic);
    let model = model.anthropic.unwrap_or(model.openai);
    let model_params = ModelParams { provider, model, max_tokens, temperature };

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

        let mut task = RunExchangeTask::new(prompt, &exchanges, &model_params);
        tokio::select! {
            _ = cancel.notified() => (),
            result = task.run() => result?
        }
        exchanges.push(task.collect());
    }
}
