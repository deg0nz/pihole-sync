use crate::sync::triggers::*;
use crate::sync::util::hash_config;
use anyhow::Result;
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use tempfile::tempdir;
use tokio::{
    sync::mpsc,
    time::{timeout, Duration},
};

#[tokio::test]
async fn interval_mode_runs_on_tick() -> Result<()> {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();

    run_interval_mode(
        Duration::from_millis(10),
        move || {
            let ctr = counter_clone.clone();
            async move {
                ctr.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }
        },
        Some(1),
    )
    .await?;

    assert_eq!(counter.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn watch_config_file_triggers_on_change() -> Result<()> {
    let temp = tempdir()?;
    let watch_path = temp.path().join("pihole.toml");
    std::fs::write(&watch_path, "initial")?;

    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let watcher = tokio::spawn(process_file_watch_events(
        event_rx,
        watch_path.clone(),
        watch_path.clone(),
        move || {
            let tx = tx.clone();
            async move {
                let _ = tx.send(());
                Ok(())
            }
        },
    ));

    let event = notify::Event {
        kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )),
        paths: vec![watch_path.clone()],
        attrs: Default::default(),
    };
    event_tx.send(Ok(event)).expect("failed to send event");

    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("watch timed out")
        .expect("channel closed");

    watcher.abort();
    Ok(())
}

#[tokio::test]
async fn watch_config_api_triggers_on_change() -> Result<()> {
    let config1 = json!({ "config": { "dns": { "servers": ["1.1.1.1"] } } });
    let config2 = json!({ "config": { "dns": { "servers": ["9.9.9.9"] } } });
    let config2_for_fetch = config2.clone();
    let config2_for_expect = config2.clone();

    let fetch_counter = Arc::new(AtomicUsize::new(0));
    let fetch_counter_clone = fetch_counter.clone();

    let (tx, mut rx) = mpsc::unbounded_channel();

    let baseline_hash = Some(hash_config(&config1)?);

    let watcher = tokio::spawn(watch_config_api(
        Duration::from_millis(50),
        baseline_hash,
        move || {
            let count = fetch_counter.fetch_add(1, Ordering::SeqCst);
            let cfg = if count == 0 {
                config1.clone()
            } else {
                config2_for_fetch.clone()
            };
            async move { Ok(cfg) }
        },
        move |cfg| {
            let tx = tx.clone();
            let expected = config2_for_expect.clone();
            async move {
                assert_eq!(cfg, expected);
                let _ = tx.send(());
                Ok(())
            }
        },
    ));

    timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("watch timed out")
        .expect("channel closed");

    watcher.abort();
    assert!(fetch_counter_clone.load(Ordering::SeqCst) >= 2);
    Ok(())
}
