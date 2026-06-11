use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

use super::BuiltinTool;

pub struct EditFileTool;

#[async_trait::async_trait]
impl BuiltinTool for EditFileTool {
    fn name(&self) -> &'static str { "edit_file" }

    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "edit_file".into(),
            description: "Replace an exact string in a file. oldString must match character-for-character including whitespace and indentation. Use read_file first to get the exact text.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["path", "oldString", "newString"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit"
                    },
                    "oldString": {
                        "type": "string",
                        "description": "Exact text to find and replace. Must be unique in the file unless replaceAll is true."
                    },
                    "newString": {
                        "type": "string",
                        "description": "Replacement text"
                    },
                    "replaceAll": {
                        "type": "boolean",
                        "description": "If true, replace all occurrences. Default: false (errors if multiple matches found)"
                    }
                }
            }),
            timeout_secs: 10,
        }
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let path = args.get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'path' field"))?;
        let old_string = args.get("oldString")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'oldString' field"))?;
        let new_string = args.get("newString")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'newString' field"))?;
        let replace_all = args.get("replaceAll")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path, e))?;

        let count = content.matches(old_string).count();

        let new_content = if count == 0 {
            return Ok(format!(
                "Error: oldString not found in {} - re-read the file to get the exact text including whitespace",
                path
            ));
        } else if count > 1 && !replace_all {
            return Ok(format!(
                "Error: {} occurrences found - oldString must be unique, or set replaceAll=true",
                count
            ));
        } else if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        tokio::fs::write(path, new_content).await?;
        Ok("OK".to_string())
    }
}
