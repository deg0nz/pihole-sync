mod runner;
pub(crate) mod triggers;
pub(crate) mod util;

pub use runner::run_sync;
pub use triggers::{run_interval_mode, watch_config_api_main, watch_config_file};

#[cfg(test)]
mod tests;
