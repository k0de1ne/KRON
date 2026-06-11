use super::BuiltinVar;

pub struct TimeVar;

impl BuiltinVar for TimeVar {
    fn name(&self) -> &'static str { "time" }
    fn resolve(&self) -> String {
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
    }
}
