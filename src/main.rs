mod agent;
mod auth;
mod browser;
mod config;
mod hub;
mod llm;
mod local;
mod node;
mod sandbox;
mod store;
mod tools;
mod turn;
mod types;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use config::Config;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use store::Store;
use turn::TurnReq;
use types::{Message, Stdout};

#[derive(Parser)]
#[command(name = "kron", version, about = "One harness for all your AI subscriptions")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[command(flatten)]
    run: RunArgs,
    /// Config file (default: ~/.kron/config.toml)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// Ignore the hub: run and store everything on this machine
    #[arg(long, global = true)]
    local: bool,
}

#[derive(Args, Default)]
struct RunArgs {
    /// What to do. Without it: interactive session (or stdin when piped).
    prompt: Vec<String>,
    /// Continue this thread (default: start a new one)
    #[arg(short, long)]
    thread: Option<String>,
    /// Agent from config
    #[arg(short, long)]
    agent: Option<String>,
    /// Provider from config
    #[arg(short, long)]
    provider: Option<String>,
    #[arg(short, long)]
    model: Option<String>,
    /// Plain question/answer, no tools
    #[arg(long)]
    chat: bool,
    /// Ask a web chat through the browser (chatgpt, gemini)
    #[arg(long, value_name = "SITE")]
    web: Option<String>,
    /// Run tools / the browser on this node (via the hub)
    #[arg(long, value_name = "NODE")]
    on: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Write an example config to ~/.kron/config.toml
    Init,
    /// Run the hub: threads, agent loops, web UI, node coordination
    Serve,
    /// What the Android app runs: creates a config if missing, serves the UI on
    /// this device and, with [hub] url set, also works as a node for that hub
    App,
    /// Connect this machine to the hub so agents can work on it
    Node {
        /// Working directory for tools (default: current directory)
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Sign in to a subscription provider (codex, claude, gemini, antigravity)
    Login { provider: String },
    /// Configured providers and their login state
    Providers,
    /// Machines connected to the hub
    Nodes,
    /// Recent threads
    Threads,
    /// Print a thread
    Show { thread: String },
    /// Exchange threads with the hub (both directions)
    Sync,
    /// Browser for web chats
    Browser {
        #[command(subcommand)]
        cmd: BrowserCmd,
    },
}

#[derive(Subcommand)]
enum BrowserCmd {
    /// Open a visible window to sign in to a chat site once
    Login { site: String },
}

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

async fn real_main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Arc::new(Config::load(cli.config.as_deref())?);
    let store = Store::new(cfg.path("threads"));
    let hub = if cli.local { None } else { node::Client::from_config(&cfg) };

    match cli.cmd {
        Some(Cmd::Init) => init(&cfg),
        Some(Cmd::Serve) => hub::serve(cfg.clone(), workdir(&cfg, None)?).await,
        Some(Cmd::App) => {
            if !cfg.file.exists() {
                init(&cfg)?;
            }
            let cfg = Arc::new(Config::load(Some(&cfg.file))?);
            let mut dir = workdir(&cfg, None)?;
            if let Some(sb) = sandbox::get() {
                // The agent works in the container's /root; fetch the rootfs now so it is ready.
                dir = sb.home();
                std::fs::create_dir_all(&dir)?;
                tokio::spawn(async move {
                    if let Err(e) = sb.ready().await {
                        eprintln!("sandbox: {e:#}");
                    }
                });
            }
            if let Some(client) = hub {
                let (cfg, name, dir) = (cfg.clone(), cfg.node_name(), dir.clone());
                tokio::spawn(async move {
                    if let Err(e) = node::work(cfg, client, name, dir, false, None).await {
                        eprintln!("node: {e:#}");
                    }
                });
            }
            hub::serve(cfg, dir).await
        }
        Some(Cmd::Node { dir }) => {
            let client = hub.context("set [hub] url (and token) in config first")?;
            let name = cfg.node_name();
            node::work(cfg.clone(), client, name, workdir(&cfg, dir)?, false, None).await
        }
        Some(Cmd::Login { provider }) => auth::login(&cfg, &provider).await,
        Some(Cmd::Providers) => {
            for (name, p) in &cfg.providers {
                let state = match p.kind.as_str() {
                    "codex" | "claude" | "gemini" | "antigravity" => {
                        auth::Auth::new(&cfg, name, &p.kind, p)?.describe()
                    }
                    _ if llm::api_key(p).is_some() => "api key set".into(),
                    _ => "no api key (set api_key or api_key_env)".into(),
                };
                let search = if cfg.search_provider().as_deref() == Some(name) { ", web search" } else { "" };
                println!("{name:14} {:12} {:24} {state}{search}", p.kind, p.model);
            }
            Ok(())
        }
        Some(Cmd::Nodes) => {
            let info = hub.context("no [hub] url configured")?.info().await?;
            for n in info["nodes"].as_array().into_iter().flatten() {
                let browser = if n["browser"] == true { "  browser" } else { "" };
                println!(
                    "{:20} {:8} {}{browser}",
                    n["name"].as_str().unwrap_or(""),
                    n["os"].as_str().unwrap_or(""),
                    n["cwd"].as_str().unwrap_or("")
                );
            }
            Ok(())
        }
        Some(Cmd::Threads) => {
            let list = match &hub {
                Some(c) => c.threads().await?,
                None => store.list(),
            };
            for t in list.iter().take(50) {
                let when = chrono::DateTime::from_timestamp_millis(t.updated)
                    .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("{:20} {when}  {}", t.name, t.title);
            }
            Ok(())
        }
        Some(Cmd::Show { thread }) => {
            let records = match &hub {
                Some(c) => c.thread(&thread).await?,
                None => store.load(&thread)?,
            };
            for r in records {
                print_message(&r.msg);
            }
            Ok(())
        }
        Some(Cmd::Sync) => {
            let c = hub.context("no [hub] url configured")?;
            let (mut up, mut down) = (0, 0);
            for t in store.list() {
                up += c.merge(&t.name, &store.load(&t.name)?).await?;
            }
            for t in c.threads().await? {
                down += store.merge(&t.name, c.thread(&t.name).await?)?;
            }
            println!("sent {up} messages, received {down}");
            Ok(())
        }
        Some(Cmd::Browser { cmd: BrowserCmd::Login { site } }) => browser::login(&cfg, &site).await,
        None => run(cfg, store, hub, cli.run).await,
    }
}

fn workdir(cfg: &Config, explicit: Option<PathBuf>) -> Result<PathBuf> {
    let dir = match explicit.or_else(|| cfg.hub.workdir.as_ref().map(PathBuf::from)) {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    Ok(std::fs::canonicalize(&dir).unwrap_or(dir))
}

fn init(cfg: &Config) -> Result<()> {
    let path = &cfg.file;
    if path.exists() {
        bail!("{} already exists", path.display());
    }
    std::fs::create_dir_all(path.parent().unwrap_or(&cfg.home))?;
    std::fs::write(path, config::EXAMPLE)?;
    println!("Wrote {}", path.display());
    Ok(())
}

fn print_message(m: &Message) {
    match m {
        Message::User { content } => println!("\n> {content}"),
        Message::Assistant { text, tool_calls, .. } => {
            if let Some(t) = text {
                println!("\n{t}");
            }
            for tc in tool_calls {
                println!("· {} {}", tc.name, types::preview(&tc.arguments.to_string(), 200));
            }
        }
        Message::Tool { content, .. } => println!("  {}", types::preview(content, 200).replace('\n', "\n  ")),
    }
}

async fn run(cfg: Arc<Config>, store: Store, hub: Option<node::Client>, args: RunArgs) -> Result<()> {
    let mut req = TurnReq {
        thread: args.thread.clone().unwrap_or_else(store::new_thread_name),
        mode: if args.web.is_some() {
            "web".into()
        } else if args.chat {
            "chat".into()
        } else {
            "agent".into()
        },
        agent: args.agent.clone(),
        provider: args.provider.clone(),
        model: args.model.clone(),
        node: args.on.clone(),
        site: args.web.clone(),
        text: String::new(),
    };
    if req.node.is_some() && hub.is_none() {
        bail!("--on needs a hub: set [hub] url in config");
    }

    // With a hub and no --on, this very process serves as the node, so tools
    // run here, in the current directory, while the thread lives on the hub.
    let mut worker = None;
    if let (Some(client), None) = (&hub, &req.node) {
        let name = format!("{}-cli{}", cfg.node_name(), std::process::id());
        let cwd = std::env::current_dir()?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        worker = Some(tokio::spawn(node::work(cfg.clone(), client.clone(), name.clone(), cwd, true, Some(tx))));
        if !matches!(tokio::time::timeout(std::time::Duration::from_secs(15), rx).await, Ok(Ok(()))) {
            bail!("cannot connect to hub {} (check url/token, or use --local)", cfg.hub.url.as_deref().unwrap_or(""));
        }
        req.node = Some(name);
    }

    let interactive = args.prompt.is_empty() && std::io::stdin().is_terminal();
    let result = if interactive {
        eprintln!("kron · thread {} · empty line or Ctrl+D to quit", req.thread);
        let mut lines = tokio::io::AsyncBufReadExt::lines(tokio::io::BufReader::new(tokio::io::stdin()));
        loop {
            eprint!("\n› ");
            let Some(line) = lines.next_line().await? else { break Ok(()) };
            if line.trim().is_empty() {
                break Ok(());
            }
            req.text = line;
            if let Err(e) = one(&cfg, &store, &hub, &req).await {
                eprintln!("error: {e:#}");
            }
        }
    } else {
        req.text = if args.prompt.is_empty() {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
            s
        } else {
            args.prompt.join(" ")
        };
        if req.text.trim().is_empty() {
            bail!("empty prompt");
        }
        let r = one(&cfg, &store, &hub, &req).await;
        eprintln!("[thread {}]", req.thread);
        r
    };
    if let Some(w) = worker {
        w.abort();
    }
    browser::shutdown().await;
    local::shutdown().await;
    result
}

async fn one(cfg: &Arc<Config>, store: &Store, hub: &Option<node::Client>, req: &TurnReq) -> Result<()> {
    match hub {
        Some(client) => {
            client.run(req, &Stdout).await?;
        }
        None => {
            let exec = Arc::new(agent::Local::new(cfg, std::env::current_dir()?));
            turn::turn(cfg, store, req, exec, &Stdout).await?;
            println!();
        }
    }
    Ok(())
}
