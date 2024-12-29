use anyhow::{Context, Result};
use serde_json::{Map, Value};
use crate::common::{Client, SSHSession, Tool, ToolError::{self, ExecutionError}};

pub const bash: Tool = Tool {
    name: "bash",
    description: include_str!("./resources/bash-description.txt"),
    input_schema: include_str!("./resources/bash-schema.json")
};

#[async_trait::async_trait]
impl russh::client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _server_public_key: &russh::keys::key::PublicKey)
    -> Result<bool, Self::Error> {
        // TODO: implement this?
        Ok(true)
    }
}

pub async fn run_command(command: &str, ssh_session: &mut SSHSession)
-> Result<(String, String, Option<i32>)> {
    let mut channel = ssh_session.channel_open_session().await.context("Failed to open channel")?;
    channel.exec(true, command).await.context("Failed to execute command")?;

    let mut stdout = "".to_string();
    let mut stderr = "".to_string();
    let mut code = None;
    while let Some(msg) = channel.wait().await {
        match msg {
            russh::ChannelMsg::Data { data } => stdout.push_str(
                &String::from_utf8(data.to_vec()).context("Failed to parse command output.")?),
            russh::ChannelMsg::ExtendedData { data, .. } => stderr.push_str(
                &String::from_utf8(data.to_vec()).context("Failed to parse command stderr.")?),
            // cannot return immediately, there may still be data to receive
            russh::ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
            _ => continue
        }
    }

    Ok((stdout, stderr, code))
}

pub async fn run_bash_tool(input: &Map<String, Value>, ssh_session: &mut SSHSession)
-> Result<String, ToolError> {
    let Some(command) = input.get("command").map(Value::as_str).flatten() else {
        crate::bail_tool_use!("The \"command\" argument is required and must be a string.");
    };

    let (output, error, status) = run_command(command, ssh_session).await.map_err(ExecutionError)?;
    let mut content = "".to_string();
    if !output.is_empty() {
        content.push_str("\n\nstdout:\n");
        content.push_str(&output);
    }

    if !error.is_empty() {
        content.push_str("\n\nstderr:\n");
        content.push_str(&error);
    }

    match status {
        None => crate::bail_tool_use!("The command did not exit cleanly.{}", content),
        Some(0) => Ok(content.trim_start().into()),
        Some(status) => crate::bail_tool_use!("Command exited with status {status}:{}", content)
    }
}