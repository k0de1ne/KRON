use anyhow::Result;
#[cfg(feature = "tools")]
use futures::future::join_all;
use proto::{AgentEvent, Config, EventSink, LlmBackend, Message, Tool};
use std::sync::Arc;

pub async fn run(
    task: String,
    history: Vec<Message>,
    cfg: Config,
    backend: Arc<dyn LlmBackend>,
    sink: Arc<dyn EventSink>,
) -> Result<Vec<Message>> {
    let cfg = Arc::new(cfg);

    #[cfg(feature = "hooks")]
    hooks::run_pre(&cfg, &task).await;

    let history_len = history.len();
    let mut messages = history;
    messages.push(Message::User(task.clone()));

    #[cfg_attr(not(feature = "hooks"), allow(unused_variables))]
    let mut total_tool_calls: usize = 0;

    loop {
        let system = build_system(&cfg);

        #[cfg(feature = "tools")]
        let manifests = tools::discover(&cfg.tool_paths());
        #[cfg(not(feature = "tools"))]
        let manifests: Vec<proto::ToolManifest> = vec![];

        let llm_tools: Vec<Tool> = manifests.iter().map(|m| Tool {
            name: m.name.clone(),
            description: m.description.clone(),
            parameters: m.parameters.clone(),
        }).collect();

        let resp = backend.complete_streaming(&system, &messages, &llm_tools, sink.as_ref()).await?;

        let assistant_text = resp.text.clone();
        messages.push(Message::Assistant {
            text: resp.text.clone(),
            tool_calls: resp.tool_calls.clone(),
        });

        if resp.tool_calls.is_empty() {
            #[cfg(feature = "hooks")]
            if let Some(text) = assistant_text {
                let enriched = hooks::enrich(&text, None, &cfg).await;
                if let Some(Message::Assistant { text: ref mut t, .. }) = messages.last_mut() {
                    *t = Some(enriched);
                }
            }
            break;
        }

        total_tool_calls += resp.tool_calls.len();

        #[cfg(feature = "tools")]
        let results = {
            let tool_paths = cfg.tool_paths();
            join_all(resp.tool_calls.iter().map(|tc| {
                let tool_paths = tool_paths.clone();
                let name = tc.name.clone();
                let args = tc.arguments.clone();
                let id = tc.id.clone();
                let sink = Arc::clone(&sink);
                let timeout = manifests.iter()
                    .find(|m| m.name == name)
                    .map(|m| m.timeout_secs)
                    .unwrap_or(30);
                async move {
                    let start = std::time::Instant::now();
                    sink.emit(AgentEvent::ToolStart { name: name.clone(), args: args.to_string() }).await;
                    let output = tools::run(&tool_paths, &name, &args, timeout)
                        .await
                        .unwrap_or_else(|e| format!("error: {e}"));
                    let elapsed_ms = start.elapsed().as_millis() as u64;
                    sink.emit(AgentEvent::ToolEnd { name: name.clone(), output: output.clone() }).await;
                    (id, output, elapsed_ms)
                }
            })).await
        };

        #[cfg(not(feature = "tools"))]
        let results: Vec<(String, String, u64)> = resp.tool_calls.iter()
            .map(|tc| (tc.id.clone(), "tools not compiled in".to_string(), 0u64))
            .collect();

        for (id, output, elapsed_ms) in results {
            #[cfg(feature = "hooks")]
            let content = hooks::enrich(&output, Some(elapsed_ms), &cfg).await;
            #[cfg(not(feature = "hooks"))]
            let content = output;
            messages.push(Message::ToolResult { tool_call_id: id, content });
        }

        if resp.done { break; }
    }

    sink.emit(AgentEvent::Done).await;
    println!();

    #[cfg(feature = "hooks")]
    {
        let final_text = messages.iter().rev().find_map(|m| match m {
            Message::Assistant { text: Some(t), .. } => Some(t.clone()),
            _ => None,
        }).unwrap_or_default();
        hooks::run_post(&cfg, &task, &final_text, total_tool_calls).await;
    }

    // Return only the new messages added during this run
    Ok(messages[history_len..].to_vec())
}

fn build_system(_cfg: &Config) -> String {
    #[cfg(feature = "context")]
    {
        let ctx_file = _cfg.context_file();
        if ctx_file.exists() {
            let template = std::fs::read_to_string(&ctx_file).unwrap_or_default();
            return context::expand(&template, &_cfg.hooks_dir());
        }
    }
    String::new()
}
