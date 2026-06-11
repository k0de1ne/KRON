mod timestamp;

pub trait BuiltinEnricher: Send + Sync {
    fn enrich(&self, content: &str, elapsed_ms: Option<u64>) -> String;
}

pub fn all() -> Vec<Box<dyn BuiltinEnricher>> {
    vec![Box::new(timestamp::TimestampEnricher)]
}
