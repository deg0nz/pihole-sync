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
        "Watching Pi-hole config file for changes: {}",
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
    while let Some(event) = rx.recv().await {
        match event {
            Ok(event) => {
                let affects_target = event
                    .paths
                    .iter()
                    .any(|path| path == &target_path || path == &canonical_target);
                if !affects_target {
                    continue;
                }

                info!(
                    "Detected change in {:?}. Debouncing for {:?}...",
                    target_path, FILE_WATCH_DEBOUNCE
                );
                sleep(FILE_WATCH_DEBOUNCE).await;

                // Drain burst events
                while let Ok(evt) = rx.try_recv() {
                    match evt {
                        Ok(more_event) => {
                            let hit = more_event
                                .paths
                                .iter()
                                .any(|p| p == &target_path || p == &canonical_target);
                            if hit {
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

pub async fn watch_config_api_main<F, Fut>(
    main: crate::pihole::client::PiHoleClient,
    poll_interval: Duration,
    last_main_config_hash: Option<u64>,
    on_change: F,
) -> Result<()>
where
    F: FnMut(serde_json::Value) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    watch_config_api_with_fetch(
        poll_interval,
        last_main_config_hash,
        move || {
            let main_clone = main.clone();
            async move { main_clone.get_config().await }
        },
        on_change,
    )
    .await
}

pub(crate) async fn watch_config_api_with_fetch<F, Fut, G, GFut>(
    poll_interval: Duration,
    last_main_config_hash: Option<u64>,
    fetch_config: G,
    on_change: F,
) -> Result<()>
where
    F: FnMut(serde_json::Value) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
    G: FnMut() -> GFut + Send + 'static,
    GFut: Future<Output = Result<serde_json::Value>> + Send,
{
    let mut last_main_config_hash = last_main_config_hash;
    let mut fetch_config = fetch_config;
    let mut on_change = on_change;

    loop {
        sleep(poll_interval).await;
        match fetch_config().await {
            Ok(main_config) => match crate::sync::util::hash_config(&main_config) {
                Ok(current_hash) => {
                    if last_main_config_hash.map_or(true, |prev| prev != current_hash) {
                        info!(
                            "Detected config change on main instance (hash {} -> {}). Syncing...",
                            last_main_config_hash
                                .map(|h| h.to_string())
                                .unwrap_or_else(|| "none".into()),
                            current_hash
                        );
                        on_change(main_config.clone()).await?;
                        last_main_config_hash = Some(current_hash);
                    } else {
                        debug!(
                            "No config change detected on main instance (hash {}).",
                            current_hash
                        );
                    }
                }
                Err(e) => warn!("Failed to hash main config: {}", e),
            },
            Err(e) => warn!("Failed to fetch config from main instance: {:?}", e),
        }
    }
}
