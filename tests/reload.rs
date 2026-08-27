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
fn modify_updates_filter_behavior() {
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
            .modify(|filter| *filter = tracing_subscriber::filter::LevelFilter::DEBUG)
            .unwrap();

        tracing::debug!("debug message after modify");
    });

    let output = buf.as_string();
    assert!(output.contains("info message"), "output was: {output}");
    assert!(!output.contains("debug message\n"), "output was: {output}");
    assert!(
        output.contains("debug message after modify"),
        "output was: {output}"
    );
}

#[test]
fn clone_current_and_with_current() {
    let (layer, handle) =
        ArcSwapLayer::<_, TestSubscriber>::new(tracing_subscriber::filter::LevelFilter::INFO);
    let cloned = handle.clone();

    assert_eq!(
        handle.clone_current(),
        Some(tracing_subscriber::filter::LevelFilter::INFO)
    );
    assert_eq!(
        handle
            .with_current(|current| *current)
            .expect("layer is still alive"),
        tracing_subscriber::filter::LevelFilter::INFO
    );

    handle
        .reload(tracing_subscriber::filter::LevelFilter::DEBUG)
        .unwrap();
    assert_eq!(
        cloned.clone_current(),
        Some(tracing_subscriber::filter::LevelFilter::DEBUG)
    );

    drop(layer);
    assert_eq!(handle.clone_current(), None);

    let err = handle
        .with_current(|current| *current)
        .expect_err("dropped layer should fail");
    assert!(err.is_dropped(), "{err:?}");
    assert!(!err.is_poisoned(), "{err:?}");
    assert_eq!(err.to_string(), "subscriber is gone");
}

#[test]
fn reload_after_layer_drop_is_subscriber_gone() {
    let (layer, handle) =
        ArcSwapLayer::<_, TestSubscriber>::new(tracing_subscriber::filter::LevelFilter::INFO);
    drop(layer);

    let err = handle
        .reload(tracing_subscriber::filter::LevelFilter::DEBUG)
        .expect_err("reload after drop should fail");
    assert!(err.is_dropped(), "{err:?}");
    assert!(!err.is_poisoned(), "{err:?}");
    assert_eq!(err.to_string(), "subscriber is gone");
}
