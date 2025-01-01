#![feature(iter_advance_by)]

use std::{collections::HashMap, os::fd::AsRawFd, path::{Path, PathBuf}, process::Stdio};
use anyhow::{bail, Context, Result};
use tonic::{transport::Server, Request, Response, Status};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, fs, process::{Child, Command}, sync::Mutex};
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

// TODO: debug nix::ioctl_none!(ioc_pipe_wait_read_invoc, 'R', 69420);
nix::ioctl_none_bad!(ioc_pipe_wait_read_invoc, 89900);

fn read_pipe<T: AsRawFd>(pipe: &mut T) -> Result<String> {
    let mut output = String::new();
    let mut buffer = [0u8; 1024];

    loop {
        match nix::unistd::read(pipe.as_raw_fd(), &mut buffer) {
            Ok(0) => break,
            Ok(n) => output.push_str(&String::from_utf8_lossy(&buffer[..n])),
            Err(nix::errno::Errno::EWOULDBLOCK) => break,
            Err(error) => return Err(error).context("Error reading from pipe")
        }
    }

    Ok(output)
}

async fn run_bash_tool(bash: &mut Child, request: BashRequest) -> Result<BashResponse> {
    let stdin = bash.stdin.as_mut().context("Failed to get stdin handle.")?;
    let fd = stdin.as_raw_fd();
    let mut handle = tokio::task::spawn_blocking(move || unsafe { ioc_pipe_wait_read_invoc(fd) });

    stdin.write_all((request.input + "\n").as_bytes()).await?;
    stdin.flush().await?;

    let stdout = bash.stdout.as_mut().context("Failed to get stdout handle.")?;
    let mut stdout_bufreader = tokio::io::BufReader::new(stdout);
    let mut stdout_buffer = [0u8; 1024];

    let stderr = bash.stderr.as_mut().ok_or(Status::internal("Failed to get stderr handle."))?;
    let mut stderr_bufreader = tokio::io::BufReader::new(stderr);
    let mut stderr_buffer = [0u8; 1024];

    let mut output = String::new();
    loop {
        tokio::select! {
            result = &mut handle => {
                result.context("Failed to wait for ioctl")?.context("Error calling ioctl")?;
                break;
            },
            n = stdout_bufreader.read(&mut stdout_buffer) => match n {
                Ok(0) => break,
                Ok(n) => output.push_str(&String::from_utf8_lossy(&stdout_buffer[..n])),
                Err(error) => return Err(error.into())
            },
            n = stderr_bufreader.read(&mut stderr_buffer) => match n {
                Ok(0) => break,
                Ok(n) => output.push_str(&String::from_utf8_lossy(&stderr_buffer[..n])),
                Err(error) => return Err(error.into())
            }
        }
    }

    output.push_str(&read_pipe(stdout_bufreader.into_inner())?);
    output.push_str(&read_pipe(stderr_bufreader.into_inner())?);

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

async fn validate_path(path: &str) -> Result<PathBuf, Status> {
    let path = Path::new(path).to_owned();
    if !path.is_absolute() {
        return Err(Status::invalid_argument("The path must be absolute"));
    }

    Ok(path)
}

async fn view(ViewRequest { path, view_range }: ViewRequest) -> Result<Snippet> {
    let path = validate_path(&path).await?;
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
    let path = validate_path(&path).await?;
    if path.exists() {
        bail!("File already exists");
    }

    write(path, file_text).await
}

async fn string_replace(request: StringReplaceRequest) -> Result<Snippet> {
    let path = validate_path(&request.path).await?;
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
    let path = validate_path(&request.path).await?;
    let mut content = fs::read_to_string(&path).await?;

    // iterate to current line
    let line_number = request.line_number.saturating_sub(1) as usize;
    let mut newlines = content.chars().enumerate().filter(|(_, char)| char == &'\n');
    if newlines.advance_by(line_number).is_err() {
        bail!("There are only {} lines in {path:?}", content.matches('\n').count());
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
    let path = validate_path(&request.path).await?;
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
    bash: Mutex<Child>
}

fn to_status(error: anyhow::Error) -> Status {
    Status::unknown(format!("{error:?}"))
}

type TonicResult<T> = Result<Response<T>, Status>;
#[tonic::async_trait]
impl tool_runner_server::ToolRunner for ToolRunner {
    async fn run_bash_tool(&self, request: Request<BashRequest>) -> TonicResult<BashResponse> {
        let mut bash = self.bash.lock().await;
        run_bash_tool(&mut bash, request.into_inner()).await.map(Response::new)
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

fn set_nonblocking<T: AsRawFd>(pipe: &mut T) -> Result<i32> {
    let mut flags = OFlag::from_bits_truncate(fcntl(pipe.as_raw_fd(), F_GETFL)?);
    flags |= OFlag::O_NONBLOCK;
    fcntl(pipe.as_raw_fd(), F_SETFL(flags)).map_err(Into::into)
}

fn spawn_bash() -> Result<Child> {
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
async fn main() -> Result<()> {
    let address = "[::1]:50051".parse()?;
    let tool_runner = ToolRunner { bash: Mutex::new(spawn_bash()?) };
    let service = tool_runner_server::ToolRunnerServer::new(tool_runner);
    Server::builder().add_service(service).serve(address).await.map_err(Into::into)
}
