use std::path::Path;

pub fn build_cmd(path: &Path) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        match ext.as_str() {
            "bat" | "cmd" => {
                let mut c = tokio::process::Command::new("cmd");
                c.args(["/c", path.to_str().unwrap_or_default()]);
                c
            }
            "ps1" => {
                let mut c = tokio::process::Command::new("powershell");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass",
                        "-File", path.to_str().unwrap_or_default()]);
                c
            }
            _ => tokio::process::Command::new(path),
        }
    }
    #[cfg(not(windows))]
    tokio::process::Command::new(path)
}
