use anyhow::{bail, Context, Result};
use serde_json::Value;
use tonic::{transport::Channel, Request};
use bash_agent::{
    tool_runner_client::ToolRunnerClient as Client, BashRequest, CreateRequest, InsertRequest,
    StringReplaceRequest, UndoEditRequest, ViewRange, ViewRequest
};

mod bash_agent {
    tonic::include_proto!("bash_agent");
    impl Snippet {
        pub fn to_string_numbered(self) -> String {
            self.lines.into_iter().enumerate()
                .map(|(i, line)| format!("{}: {line}", self.start as usize + i))
                .collect::<Vec<_>>().join("\n")
        }
    }
}

async fn client() -> Result<Client<Channel>> {
    Client::connect("http://[::1]:50051").await.context("Failed to connect to gRPC server")
}

async fn call_bash_tool(input: &Value) -> Result<String> {
    let Some(input) = input.as_object() else {
        bail!("The argument(s) must be fields in a JSON object");
    };
    let Some(command) = input.get("command").map(Value::as_str).flatten() else {
        bail!("The \"command\" argument is required and must be a string");
    };

    let request = Request::new(BashRequest { input: command.into() });
    let response = client().await?.run_bash_tool(request).await?.into_inner();

    let mut content = "".to_string();
    if !response.stdout.is_empty() {
        content.push_str("stdout:\n");
        content.push_str(&response.stdout);
    }

    if !response.stderr.is_empty() {
        content.push_str("\n\nstderr:\n");
        content.push_str(&response.stderr);
    }

    Ok(content.trim_start().into())
}

#[derive(serde::Deserialize)]
struct TextEditorInput {
    command: String,
    path: String,
    #[serde(default)]
    file_text: Option<String>,
    #[serde(default)]
    insert_line: Option<u32>,
    #[serde(default)]
    new_str: Option<String>,
    #[serde(default)]
    old_str: Option<String>,
    #[serde(default)]
    view_range: Option<Vec<i32>>
}

async fn call_view(path: &str, view_range: Option<Vec<i32>>) -> Result<String> {
    let view_range = match view_range.as_ref().map(Vec::as_slice) {
        Some([start, -1]) if start > &0 => Some(ViewRange { start: *start as u32, end: None }),
        Some([start, end]) if start > &0 || end > &0 =>
            Some(ViewRange { start: *start as u32, end: Some(*end as u32) }),
        Some(_) => bail!("view_range must have two positive entries"),
        None => None
    };
    let request = Request::new(ViewRequest { path: path.into(), view_range });
    let snippet = client().await?.view(request).await?.into_inner();

    Ok(format!("Here's {path} with each line numbered:\n{}", snippet.to_string_numbered()))
}

async fn call_create(path: &str, file_text: Option<String>) -> Result<String> {
    let file_text = file_text.context("file_text is required with the create command")?;
    let request = Request::new(CreateRequest { path: path.into(), file_text });
    client().await?.create(request).await?;
    Ok(format!("Successfully created {path}."))
}

async fn call_str_replace(path: &str, old: Option<String>, new: Option<String>) -> Result<String> {
    let old = old.context("old_str is required with the str_replace command")?;
    let request = Request::new(StringReplaceRequest {
        path: path.into(),
        to_replace: old,
        replacement: new
    });
    let snippet = client().await?.string_replace(request).await?.into_inner().to_string_numbered();
    Ok(format!("Review the changes and make sure it's as expected, edit again if not:\n{snippet}"))
}

async fn insert(path: &str, line_number: Option<u32>, line: Option<String>) -> Result<String> {
    let line_number = line_number.context("insert_line is required with the insert command")?;
    let line = line.context("new_str is required with the insert command")?;
    let request = Request::new(InsertRequest { path: path.into(), line_number, line } );
    let snippet = client().await?.insert(request).await?.into_inner().to_string_numbered();
    Ok(format!("Review the change and make sure it's as expected ({}). {}:\n{snippet}",
        "correct indentation, no duplicate lines, etc", "Edit the file if not."))
}

async fn undo_edit(path: &str) -> Result<String> {
    let request = Request::new(UndoEditRequest { path: path.into() } );
    let snippet = client().await?.undo_edit(request).await?.into_inner().to_string_numbered();
    Ok(format!("Last edit to {path} undone successfully. Please review:\n{snippet}"))
}

async fn call_text_editor_tool(input: &Value) -> Result<String> {
    let TextEditorInput { command, path, file_text, insert_line, new_str, old_str, view_range } =
        serde_json::from_value::<TextEditorInput>(input.clone()).context("Failed to parse input")?;

    match command.as_str() {
        "view" => call_view(&path, view_range).await,
        "create" => call_create(&path, file_text).await,
        "str_replace" => call_str_replace(&path, old_str, new_str).await,
        "insert" => insert(&path, insert_line, new_str).await,
        "undo_edit" => undo_edit(&path).await,
        command => bail!("{command} is an invalid text_editor command")
    }
}

pub async fn call_tool(name: &str, input: &Value) -> Result<String> {
    match name {
        "bash" => call_bash_tool(input).await,
        "text_editor" => call_text_editor_tool(input).await,
        tool => bail!("Tool {tool} not available")
    }
}
