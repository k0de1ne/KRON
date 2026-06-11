use minijinja::{Environment, UndefinedBehavior};
use serde_json::{json, Map, Value};
use std::path::Path;

mod builtin;
mod cmd;

pub fn expand(template: &str, hooks_dir: &Path) -> String {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Lenient);

    if env.add_template("__main__", template).is_err() {
        return template.to_string();
    }

    let ctx = build_context(hooks_dir);

    env.get_template("__main__")
        .and_then(|t| t.render(&ctx))
        .unwrap_or_else(|_| template.to_string())
}

fn build_context(hooks_dir: &Path) -> Value {
    let mut map = Map::new();

    // Built-in variables
    for var in builtin::all() {
        map.insert(var.name().to_string(), json!(var.resolve()));
    }

    // env object — {{ env.HOME }}, {{ env.PATH }}, etc.
    let env_obj: Map<String, Value> = std::env::vars().map(|(k, v)| (k, json!(v))).collect();
    map.insert("env".into(), Value::Object(env_obj));

    // External scripts — same name overrides a built-in (user wins)
    if let Ok(entries) = std::fs::read_dir(hooks_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() { continue; }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(|s| s.replace('-', "_"))
                .unwrap_or_default();
            if name.is_empty() { continue; }
            let output = cmd::run_capture(&path).unwrap_or_default();
            map.insert(name, json!(output));
        }
    }

    Value::Object(map)
}
