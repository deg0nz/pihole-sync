use crate::sync::triggers::*;
use anyhow::Result;
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

    // Both pihole.toml and the sibling gravity.db (groups/lists/domains) must trigger.
    for path in [watch_path.clone(), temp.path().join("gravity.db")] {
        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Content,
            )),
            paths: vec![path],
            attrs: Default::default(),
        };
        event_tx.send(Ok(event)).expect("failed to send event");

        timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("watch timed out")
            .expect("channel closed");
    }

    watcher.abort();
    Ok(())
}

/// Unrelated files in /etc/pihole (pihole-FTL.db is written constantly) must not trigger,
/// and a burst of writes must collapse into one sync.
#[tokio::test]
async fn watch_ignores_unrelated_files_and_coalesces_bursts() -> Result<()> {
    let temp = tempdir()?;
    let watch_path = temp.path().join("pihole.toml");
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
    let send = |path: std::path::PathBuf| {
        event_tx
            .send(Ok(notify::Event::new(notify::EventKind::Any).add_path(path)))
            .unwrap()
    };

    send(temp.path().join("pihole-FTL.db"));
    assert!(timeout(Duration::from_millis(1500), rx.recv())
        .await
        .is_err());

    for _ in 0..3 {
        send(watch_path.clone());
    }
    send(temp.path().join("gravity.db"));
    timeout(Duration::from_secs(5), rx.recv()).await?;
    assert!(timeout(Duration::from_millis(1500), rx.recv())
        .await
        .is_err());

    watcher.abort();
    Ok(())
}
