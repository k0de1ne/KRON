use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;

#[cfg(feature = "http")]
mod http;

#[derive(Parser)]
#[command(name = "plexus", about = "Minimal autonomous agent engine")]
struct Cli {
    /// Task to run. If omitted, read from stdin.
    task: Option<String>,

    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Thread key for persistent history (required).
    /// --thread <KEY>  → resume or create thread by name
    #[cfg(feature = "loop")]
    #[arg(long, value_name = "KEY")]
    thread: Option<String>,

    #[cfg(feature = "trigger")]
    #[arg(long, value_name = "DIR")]
    watch: Option<PathBuf>,

    #[cfg(feature = "http")]
    #[arg(long)]
    serve: bool,

    #[cfg(feature = "http")]
    #[arg(long, default_value = "8080")]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = proto::Config::load(cli.config.as_deref())?;
    let backend = llm::create(&cfg.llm, cfg.api_key())?;

    #[cfg(feature = "http")]
    if cli.serve {
        use std::sync::Arc;
        let state = http::AppState { cfg: Arc::new(cfg), backend };
        return http::serve(state, cli.port).await;
    }

    #[cfg(feature = "trigger")]
    if let Some(dir) = cli.watch {
        return trigger::file::watch(cfg, backend, dir).await;
    }

    let task = read_task(cli.task)?;

    #[cfg(feature = "loop")]
    {
        use proto::StdoutSink;
        use std::sync::Arc;

        let key = cli.thread.ok_or_else(|| anyhow::anyhow!("--thread <KEY> is required"))?;
        let threads_dir = cfg.threads_dir();
        let mut sess = session::Session::open(&key, None, &threads_dir)?;
        eprintln!("[thread] {}", sess.key);
        return sess.run(task, backend, Arc::new(StdoutSink)).await.map(|_| ());
    }

    #[cfg(not(feature = "loop"))]
    qa(task, cfg, backend).await
}

#[cfg(not(feature = "loop"))]
async fn qa(
    task: String,
    _cfg: proto::Config,
    backend: std::sync::Arc<dyn proto::LlmBackend>,
) -> Result<()> {
    use proto::{AgentEvent, EventSink, Message, StdoutSink};

    #[cfg(feature = "context")]
    let system = {
        let ctx = _cfg.context_file();
        if ctx.exists() {
            let tmpl = std::fs::read_to_string(&ctx).unwrap_or_default();
            context::expand(&tmpl, &_cfg.hooks_dir())
        } else {
            String::new()
        }
    };
    #[cfg(not(feature = "context"))]
    let system = String::new();

    #[cfg(feature = "hooks")]
    hooks::run_pre(&_cfg, &task).await;

    let sink = StdoutSink;
    let resp = backend.complete_streaming(&system, &[Message::User(task.clone())], &[], &sink).await?;

    if let Some(ref text) = resp.text {
        sink.emit(AgentEvent::Text { content: text.clone() }).await;
    }
    println!();

    #[cfg(feature = "hooks")]
    hooks::run_post(&_cfg, &task, resp.text.as_deref().unwrap_or_default(), 0).await;

    Ok(())
}

fn read_task(task: Option<String>) -> Result<String> {
    match task {
        Some(t) => Ok(t),
        None => {
            let mut buf = String::new();
            std::io::stdin().read_line(&mut buf)?;
            Ok(buf.trim().to_string())
        }
    }
}
