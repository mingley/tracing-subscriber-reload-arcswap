#![cfg_attr(docsrs, feature(doc_cfg))]

//! Lock-free reload support for `tracing-subscriber` layers and filters.
//!
//! This crate provides [`ArcSwapLayer`], a pragmatic alternative to
//! `tracing_subscriber::reload::Layer` that avoids acquiring an `RwLock` on every
//! span/event fast-path.
//!
//! Reloading (swapping in a new `Layer`/`Filter`) is expected to be rare and is
//! serialized internally, while reads are lock-free.
//!
//! ## Trait bounds
//!
//! - When wrapping a type used as a [`Layer`], `ArcSwapLayer` requires `L: Clone`
//!   (because `Layer::on_layer` requires `&mut self`).
//! - When wrapping a per-layer [`Filter`](tracing_subscriber::layer::Filter),
//!   `ArcSwapLayer` does *not* require `L: Clone` for the read path or for
//!   [`ArcSwapHandle::reload`]. However, [`ArcSwapHandle::modify`] requires
//!   `L: Clone` because it applies changes by cloning and swapping the value.
//!
//! # When is this useful?
//!
//! In many common Tokio services (async request handling on a runtime with a
//! small, fixed number of worker threads), the `RwLock` overhead of
//! `tracing_subscriber::reload::Layer` typically doesn't dominate.
//!
//! This crate is primarily intended for high log volume *and* real OS-thread
//! parallelism (e.g., `tokio::spawn_blocking`, Rayon, or other thread pools),
//! where the reload layer is enabled but reloads are infrequent.
//!
//! # Examples
//!
//! Reloading a global filter:
//!
//! ```rust
//! use tracing::info;
//! use tracing_subscriber::{filter, fmt, prelude::*};
//! use tracing_subscriber_reload_arcswap::ArcSwapLayer;
//!
//! let (filter, handle) = ArcSwapLayer::new(filter::LevelFilter::WARN);
//! tracing_subscriber::registry()
//!     .with(filter)
//!     .with(fmt::layer())
//!     .init();
//!
//! info!("this is ignored");
//! handle.reload(filter::LevelFilter::INFO).unwrap();
//! info!("this is logged");
//! ```
//!
//! Reloading a per-layer filter (preferred):
//!
//! ```rust
//! use tracing::info;
//! use tracing_subscriber::{filter, fmt, prelude::*};
//! use tracing_subscriber_reload_arcswap::ArcSwapLayer;
//!
//! // When possible, wrap the `Filter` directly (rather than a `Filtered` layer),
//! // since `ArcSwapLayer` requires the wrapped type to be `Clone`.
//! let (filter, handle) = ArcSwapLayer::<_, tracing_subscriber::Registry>::new(filter::LevelFilter::WARN);
//! let layer = fmt::layer().with_filter(filter);
//! tracing_subscriber::registry().with(layer).init();
//!
//! info!("this is ignored");
//! handle.reload(filter::LevelFilter::INFO).unwrap();
//! info!("this is logged");
//! ```
//!
//! ## Note
//!
//! Like `tracing_subscriber::reload::Layer`, this wrapper does not provide full
//! downcasting support.

use arc_swap::ArcSwap;
use core::marker::PhantomData;
use std::{
    error, fmt,
    sync::{Arc, Weak},
};
use tracing_core::{
    Dispatch, Event, LevelFilter, Metadata, callsite, span,
    subscriber::{Interest, Subscriber},
};
use tracing_subscriber::{Layer, layer};

/// Wraps a `Layer` or per-layer `Filter` using `arc_swap::ArcSwap`, allowing it
/// to be reloaded dynamically with a lock-free read path.
///
/// This type is intended as a replacement for `tracing_subscriber::reload::Layer`
/// when read-side contention on the `RwLock` becomes a bottleneck.
///
/// If you only need to reload a filter applied with `.with_filter(...)`, prefer
/// wrapping the filter itself (rather than wrapping the resulting
/// [`Filtered`](tracing_subscriber::filter::Filtered) layer), as this type's
/// [`Layer`] implementation requires `L: Clone`.
#[derive(Debug)]
pub struct ArcSwapLayer<L, S> {
    inner: Arc<ArcSwap<L>>,
    modify_lock: Arc<std::sync::Mutex<()>>,
    _s: PhantomData<fn(S)>,
}

/// Allows reloading the state of an associated [`ArcSwapLayer`].
///
/// Use this handle to swap in a new value (`reload`) or to apply a change to the
/// current value (`modify`).
///
/// Note: if the associated layer has been dropped, operations will return an
/// error.
#[derive(Debug)]
pub struct ArcSwapHandle<L, S> {
    inner: Weak<ArcSwap<L>>,
    modify_lock: Weak<std::sync::Mutex<()>>,
    _s: PhantomData<fn(S)>,
}

/// Indicates that an error occurred when reloading a layer.
///
/// This typically means either the associated layer was dropped, or the internal
/// update lock was poisoned by a panic during a previous update.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
}

#[derive(Debug)]
enum ErrorKind {
    SubscriberGone,
    Poisoned,
}

impl Error {
    fn poisoned() -> Self {
        Self {
            kind: ErrorKind::Poisoned,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::SubscriberGone => f.write_str("subscriber is gone"),
            ErrorKind::Poisoned => f.write_str("reload lock poisoned"),
        }
    }
}

impl error::Error for Error {}

impl<L, S> Clone for ArcSwapHandle<L, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            modify_lock: self.modify_lock.clone(),
            _s: PhantomData,
        }
    }
}

impl<L, S> Clone for ArcSwapLayer<L, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            modify_lock: self.modify_lock.clone(),
            _s: PhantomData,
        }
    }
}

impl<L, S> ArcSwapLayer<L, S> {
    /// Wraps the given [`Layer`] or [`Filter`](tracing_subscriber::layer::Filter),
    /// returning an [`ArcSwapLayer`] and an [`ArcSwapHandle`] that allows the
    /// inner value to be modified at runtime.
    ///
    /// Note: the `S` type parameter is the `Subscriber` this layer/filter will
    /// be used with. When using `tracing_subscriber::Registry` (directly or via
    /// `registry().with(...)`), `S` is typically `tracing_subscriber::Registry`.
    ///
    /// The returned handle holds a `Weak` reference; if the layer is dropped,
    /// handle operations will fail with an error.
    pub fn new(inner: L) -> (Self, ArcSwapHandle<L, S>) {
        let this = Self {
            inner: Arc::new(ArcSwap::from_pointee(inner)),
            modify_lock: Arc::new(std::sync::Mutex::new(())),
            _s: PhantomData,
        };
        let handle = this.handle();
        (this, handle)
    }

    /// Returns an [`ArcSwapHandle`] that can be used to reload the wrapped
    /// value.
    ///
    /// Handles can be cloned cheaply.
    pub fn handle(&self) -> ArcSwapHandle<L, S> {
        ArcSwapHandle {
            inner: Arc::downgrade(&self.inner),
            modify_lock: Arc::downgrade(&self.modify_lock),
            _s: PhantomData,
        }
    }
}

impl<L, S> ArcSwapHandle<L, S> {
    /// Atomically replace the current value with `new_value`.
    ///
    /// After swapping, this rebuilds the global callsite interest cache (via
    /// [`tracing_core::callsite::rebuild_interest_cache`]) so cached enablement
    /// and max-level hints are recomputed.
    ///
    /// With the `tracing-log` feature enabled, this also synchronizes `log`'s
    /// max-level.
    pub fn reload(&self, new_value: impl Into<L>) -> Result<(), Error> {
        let inner = self.inner.upgrade().ok_or(Error {
            kind: ErrorKind::SubscriberGone,
        })?;
        let modify_lock = self.modify_lock.upgrade().ok_or(Error {
            kind: ErrorKind::SubscriberGone,
        })?;

        let _guard = modify_lock.lock().map_err(|_| Error::poisoned())?;
        inner.store(Arc::new(new_value.into()));

        callsite::rebuild_interest_cache();

        #[cfg(feature = "tracing-log")]
        tracing_log::log::set_max_level(tracing_log::AsLog::as_log(
            &tracing_subscriber::filter::LevelFilter::current(),
        ));

        Ok(())
    }

    /// Applies an update by cloning the current value, mutating it, and swapping
    /// it back in.
    ///
    /// This is useful when you can't (or don't want to) construct a brand new
    /// `L` just to apply a small change.
    ///
    /// After swapping, this rebuilds the global callsite interest cache (via
    /// [`tracing_core::callsite::rebuild_interest_cache`]) so cached enablement
    /// and max-level hints are recomputed.
    ///
    /// With the `tracing-log` feature enabled, this also synchronizes `log`'s
    /// max-level.
    pub fn modify(&self, f: impl FnOnce(&mut L)) -> Result<(), Error>
    where
        L: Clone,
    {
        let inner = self.inner.upgrade().ok_or(Error {
            kind: ErrorKind::SubscriberGone,
        })?;
        let modify_lock = self.modify_lock.upgrade().ok_or(Error {
            kind: ErrorKind::SubscriberGone,
        })?;

        let _guard = modify_lock.lock().map_err(|_| Error::poisoned())?;

        let current = inner.load_full();
        let mut next = (*current).clone();
        f(&mut next);
        inner.store(Arc::new(next));

        callsite::rebuild_interest_cache();

        #[cfg(feature = "tracing-log")]
        tracing_log::log::set_max_level(tracing_log::AsLog::as_log(
            &tracing_subscriber::filter::LevelFilter::current(),
        ));

        Ok(())
    }

    /// Returns a clone of the current value if the associated layer still
    /// exists.
    ///
    /// If the layer has been dropped, returns `None`.
    pub fn clone_current(&self) -> Option<L>
    where
        L: Clone,
    {
        self.with_current(L::clone).ok()
    }

    /// Runs `f` against the current value, without cloning it.
    ///
    /// This is a convenience for querying state (e.g. formatting a debug view or
    /// extracting some field) without forcing `L: Clone`.
    ///
    /// Returns an error if the associated layer has been dropped.
    pub fn with_current<T>(&self, f: impl FnOnce(&L) -> T) -> Result<T, Error> {
        let inner = self.inner.upgrade().ok_or(Error {
            kind: ErrorKind::SubscriberGone,
        })?;
        let current = inner.load();
        Ok(f(current.as_ref()))
    }
}

impl<L, S> Layer<S> for ArcSwapLayer<L, S>
where
    L: Layer<S> + Clone + 'static,
    S: Subscriber,
{
    fn on_register_dispatch(&self, subscriber: &Dispatch) {
        self.inner.load().on_register_dispatch(subscriber);
    }

    fn on_layer(&mut self, subscriber: &mut S) {
        let _guard = match self.modify_lock.lock() {
            Ok(g) => g,
            Err(_) => {
                if std::thread::panicking() {
                    return;
                }
                panic!("lock poisoned")
            }
        };

        let current = self.inner.load_full();
        let mut next = (*current).clone();
        next.on_layer(subscriber);
        self.inner.store(Arc::new(next));
    }

    #[inline]
    fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
        self.inner.load().register_callsite(metadata)
    }

    #[inline]
    fn enabled(&self, metadata: &Metadata<'_>, ctx: layer::Context<'_, S>) -> bool {
        self.inner.load().enabled(metadata, ctx)
    }

    #[inline]
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_new_span(attrs, id, ctx)
    }

    #[inline]
    fn on_record(&self, span: &span::Id, values: &span::Record<'_>, ctx: layer::Context<'_, S>) {
        self.inner.load().on_record(span, values, ctx)
    }

    #[inline]
    fn on_follows_from(&self, span: &span::Id, follows: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_follows_from(span, follows, ctx)
    }

    #[inline]
    fn event_enabled(&self, event: &Event<'_>, ctx: layer::Context<'_, S>) -> bool {
        self.inner.load().event_enabled(event, ctx)
    }

    #[inline]
    fn on_event(&self, event: &Event<'_>, ctx: layer::Context<'_, S>) {
        self.inner.load().on_event(event, ctx)
    }

    #[inline]
    fn on_enter(&self, id: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_enter(id, ctx)
    }

    #[inline]
    fn on_exit(&self, id: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_exit(id, ctx)
    }

    #[inline]
    fn on_close(&self, id: span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_close(id, ctx)
    }

    #[inline]
    fn on_id_change(&self, old: &span::Id, new: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_id_change(old, new, ctx)
    }

    #[inline]
    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.inner.load().max_level_hint()
    }
}

impl<S, L> tracing_subscriber::layer::Filter<S> for ArcSwapLayer<L, S>
where
    L: tracing_subscriber::layer::Filter<S> + 'static,
    S: Subscriber,
{
    #[inline]
    fn callsite_enabled(&self, metadata: &'static Metadata<'static>) -> Interest {
        self.inner.load().callsite_enabled(metadata)
    }

    #[inline]
    fn enabled(&self, metadata: &Metadata<'_>, ctx: &layer::Context<'_, S>) -> bool {
        self.inner.load().enabled(metadata, ctx)
    }

    #[inline]
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_new_span(attrs, id, ctx)
    }

    #[inline]
    fn on_record(&self, span: &span::Id, values: &span::Record<'_>, ctx: layer::Context<'_, S>) {
        self.inner.load().on_record(span, values, ctx)
    }

    #[inline]
    fn on_enter(&self, id: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_enter(id, ctx)
    }

    #[inline]
    fn on_exit(&self, id: &span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_exit(id, ctx)
    }

    #[inline]
    fn on_close(&self, id: span::Id, ctx: layer::Context<'_, S>) {
        self.inner.load().on_close(id, ctx)
    }

    #[inline]
    fn max_level_hint(&self) -> Option<LevelFilter> {
        self.inner.load().max_level_hint()
    }
}
