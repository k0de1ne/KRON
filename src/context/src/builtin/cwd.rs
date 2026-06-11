use super::BuiltinVar;

pub struct CwdVar;

impl BuiltinVar for CwdVar {
    fn name(&self) -> &'static str { "cwd" }
    fn resolve(&self) -> String {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}
