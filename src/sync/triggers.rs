use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::{sync::mpsc, time::sleep};
use tracing::{debug, info, warn};

use crate::sync::util::FILE_WATCH_DEBOUNCE;

pub async fn run_interval_mode<F, Fut>(
    sync_interval: Duration,
    mut on_tick: F,
    max_iterations: Option<usize>,
) -> Result<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    let mut iterations = 0usize;
    loop {
        info!(
            "Sync complete. Sleeping for {} minutes...",
            sync_interval.as_secs() / 60
        );
        sleep(sync_interval).await;
        on_tick().await?;
        if let Some(max) = max_iterations {
            iterations += 1;
            if iterations >= max {
                break;
            }
        }
    }
    Ok(())
}

pub async fn watch_config_file<F, Fut>(watch_path: &Path, on_change: F) -> Result<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    let parent_dir = watch_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let (tx, rx) = mpsc::unbounded_channel();
    let canonical_target = watch_path
        .canonicalize()
        .unwrap_or_else(|_| watch_path.to_path_buf());
    let target_path = watch_path.to_path_buf();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    )?;

    watcher.watch(&parent_dir, RecursiveMode::NonRecursive)?;
    info!(
        "Watching Pi-hole config file and gravity.db for changes: {}",
        watch_path.display()
    );

    process_file_watch_events(rx, target_path, canonical_target, on_change).await
}

pub async fn process_file_watch_events<F, Fut>(
    mut rx: mpsc::UnboundedReceiver<notify::Result<notify::Event>>,
    target_path: PathBuf,
    canonical_target: PathBuf,
    mut on_change: F,
) -> Result<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    // Groups, lists and domains live in gravity.db next to pihole.toml.
    let targets = [
        target_path.with_file_name("gravity.db"),
        canonical_target.with_file_name("gravity.db"),
        target_path,
        canonical_target,
    ];
    let affects_target = |paths: &[PathBuf]| paths.iter().any(|p| targets.contains(p));

    while let Some(event) = rx.recv().await {
        match event {
            Ok(event) => {
                if !affects_target(&event.paths) {
                    continue;
                }

                info!(
                    "Detected change in {:?}. Debouncing for {:?}...",
                    event.paths, FILE_WATCH_DEBOUNCE
                );
                sleep(FILE_WATCH_DEBOUNCE).await;

                // Drain burst events
                while let Ok(evt) = rx.try_recv() {
                    match evt {
                        Ok(more_event) => {
                            if affects_target(&more_event.paths) {
                                debug!("Coalescing additional file change event");
                            }
                        }
                        Err(e) => warn!("File watch error during debounce: {:?}", e),
                    }
                }

                on_change().await?;
            }
            Err(e) => warn!("File watch error: {:?}", e),
        }
    }

    Ok(())
}

/// Polls `fetch_hash` and calls `on_change` only when the hash differs from the last synced one.
pub async fn watch_config_api<F, Fut, G, GFut>(
    poll_interval: Duration,
    mut last_synced_hash: Option<u64>,
    mut fetch_hash: G,
    mut on_change: F,
) -> Result<()>
where
    F: FnMut() -> Fut + Send + 'static,
    // Ok(true) = synced; Ok(false) = skipped, retry on the next poll.
    Fut: Future<Output = Result<bool>> + Send,
    G: FnMut() -> GFut + Send + 'static,
    GFut: Future<Output = Result<u64>> + Send,
{
    loop {
        sleep(poll_interval).await;
        match fetch_hash().await {
            Ok(hash) if last_synced_hash == Some(hash) => {
                debug!("No change detected on main instance (hash {}).", hash)
            }
            Ok(hash) => {
                info!(
                    "Detected change on main instance (hash {:?} -> {}). Syncing...",
                    last_synced_hash, hash
                );
                if on_change().await? {
                    last_synced_hash = Some(hash);
                }
            }
            Err(e) => warn!("Failed to fetch change signal from main instance: {:?}", e),
        }
    }
}
