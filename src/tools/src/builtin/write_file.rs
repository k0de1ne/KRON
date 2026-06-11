use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

use super::BuiltinTool;

pub struct WriteFileTool;

#[async_trait::async_trait]
impl BuiltinTool for WriteFileTool {
    fn name(&self) -> &'static str { "write_file" }

    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "write_file".into(),
            description: "Create or completely overwrite a file with new content. Use edit_file for partial edits.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["path", "content"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write (relative to working directory)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full content to write to the file"
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
        let content = args.get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'content' field"))?;

        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        tokio::fs::write(path, content).await?;
        Ok(format!("written {} bytes to {}", content.len(), path))
    }
}
