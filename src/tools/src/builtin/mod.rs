use anyhow::Result;
use proto::ToolManifest;
use serde_json::Value;

mod edit_file;
mod grep;
mod list_files;
mod read_file;
mod shell;
mod write_file;

#[async_trait::async_trait]
pub trait BuiltinTool: Send + Sync {
    fn name(&self) -> &'static str;
    fn manifest(&self) -> ToolManifest;
    async fn call(&self, args: &Value) -> Result<String>;
}

pub fn all() -> Vec<Box<dyn BuiltinTool>> {
    vec![
        Box::new(shell::ShellTool),
        Box::new(read_file::ReadFileTool),
        Box::new(write_file::WriteFileTool),
        Box::new(edit_file::EditFileTool),
        Box::new(grep::GrepTool),
        Box::new(list_files::ListFilesTool),
    ]
}
