use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

use super::BuiltinTool;

pub struct ShellTool;

#[async_trait::async_trait]
impl BuiltinTool for ShellTool {
    fn name(&self) -> &'static str { "shell" }

    fn manifest(&self) -> ToolManifest {
        ToolManifest {
            name: "shell".into(),
            description: "Run a shell command and return stdout. Use for listing files, reading text, system info.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "required": ["command"],
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command to execute"
                    }
                }
            }),
            timeout_secs: 15,
        }
    }

    async fn call(&self, args: &Value) -> Result<String> {
        let cmd = args.get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("missing 'command' field"))?;

        let out = if cfg!(windows) {
            tokio::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", cmd])
                .output().await?
        } else {
            tokio::process::Command::new("sh")
                .args(["-c", cmd])
                .output().await?
        };

        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();

        if !out.status.success() && stdout.is_empty() {
            Ok(format!("error (exit {}): {}", out.status, stderr.trim()))
        } else if stdout.is_empty() {
            Ok(stderr)
        } else {
            Ok(stdout)
        }
    }
}
