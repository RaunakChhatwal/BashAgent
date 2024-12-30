#![feature(iter_advance_by)]

use std::{collections::HashMap, path};
use tonic::{transport::Server, Request, Response, Status};

mod bash_agent {
    tonic::include_proto!("bash_agent");
}
use bash_agent::{
    tool_runner_server, BashRequest, BashResponse, CreateRequest, InsertRequest,
    Snippet, StringReplaceRequest, UndoEditRequest, ViewRange, ViewRequest
};

#[derive(Default)]
struct FileHistoryEntry {
    latest: String,
    history: Vec<String>
}

type Result<T> = std::result::Result<T, Status>;
#[derive(Default)]
struct ToolRunner {
    file_history: tokio::sync::Mutex<HashMap<path::PathBuf, FileHistoryEntry>>
}

impl ToolRunner {
    async fn write(&self, path: path::PathBuf, content: String) -> std::io::Result<()> {
        let mut file_history = self.file_history.lock().await;
        tokio::fs::write(&path, &content).await?;
        if let Some(FileHistoryEntry { latest, history }) = file_history.get_mut(&path) {
            history.push(std::mem::replace(latest, content));
        } else {
            file_history.insert(path, FileHistoryEntry { latest: content, history: vec![] });
        }

        Ok(())
    }
}

impl Snippet {
    fn new(content: &str, range: Option<(usize, usize)>) -> Snippet {
        let lines = content.split("\n").map(str::to_owned);
        let Some((mut start, end)) = range else {
            return Snippet { start_line_number: 1, lines: lines.collect() };
        };

        let padding = 4;
        start = start.saturating_sub(padding);
        Snippet {
            start_line_number: 1 + start as u32,
            lines: lines.take(end + padding).skip(start).collect()
        }
    }
}

async fn validate_path(path: &str) -> Result<path::PathBuf> {
    let path = path::Path::new(path).to_owned();
    if !path.is_absolute() {
        return Err(Status::invalid_argument("The path must be absolute"));
    }

    Ok(path)
}

#[tonic::async_trait]
impl tool_runner_server::ToolRunner for ToolRunner {
    async fn run_bash_tool(&self, request: Request<BashRequest>) -> Result<Response<BashResponse>> {
        let command = &request.get_ref().command;
        let output = tokio::process::Command::new("bash").args(["-c", command]).output().await
            .map_err(|error| Status::internal(format!("Failed to spawn `{command}`: {error}")))?;
        let stdout = String::from_utf8(output.stdout)
            .map_err(|_| Status::data_loss("Unable to read stdout as UTF-8 string"))?;
        let stderr = String::from_utf8(output.stderr)
            .map_err(|_| Status::data_loss("Unable to read stderr as UTF-8 string"))?;

        Ok(Response::new(BashResponse { stdout, stderr, status_code: output.status.code() }))
    }

    async fn view(&self, request: Request<ViewRequest>) -> Result<Response<Snippet>> {
        let path = validate_path(&request.get_ref().path).await?;
        let content = tokio::fs::read_to_string(&path).await?;

        let Some(ViewRange { start, end }) = request.into_inner().view_range else {
            return Ok(Response::new(Snippet::new(&content, None)));
        };
        let start = (start as usize).saturating_sub(1);
        let end = match end {
            Some(end) => end as usize,
            None => content.matches('\n').count() + 1
        };
        Ok(Response::new(Snippet::new(&content, Some((start, end)))))
    }

    async fn create(&self, request: Request<CreateRequest>) -> Result<Response<()>> {
        let CreateRequest { path, file_text } = request.into_inner();
        let path = validate_path(&path).await?;
        if path.exists() {
            return Err(Status::already_exists("File already exists"));
        }

        self.write(path, file_text).await.map(Response::new).map_err(Into::into)
    }

    async fn string_replace(&self, request: Request<StringReplaceRequest>)
    -> Result<Response<Snippet>> {
        let path = validate_path(&request.get_ref().path).await?;
        let mut content = tokio::fs::read_to_string(&path).await?;

        let to_replace = &request.get_ref().to_replace;
        let Some(index) = content.find(to_replace) else {
            return Err(Status::not_found("No match found to `to_replace` for replacement"));
        };
        if content.matches(to_replace).skip(1).next().is_some() {
            let error = "Multiple matches found to `to_replace`, a unique match is necessary";
            return Err(Status::invalid_argument(error));
        }

        let replacement = request.get_ref().replacement.as_ref().map(String::as_str).unwrap_or("");
        content.replace_range(index..index + to_replace.len(), replacement);

        let start = content[..index].matches('\n').count();
        let end = start + replacement.matches('\n').count();
        let snippet = Snippet::new(&content, Some((start, end + 1)));

        self.write(path, content).await?;
        Ok(Response::new(snippet))
    }

    async fn insert(&self, request: Request<InsertRequest>) -> Result<Response<Snippet>> {
        let path = validate_path(&request.get_ref().path).await?;
        let mut content = tokio::fs::read_to_string(&path).await?;

        // iterate to current line
        let line_number = request.get_ref().line_number.saturating_sub(1) as usize;
        let mut newlines = content.chars().enumerate().filter(|(_, char)| char == &'\n');
        if newlines.advance_by(line_number).is_err() {
            return Err(Status::invalid_argument(
                format!("There are only {} lines in {path:?}", content.matches('\n').count())));
        };

        // insert after current line
        let index = newlines.next().map(|(index, _)| index).unwrap_or(content.len());
        let line = "\n".to_string() + &request.get_ref().line;
        content.insert_str(index, &line);

        let end = line_number + line.matches('\n').count();
        let snippet = Snippet::new(&content, Some((line_number, end + 1)));

        self.write(path, content).await?;
        Ok(Response::new(snippet))
    }

    async fn undo_edit(&self, request: Request<UndoEditRequest>) -> Result<Response<Snippet>> {
        let path = validate_path(&request.get_ref().path).await?;
        let mut file_history = self.file_history.lock().await;
        let Some(FileHistoryEntry { latest, history }) = file_history.get_mut(&path) else {
            return Err(Status::not_found(format!("No history found for {path:?}")));
        };
        let Some(new_latest) = history.pop() else {
            return Err(Status::resource_exhausted("Already at oldest change"));
        };
        if let Err(error) = tokio::fs::write(&path, &new_latest).await {
            history.push(new_latest);
            return Err(error.into());
        }

        *latest = new_latest;
        Ok(Response::new(Snippet::new(latest, None)))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let address = "[::1]:50051".parse()?;
    let service = tool_runner_server::ToolRunnerServer::new(ToolRunner::default());
    Server::builder().add_service(service).serve(address).await.map_err(Into::into)
}
