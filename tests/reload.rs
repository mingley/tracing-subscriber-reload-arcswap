use std::io;
use std::sync::{Arc, Mutex};
use tracing_subscriber::prelude::*;

use tracing_subscriber_reload_arcswap::ArcSwapLayer;

type TestSubscriber = tracing_subscriber::Registry;

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn as_string(&self) -> String {
        let bytes = self.0.lock().expect("lock shared buffer").clone();
        String::from_utf8(bytes).expect("shared buffer is valid utf-8")
    }
}

struct SharedBufGuard(Arc<Mutex<Vec<u8>>>);

impl io::Write for SharedBufGuard {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("lock shared buffer")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
    type Writer = SharedBufGuard;

    fn make_writer(&'a self) -> Self::Writer {
        SharedBufGuard(Arc::clone(&self.0))
    }
}

#[test]
fn mixed_targets_filter_does_not_suppress_default_level() {
    let targets: tracing_subscriber::filter::Targets = "info,my_crate=error".parse().unwrap();
    let (reloadable, _handle) = ArcSwapLayer::<_, TestSubscriber>::new(targets);
    let buf = SharedBuf::default();

    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(buf.clone())
            .with_filter(reloadable),
    );

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "other_crate", "other allowed");
        tracing::info!(target: "my_crate", "my_crate should be filtered");
    });

    let output = buf.as_string();
    assert!(output.contains("other allowed"), "output was: {output}");
    assert!(
        !output.contains("my_crate should be filtered"),
        "output was: {output}"
    );
}

#[test]
fn reload_updates_filter_behavior() {
    let (reloadable, handle) =
        ArcSwapLayer::<_, TestSubscriber>::new(tracing_subscriber::filter::LevelFilter::INFO);
    let buf = SharedBuf::default();

    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .without_time()
            .with_writer(buf.clone())
            .with_filter(reloadable),
    );

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!("info message");
        tracing::debug!("debug message");

        handle
            .reload(tracing_subscriber::filter::LevelFilter::DEBUG)
            .unwrap();

        tracing::debug!("debug message after reload");
    });

    let output = buf.as_string();
    assert!(output.contains("info message"), "output was: {output}");
    assert!(!output.contains("debug message\n"), "output was: {output}");
    assert!(
        output.contains("debug message after reload"),
        "output was: {output}"
    );
}

#[test]
fn handle_inspection_and_modify() {
    let (_reloadable, handle) =
        ArcSwapLayer::<_, TestSubscriber>::new(tracing_subscriber::filter::LevelFilter::INFO);

    assert_eq!(
        handle.clone_current(),
        Some(tracing_subscriber::filter::LevelFilter::INFO)
    );
    assert_eq!(
        handle.with_current(|f| *f).unwrap(),
        tracing_subscriber::filter::LevelFilter::INFO
    );

    handle
        .modify(|f| *f = tracing_subscriber::filter::LevelFilter::TRACE)
        .unwrap();

    assert_eq!(
        handle.clone_current(),
        Some(tracing_subscriber::filter::LevelFilter::TRACE)
    );
}

#[test]
fn handle_returns_error_after_layer_dropped() {
    let (reloadable, handle) =
        ArcSwapLayer::<_, TestSubscriber>::new(tracing_subscriber::filter::LevelFilter::INFO);

    assert!(handle.clone_current().is_some());
    drop(reloadable);

    assert!(handle.clone_current().is_none());
    assert!(handle.with_current(|f| *f).is_err());
    assert!(
        handle
            .reload(tracing_subscriber::filter::LevelFilter::DEBUG)
            .is_err()
    );
}

#[test]
fn layer_default_constructs_default_inner() {
    let layer = ArcSwapLayer::<String, TestSubscriber>::default();
    let handle = layer.handle();
    assert_eq!(handle.clone_current(), Some(String::new()));
    drop(layer);
}
