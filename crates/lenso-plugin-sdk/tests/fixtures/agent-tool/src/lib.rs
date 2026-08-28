use lenso_plugin_sdk::AgentTool;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Arguments {
    text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolError {
    EmptyText,
}

#[derive(Default)]
struct Uppercase;

impl AgentTool for Uppercase {
    type Arguments = Arguments;
    type Error = ToolError;

    const NAME: &'static str = "uppercase";
    const DESCRIPTION: &'static str = "Convert text to uppercase.";

    fn execute(&self, arguments: Arguments) -> Result<String, ToolError> {
        if arguments.text.is_empty() {
            Err(ToolError::EmptyText)
        } else {
            Ok(arguments.text.to_uppercase())
        }
    }
}

lenso_plugin_sdk::export_agent_tool!(Uppercase);
