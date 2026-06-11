use std::path::Path;

pub fn run_capture(path: &Path) -> anyhow::Result<String> {
    let output = build_cmd(path).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim_end().to_string())
}

fn build_cmd(path: &Path) -> std::process::Command {
    #[cfg(windows)]
    {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        match ext.as_str() {
            "bat" | "cmd" => {
                let mut c = std::process::Command::new("cmd");
                c.args(["/c", path.to_str().unwrap_or_default()]);
                c
            }
            "ps1" => {
                let mut c = std::process::Command::new("powershell");
                c.args(["-NoProfile", "-ExecutionPolicy", "Bypass",
                        "-File", path.to_str().unwrap_or_default()]);
                c
            }
            _ => std::process::Command::new(path),
        }
    }
    #[cfg(not(windows))]
    std::process::Command::new(path)
}
