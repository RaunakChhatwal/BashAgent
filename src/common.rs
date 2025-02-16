use serde_json::Value;
use tokio::io::{self, AsyncWriteExt, AsyncBufReadExt, BufReader};

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: &'static str,     // TODO: change this to serde_json::Value
}

pub const bash_tool: Tool = Tool {
    name: "bash",
    description: include_str!("./resources/bash-description.txt"),
    input_schema: include_str!("./resources/bash-schema.json")
};

pub const text_editor_tool: Tool = Tool {
    name: "text_editor",
    description: include_str!("./resources/text_editor-description.txt"),
    input_schema: include_str!("./resources/text_editor-schema.json")
};

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ToolUse {
    pub name: String,
    pub id: String,
    pub input: Value,
    #[serde(default)]
    pub output: Option<(String, bool)>      // bool denotes whether error
}

#[derive(Clone, Debug, Default)]
pub struct Exchange {
    pub prompt: String,
    pub response: Vec<(String, Vec<ToolUse>)>
}

pub enum Provider {
    Anthropic,
    OpenAI
}

pub struct ModelParams {
    pub provider: Provider,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f64
}

#[derive(clap::Args, Clone, Debug, PartialEq)]
#[group(multiple = false)]
pub struct Model {
    #[clap(long)]
    pub anthropic: Option<String>,
    #[clap(long, default_value = "gpt-4o")] // "o3-mini")]
    pub openai: String
}

#[derive(clap::Parser, Clone, Debug, PartialEq)]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[arg(long)]
    pub server: String,
    #[command(flatten)]
    pub model: Model,
    #[arg(long, default_value_t = 8192)]
    pub max_tokens: u32,
    #[arg(long, default_value_t = 1.0)]
    pub temperature: f64
}

pub async fn write(text: impl AsRef<[u8]>) -> io::Result<()> {
    let mut stdout = io::stdout();
    stdout.write_all(text.as_ref()).await?;
    stdout.flush().await
}

pub async fn input(prompt: &str) -> io::Result<Option<String>> {
    write(prompt).await?;

    let mut stdin = BufReader::new(io::stdin());
    let mut input = String::new();
    
    match stdin.read_line(&mut input).await {
        Ok(0) => Ok(None),      // user presses ctrl d
        Ok(_) => Ok(Some(input.trim().to_string())),
        Err(error) => Err(error),
    }
}
