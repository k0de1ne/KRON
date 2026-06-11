use super::BuiltinVar;

pub struct GitStatusVar;

impl BuiltinVar for GitStatusVar {
    fn name(&self) -> &'static str { "git_status" }
    fn resolve(&self) -> String {
        let out = std::process::Command::new("git")
            .args(["status", "--short"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if s.is_empty() { "(clean)".to_string() } else { s }
            }
            _ => "(not a git repo)".to_string(),
        }
    }
}
