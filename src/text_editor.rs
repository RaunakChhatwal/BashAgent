use serde_json::{Map, Value};
use crate::common::{SSHSession, Tool, ToolError};

pub const _text_editor: Tool = Tool {
    name: "text_editor",
    description: include_str!("./resources/text_editor-description.txt"),
    input_schema: include_str!("./resources/text_editor-schema.json")
};

pub async fn run_text_editor_tool(input: &Map<String, Value>, _ssh_session: &mut SSHSession)
-> Result<String, ToolError> {
    let Some(command) = input.get("command").map(Value::as_str).flatten() else {
        crate::bail_tool_use!("The \"command\" argument is required and must be a string.");
    };

    let Some(_path) = input.get("path").map(Value::as_str).flatten() else {
        crate::bail_tool_use!("The \"path\" argument is required and must be a string.");
    };

    match command {
        "view" => todo!(),
        "create" => todo!(),
        "str_replace" => todo!(),
        "insert" => todo!(),
        "undo_edit" => todo!(),
        _ => crate::bail_tool_use!("{command} is an invalid text_editor command.")
    }

    // todo!();
}