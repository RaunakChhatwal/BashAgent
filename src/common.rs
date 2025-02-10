use serde_json::{json, Value};
use tokio::io::{self, AsyncWriteExt, AsyncBufReadExt, BufReader};

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: &'static str,     // TODO: change this to serde_json::Value
}

impl serde::Serialize for Tool {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match serde_json::from_str::<Value>(self.input_schema) {
            Ok(input_schema) => json!({
                "name": self.name,
                "description": self.description,
                "input_schema": input_schema,
            }).serialize(serializer),
            Err(error) => Err(serde::ser::Error::custom(error))
        }
    }
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct ToolUse {
    pub name: String,
    pub id: String,
    pub input: Value,
    #[serde(default)]
    pub output: (String, bool)      // bool denotes whether error
}

#[derive(Clone, Debug)]
pub struct Exchange {
    pub prompt: String,
    pub response: Vec<(String, Vec<ToolUse>)>
}

#[derive(Clone, Debug, clap::Parser, PartialEq)]
#[command(version, about, long_about = None)]
pub struct Cli {
    #[arg(long)]
    pub server: String,
    #[arg(long)]
    pub model: String,
    #[arg(long)]
    pub temperature: Option<f64>,
    #[arg(long)]
    pub max_tokens: Option<u32>
}

pub async fn write<T: AsRef<[u8]>>(text: T) -> io::Result<()> {
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
