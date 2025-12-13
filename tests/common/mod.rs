use std::sync::Once;

use tracing_log::LogTracer;
use tracing_subscriber::EnvFilter;

static INIT: Once = Once::new();

pub fn init_logging() {
    INIT.call_once(|| {
        // Forward `log` records (used by testcontainers) into tracing so they get printed.
        let _ = LogTracer::init();
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .with_test_writer()
            .with_target(false)
            .try_init();
    });
}
