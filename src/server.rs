#![feature(iter_advance_by)]

use std::{collections::HashMap, os::fd::AsRawFd, path::{Path, PathBuf}, process::Stdio};
use anyhow::Context;
use tonic::{transport::Server, Request, Response, Status};
use tokio::{io::AsyncWriteExt, process::{Child, Command}, sync::Mutex};
use nix::fcntl::{fcntl, FcntlArg::{F_GETFL, F_SETFL}, OFlag};
use bash_agent::{
    tool_runner_server, BashRequest, BashResponse, CreateRequest, InsertRequest,
    Snippet, StringReplaceRequest, UndoEditRequest, ViewRange, ViewRequest
};

mod bash_agent {
    tonic::include_proto!("bash_agent");
    impl Snippet {
        pub fn new(content: &str, range: Option<(usize, usize)>) -> Snippet {
            let lines = content.split("\n").map(str::to_owned);
            let Some((mut start, end)) = range else {
                return Snippet { start: 1, lines: lines.collect() };
            };
    
            let padding = 4;
            start = start.saturating_sub(padding);
            Snippet {
                start: 1 + start as u32,
                lines: lines.take(end + padding).skip(start).collect()
            }
        }
    }    
}

#[derive(Default)]
struct FileHistoryEntry {
    latest: String,
    history: Vec<String>
}

type Result<T> = std::result::Result<T, Status>;
struct ToolRunner {
    file_history: Mutex<HashMap<PathBuf, FileHistoryEntry>>,
    bash: Mutex<Child>
}

impl ToolRunner {
    async fn write(&self, path: PathBuf, content: String) -> std::io::Result<()> {
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

async fn validate_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path).to_owned();
    if !path.is_absolute() {
        return Err(Status::invalid_argument("The path must be absolute"));
    }

    Ok(path)
}

// TODO: debug nix::ioctl_none!(ioc_pipe_wait_read_invoc, 'R', 69420);
nix::ioctl_none_bad!(ioc_pipe_wait_read_invoc, 89900);

fn read_pipe<T: AsRawFd>(pipe: &mut T) -> anyhow::Result<String> {
    let mut output = String::new();
    let mut buffer = [0u8; 1024];

    loop {
        match nix::unistd::read(pipe.as_raw_fd(), &mut buffer) {
            Ok(0) => break,
            Ok(n) => output.push_str(&String::from_utf8_lossy(&buffer[..n])),
            Err(nix::errno::Errno::EWOULDBLOCK) => break,
            Err(error) => return Err(error.into())
        }
    }

    Ok(output)
}

#[tonic::async_trait]
impl tool_runner_server::ToolRunner for ToolRunner {
    async fn run_bash_tool(&self, request: Request<BashRequest>) -> Result<Response<BashResponse>> {
        let mut bash = self.bash.lock().await;

        let stdin = bash.stdin.as_mut().ok_or(Status::internal("Failed to get stdin handle."))?;
        let fd = stdin.as_raw_fd();
        let handle = tokio::task::spawn_blocking(move || unsafe { ioc_pipe_wait_read_invoc(fd) });

        stdin.write_all((request.into_inner().input + "\n").as_bytes()).await?;
        stdin.flush().await?;

        handle.await.expect("Failed to wait for ioctl").expect("Error running ioctl");

        let stdout = bash.stdout.as_mut().ok_or(Status::internal("Failed to get stdout handle."))?;
        let output = read_pipe(stdout)
            .map_err(|error| Status::internal(format!("Error reading from pipe: {error}")))?;

        let stderr = bash.stderr.as_mut().ok_or(Status::internal("Failed to get stderr handle."))?;
        let error = read_pipe(stderr)
            .map_err(|error| Status::internal(format!("Error reading from pipe: {error}")))?;

        Ok(Response::new(BashResponse { stdout: output, stderr: error }))
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

fn set_nonblocking<T: AsRawFd>(pipe: &mut T) -> anyhow::Result<i32> {
    let mut flags = OFlag::from_bits_truncate(fcntl(pipe.as_raw_fd(), F_GETFL)?);
    flags |= OFlag::O_NONBLOCK;
    fcntl(pipe.as_raw_fd(), F_SETFL(flags)).map_err(Into::into)
}

fn spawn_bash() -> anyhow::Result<Child> {
    let mut bash = Command::new("bash")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().context("Error spawning bash")?;

    let stdout = bash.stdout.as_mut().ok_or(Status::internal("Failed to get stdout handle."))?;
    set_nonblocking(stdout)?;

    let stderr = bash.stderr.as_mut().ok_or(Status::internal("Failed to get stderr handle."))?;
    set_nonblocking(stderr)?;

    Ok(bash)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let address = "[::1]:50051".parse()?;
    let service = tool_runner_server::ToolRunnerServer::new(ToolRunner {
        bash: Mutex::new(spawn_bash()?),
        file_history: Default::default()
    });
    Server::builder().add_service(service).serve(address).await.map_err(Into::into)
}
