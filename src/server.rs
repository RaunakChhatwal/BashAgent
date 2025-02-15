#![feature(iter_advance_by)]

use std::{collections::HashMap, os::fd::{AsFd, AsRawFd}, path::{Path, PathBuf}, process::Stdio};
use anyhow::{bail, Context, Result};
use nix::{sys::termios, unistd};
use tonic::{transport::Server, Request, Response, Status};
use tokio::{io::{self, AsyncReadExt, AsyncWriteExt}, fs, process::Command, sync::{mpsc, Mutex}};
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
    input_sender: &Sender,
    output_recv: &mut Recv,
    slave: &std::fs::File,
    BashRequest { input }: BashRequest
) -> Result<BashResponse> {
    let mut output = vec![];
    for line in input.split("\n").map(str::as_bytes) {
        let slave_fd = slave.as_raw_fd();
        let mut handle = tokio::task::spawn_blocking(move || unsafe { t_ioc_read_invoc(slave_fd) });
        input_sender.send(line.to_vec()).context("Error sending input to the echoing task.")?;

        loop {
            tokio::select! {
                result = &mut handle => {
                    result.context("Failed to wait for ioctl")?.context("Error calling ioctl")?;
                    break;
                },
                Some(data) = output_recv.recv() => output.extend(data)
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // read any leftover data
    let data_future = std::pin::pin!(output_recv.recv());
    if let futures::task::Poll::Ready(Some(data)) = futures::poll!(data_future) {
        output.extend(data); 
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

type Sender = mpsc::UnboundedSender<Vec<u8>>;
type Recv = mpsc::UnboundedReceiver<Vec<u8>>;

struct ToolRunner {
    input_sender: Sender,
    output_recv: Mutex<Recv>,
    slave: std::fs::File
}

fn to_status(error: anyhow::Error) -> Status {
    Status::unknown(format!("{error:?}"))       // format as debug to include the anyhow context
}

type TonicResult<T> = Result<Response<T>, Status>;

#[tonic::async_trait]
impl tool_runner_server::ToolRunner for ToolRunner {
    async fn run_bash_tool(&self, request: Request<BashRequest>) -> TonicResult<BashResponse> {
        let output_recv = &mut self.output_recv.lock().await;
        run_bash_tool(&self.input_sender, output_recv, &self.slave, request.into_inner()).await
            // format as debug to include the anyhow context
            .map(Response::new).map_err(|error| Status::internal(format!("{error:?}")))
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

async fn write_all(mut writer: impl AsyncWriteExt + Unpin, text: impl AsRef<[u8]>) -> Result<()> {
    writer.write_all(text.as_ref()).await?;
    writer.flush().await.map_err(Into::into)
}

async fn echo_pty(master: std::os::fd::OwnedFd, mut input_recv: Recv, output_sender: Sender)
-> Result<()> {
    let mut stdin = io::stdin();
    let mut write_end: fs::File = std::fs::File::from(master).into();
    let mut input_buffer = [0u8; 1024];

    let mut read_end = write_end.try_clone().await.context("Failed to clone master pty")?;
    let mut output_buffer = [0u8; 1024];

    loop {
        tokio::select! {
            n = stdin.read(&mut input_buffer) => match n {
                Ok(0) => bail!("Stdin dropped"),
                Ok(n) => {
                    let input = input_buffer[..n].to_vec();
                    write_all(&mut write_end, &input).await.context("Failed to write pty input")?;
                    output_sender.send(input).context("Failed to echo input to output_sender")?;
                },
                Err(error) => return Err(error).context("Failed to read from stdin")
            },
            Some(mut input) = input_recv.recv() => {
                input.push(b'\n');
                write_all(io::stdout(), &input).await.context("Failed to print pty input")?;
                write_all(&mut write_end, &input).await.context("Failed to write pty input")?;
                output_sender.send(input).context("Failed to echo input to output_sender")?;
            },
            n = read_end.read(&mut output_buffer) => match n {
                Ok(0) => bail!("Lost master pty handle"),
                Ok(n) => {
                    let data = output_buffer[..n].to_vec();
                    write_all(io::stdout(), &data).await.context("Failed to print pty output")?;
                    output_sender.send(data).context("Failed to send pty output")?;
                },
                Err(error) => return Err(error).context("Failed to read from master pty")
            }
        }
    }
}

nix::ioctl_none_bad!(t_ioc_s_c_tty, nix::libc::TIOCSCTTY);

// inspired by portable_pty::SlavePty::spawn_command
fn spawn_pty() -> Result<(std::os::fd::OwnedFd, std::fs::File, tokio::process::Child)> {
    let pty_pair = nix::pty::openpty(None, None).context("Failed to open pty pair")?;
    let slave = std::fs::File::from(pty_pair.slave);

    // set raw terminal mode
    let mut termios = termios::tcgetattr(pty_pair.master.as_fd())?;
    termios::cfmakeraw(&mut termios);
    termios::tcsetattr(pty_pair.master.as_fd(), termios::SetArg::TCSANOW, &termios)?;

    let mut bash = Command::new("bash");
    for setter in [Command::stdin, Command::stdout, Command::stderr] {
        setter(&mut bash, Stdio::from(slave.try_clone().context("Error copying slave pty")?));
    }

    unsafe {
        bash.pre_exec(move || {
            unistd::setsid()?;      // create new session
            t_ioc_s_c_tty(0)?;      // set controlling terminal
            Ok(())
        });
    }

    let child = bash.spawn().context("Error spawning bash subprocess")?;
    Ok((pty_pair.master, slave, child))
}

#[tokio::main]
async fn main() -> Result<()> {
    let (master, slave, mut child) = spawn_pty()?;
    let (input_sender, input_recv) = mpsc::unbounded_channel::<Vec<u8>>();
    let (output_sender, output_recv) = mpsc::unbounded_channel::<Vec<u8>>(); 
    let handle = tokio::spawn(echo_pty(master, input_recv, output_sender));

    let address = "0.0.0.0:50051".parse()?;
    let tool_runner = ToolRunner { input_sender, output_recv: Mutex::new(output_recv), slave };
    let service = tool_runner_server::ToolRunnerServer::new(tool_runner);

    tokio::select! {
        result = Server::builder().add_service(service).serve(address) => match result {
            Ok(()) => bail!("Service listener exitted prematurely"),
            error => error.context("Failed to listen on {address}")
        },
        result = handle => match result {
            Ok(Ok(())) => bail!("Echoing task exitted prematurely"),
            error => error.map_err(Into::into).unwrap_or_else(Err).context("Failed to echo pty")
        },
        status = child.wait() => {
            let status = status.context("Failed to wait for bash subprocess")?.code().unwrap_or(1);
            std::process::exit(status);
        }
    }
}
