use super::BuiltinEnricher;

pub struct TimestampEnricher;

impl BuiltinEnricher for TimestampEnricher {
    fn enrich(&self, content: &str, elapsed_ms: Option<u64>) -> String {
        let ts = chrono::Local::now().format("%H:%M:%S");
        match elapsed_ms {
            Some(ms) => format!("[{ts} +{ms}ms]\n{content}"),
            None     => format!("[{ts}]\n{content}"),
        }
    }
}
