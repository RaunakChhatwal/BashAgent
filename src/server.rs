#![feature(iter_advance_by)]

use std::{collections::HashMap, os::fd::AsRawFd, path::{Path, PathBuf}, process::Stdio};
use anyhow::{bail, Context, Result};
use tonic::{transport::Server, Request, Response, Status};
use tokio::{io::{self, AsyncReadExt, AsyncWriteExt}, fs, process::Command, sync::Mutex};
use bash_agent::*;

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

// wait for a task to block on read
nix::ioctl_none!(t_ioc_read_invoc, 'T', 0x69);

async fn run_bash_tool(
    master: &mut fs::File,
    slave_fd: i32,
    BashRequest { input }: BashRequest
) -> Result<BashResponse> {
    let mut handle = tokio::task::spawn_blocking(move || unsafe { t_ioc_read_invoc(slave_fd) });

    master.write_all((input + "\n").as_bytes()).await?;
    master.flush().await?;

    let mut output = vec![];
    let mut bufreader = tokio::io::BufReader::new(master);
    let mut buffer = [0u8; 1024];
    loop {
        tokio::select! {
            result = &mut handle => {
                result.context("Failed to wait for ioctl")?.context("Error calling ioctl")?;
                break;
            },
            n = bufreader.read(&mut buffer) => match n {
                Ok(0) => break,
                Ok(n) => output.extend(&buffer[..n]),
                Err(error) => return Err(error).context("Failed to read from master pty")
            }
        }
    }

    // read any leftover data
    let mut future = bufreader.read(&mut buffer);
    while let futures::task::Poll::Ready(n) = futures::poll!(std::pin::pin!(future)) {
        output.extend(&buffer[..n.context("Failed to read from master pty")?]); 
        future = bufreader.read(&mut buffer);
    }

    // remove escape sequences
    let escape_sequence_pattern =
        regex::Regex::new(r"\x1b\[[0-9;]*[\x40-\x7E]").expect("Invalid regex");
    let output = escape_sequence_pattern.replace_all(&String::from_utf8_lossy(&output), "").into();

    Ok(BashResponse { output })
}

#[derive(Default)]
struct FileHistoryEntry {
    latest: String,
    history: Vec<String>
}

lazy_static::lazy_static!{
    static ref file_history: Mutex<HashMap<PathBuf, FileHistoryEntry>> = Default::default();
}

async fn write(path: PathBuf, content: String) -> Result<()> {
    let mut history = file_history.lock().await;
    fs::write(&path, &content).await?;
    if let Some(FileHistoryEntry { latest, history }) = history.get_mut(&path) {
        history.push(std::mem::replace(latest, content));
    } else {
        history.insert(path, FileHistoryEntry { latest: content, history: vec![] });
    }

    Ok(())
}

fn validate_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path).to_owned();
    if !path.is_absolute() {
        bail!("The path must be absolute");
    }

    Ok(path)
}

async fn view(ViewRequest { path, view_range }: ViewRequest) -> Result<Snippet> {
    let path = validate_path(&path)?;
    let content = fs::read_to_string(&path).await?;

    let Some(ViewRange { start, end }) = view_range else {
        return Ok(Snippet::new(&content, None));
    };
    let start = (start as usize).saturating_sub(1);
    let end = match end {
        Some(end) => end as usize,
        None => content.matches('\n').count() + 1
    };
    Ok(Snippet::new(&content, Some((start, end))))
}

async fn create(CreateRequest { path, file_text }: CreateRequest) -> Result<()> {
    let path = validate_path(&path)?;
    if path.exists() {
        bail!("{path:?} already exists");
    }

    write(path, file_text).await
}

async fn string_replace(request: StringReplaceRequest) -> Result<Snippet> {
    let path = validate_path(&request.path)?;
    let mut content = fs::read_to_string(&path).await?;

    let to_replace = &request.to_replace;
    let Some(index) = content.find(to_replace) else {
        bail!("No match found to `to_replace` for replacement");
    };
    if content.matches(to_replace).skip(1).next().is_some() {
        bail!("Multiple matches found to `to_replace`, a unique match is necessary");
    }

    let replacement = request.replacement.as_ref().map(String::as_str).unwrap_or("");
    content.replace_range(index..index + to_replace.len(), replacement);

    let start = content[..index].matches('\n').count();
    let end = start + replacement.matches('\n').count();
    let snippet = Snippet::new(&content, Some((start, end + 1)));

    write(path, content).await?;
    Ok(snippet)
}

async fn insert(request: InsertRequest) -> Result<Snippet> {
    let path = validate_path(&request.path)?;
    let mut content = fs::read_to_string(&path).await?;

    // iterate to current line
    let line_number = request.line_number.saturating_sub(1) as usize;
    let mut newlines = content.chars().enumerate().filter(|(_, char)| char == &'\n');
    if newlines.advance_by(line_number).is_err() {
        bail!("There are only {} lines in {path:?}", content.matches('\n').count() + 1);
    };

    // insert after current line
    let index = newlines.next().map(|(index, _)| index).unwrap_or(content.len());
    let line = "\n".to_string() + &request.line;
    content.insert_str(index, &line);

    let end = line_number + line.matches('\n').count();
    let snippet = Snippet::new(&content, Some((line_number, end + 1)));

    write(path, content).await?;
    Ok(snippet)
}

async fn undo_edit(request: UndoEditRequest) -> Result<Snippet> {
    let path = validate_path(&request.path)?;
    let mut history = file_history.lock().await;
    let Some(FileHistoryEntry { latest, history }) = history.get_mut(&path) else {
        bail!("No history found for {path:?}");
    };
    let Some(new_latest) = history.pop() else {
        bail!("Already at oldest change");
    };
    if let Err(error) = fs::write(&path, &new_latest).await {
        history.push(new_latest);
        return Err(error.into());
    }

    *latest = new_latest;
    Ok(Snippet::new(latest, None))
}

struct ToolRunner {
    master: Mutex<fs::File>,
    slave_fd: i32
}

fn to_status(error: anyhow::Error) -> Status {
    Status::unknown(format!("{error:?}"))       // format as debug to include the anyhow context
}

type TonicResult<T> = Result<Response<T>, Status>;

#[tonic::async_trait]
impl tool_runner_server::ToolRunner for ToolRunner {
    async fn run_bash_tool(&self, request: Request<BashRequest>) -> TonicResult<BashResponse> {
        let master = &mut self.master.lock().await;
        run_bash_tool(master, self.slave_fd, request.into_inner()).await.map(Response::new)
            // format as debug to include the anyhow context
            .map_err(|error| Status::internal(format!("{error:?}")))
    }

    async fn view(&self, request: Request<ViewRequest>) -> TonicResult<Snippet> {
        view(request.into_inner()).await.map(Response::new).map_err(to_status)
    }

    async fn create(&self, request: Request<CreateRequest>) -> TonicResult<()> {
        create(request.into_inner()).await.map(Response::new).map_err(to_status)
    }

    async fn string_replace(&self, request: Request<StringReplaceRequest>) -> TonicResult<Snippet> {
        string_replace(request.into_inner()).await.map(Response::new).map_err(to_status)
    }

    async fn insert(&self, request: Request<InsertRequest>) -> TonicResult<Snippet> {
        insert(request.into_inner()).await.map(Response::new).map_err(to_status)
    }

    async fn undo_edit(&self, request: Request<UndoEditRequest>) -> TonicResult<Snippet> {
        undo_edit(request.into_inner()).await.map(Response::new).map_err(to_status)
    }
}

async fn echo_pty(mut master: fs::File) -> Result<()> {
    let mut buffer = [0u8; 1024];
    loop {
        let n = master.read(&mut buffer).await.context("Error reading master pty")?;
        if n == 0 {     // handle EOF
            std::process::exit(0);
        }

        io::stdout().write_all(&buffer[..n]).await.context("Error echoing pty output")?;
        io::stdout().flush().await.context("Error flushing stdout")?;
    }
}

fn spawn_pty() -> Result<(fs::File, std::fs::File)> {
    let pty_pair = nix::pty::openpty(None, None).context("Failed to open pty pair")?;
    let slave = std::fs::File::from(pty_pair.slave);

    let mut bash = Command::new("bash");
    for setter in [Command::stdin, Command::stdout, Command::stderr] {
        setter(&mut bash, Stdio::from(slave.try_clone().context("Error copying slave pty")?));
    }
    bash.spawn().context("Error spawning bash subprocess")?;

    Ok((std::fs::File::from(pty_pair.master).into(), slave))
}

#[tokio::main]
async fn main() -> Result<()> {
    let (master, slave) = spawn_pty()?;
    tokio::spawn(echo_pty(master.try_clone().await.context("Failed to copy master pty")?));

    let address = "0.0.0.0:50051".parse()?;
    let tool_runner = ToolRunner { master: Mutex::new(master), slave_fd: slave.as_raw_fd() };
    let service = tool_runner_server::ToolRunnerServer::new(tool_runner);
    Server::builder().add_service(service).serve(address).await.map_err(Into::into)
}
