/// File-based trigger: watches a directory for `.task` files.
///
/// Drop `my_task.task` into the directory — the agent runs and writes `my_task.result` next to it.
use anyhow::Result;
use proto::{AgentEvent, Config, EventSink, LlmBackend};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::time::{sleep, Duration};

pub async fn watch(cfg: Config, backend: Arc<dyn LlmBackend>, dir: PathBuf) -> Result<()> {
    tokio::fs::create_dir_all(&dir).await?;
    eprintln!("[plexus/trigger] watching {:?} for .task files", dir);

    loop {
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("task") {
                continue;
            }

            let processing = path.with_extension("processing");
            if tokio::fs::rename(&path, &processing).await.is_err() {
                continue; // another worker grabbed it, or file disappeared
            }

            let task = match tokio::fs::read_to_string(&processing).await {
                Ok(s) => s.trim().to_string(),
                Err(e) => {
                    eprintln!("[plexus/trigger] failed to read {:?}: {}", processing, e);
                    continue;
                }
            };

            if task.is_empty() {
                tokio::fs::remove_file(&processing).await.ok();
                continue;
            }

            let result_path = processing.with_extension("result");
            let cfg = cfg.clone();
            let backend = Arc::clone(&backend);
            let processing = processing.clone();

            tokio::spawn(async move {
                eprintln!("[plexus/trigger] run: {}", task.chars().take(80).collect::<String>());
                let sink = Arc::new(CollectSink::default());
                let outcome = agent::run(
                    task,
                    vec![],
                    cfg,
                    backend,
                    Arc::clone(&sink) as Arc<dyn EventSink>,
                ).await.map(|_| ());

                let text = sink.text();
                let output = match outcome {
                    Ok(()) => text,
                    Err(e) => format!("error: {e}"),
                };

                tokio::fs::write(&result_path, &output).await.ok();
                tokio::fs::remove_file(&processing).await.ok();
                eprintln!("[plexus/trigger] done → {:?}", result_path);
            });
        }

        sleep(Duration::from_secs(2)).await;
    }
}

// ── Collecting sink ───────────────────────────────────────────────────────────

#[derive(Default)]
struct CollectSink {
    buf: Mutex<String>,
}

impl CollectSink {
    fn text(&self) -> String {
        self.buf.lock().map(|b| b.clone()).unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl EventSink for CollectSink {
    async fn emit(&self, event: AgentEvent) {
        if let AgentEvent::Text { content } = event {
            if let Ok(mut b) = self.buf.lock() {
                b.push_str(&content);
            }
        }
    }
}
