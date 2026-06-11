use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

use super::BuiltinTool;

pub struct ReadFileTool;

#[async_trait::async_trait]
impl BuiltinTool for ReadFileTool {
    fn name(&self) -> &'static str { "read_file" }

    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "read_file".into(),
            description: "Read a file with line numbers. Use offset and limit to page through large files without overwhelming the context.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (relative to working directory)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "First line to read, 1-based (default: 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to return (default: 150)"
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

        let content = tokio::fs::read_to_string(path).await
            .map_err(|e| anyhow::anyhow!("cannot read {}: {}", path, e))?;

        let lines: Vec<&str> = content.lines().collect();
        let total = lines.len();

        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let limit  = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(150) as usize;

        let start = offset.saturating_sub(1).min(total);
        let end   = (start + limit).min(total);

        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            out.push_str(&format!("{:5}\t{}\n", start + i + 1, line));
        }
        if end < total {
            out.push_str(&format!(
                "(lines {}-{} of {} shown; use offset={} to continue)",
                start + 1, end, total, end + 1
            ));
        }
        Ok(out)
    }
}
