mod cwd;
mod git_status;
mod time;

pub trait BuiltinVar: Send + Sync {
    fn name(&self) -> &'static str;
    fn resolve(&self) -> String;
}

pub fn all() -> Vec<Box<dyn BuiltinVar>> {
    vec![
        Box::new(time::TimeVar),
        Box::new(cwd::CwdVar),
        Box::new(git_status::GitStatusVar),
    ]
}
