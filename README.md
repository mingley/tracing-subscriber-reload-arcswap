# tracing-subscriber-reload-arcswap

Lock-free reload support for `tracing-subscriber` layers and filters, using `arc-swap`.

This is a pragmatic alternative to `tracing_subscriber::reload::Layer` that avoids acquiring an
`RwLock` on the hot path of span/event processing. Reloads are serialized internally and still
rebuild the callsite interest cache.

## Usage

```rust
use tracing::info;
use tracing_subscriber::{filter, fmt, prelude::*};
use tracing_subscriber_reload_arcswap::ArcSwapLayer;

let (filter, handle) = ArcSwapLayer::new(filter::LevelFilter::WARN);
tracing_subscriber::registry()
    .with(filter)
    .with(fmt::layer())
    .init();

info!("this is ignored");
handle.reload(filter::LevelFilter::INFO).unwrap();
info!("this is logged");
```

For per-layer filtering, prefer wrapping the filter directly:

```rust
use tracing_subscriber::{filter, fmt, prelude::*};
use tracing_subscriber_reload_arcswap::ArcSwapLayer;

let (filter, handle) =
    ArcSwapLayer::<_, tracing_subscriber::Registry>::new(filter::LevelFilter::WARN);
let layer = fmt::layer().with_filter(filter);
tracing_subscriber::registry().with(layer).init();

handle.reload(filter::LevelFilter::INFO).unwrap();
```

## Benchmarks

`cargo bench`

The multi-threaded benchmarks intentionally construct OS-thread parallelism (via `std::thread`,
`tokio::spawn_blocking`, and a Rayon pool) to exacerbate read-side synchronization contention. This
is not representative of typical Tokio async request-handling on a small number of runtime worker
threads.

## Optional features

- `tracing-log`: updates `log`'s max-level after reload/modify.

## References

- `tokio-rs/tracing` issue: https://github.com/tokio-rs/tracing/issues/2658

## Author

Michael Ingley <michael.ingley@gmail.com>
