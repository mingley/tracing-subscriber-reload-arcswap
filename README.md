# tracing-subscriber-reload-arcswap

This crate exists because the `tracing-subscriber` maintainers asked that an `arc-swap`-based
reload layer be split out into a separate crate rather than adding a new feature to
`tracing-subscriber`.

TL;DR: a functionally-equivalent, drop-in alternative to
[`tracing_subscriber::reload::Layer`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/reload/struct.Layer.html)
that is typically comparable and can be 8x+ faster under high OS-thread parallelism (e.g.
`tokio::spawn_blocking`, Rayon, or other thread pools); see Benchmarks.

Context:
- Original `tracing-subscriber` PR/discussion: https://github.com/tokio-rs/tracing/pull/3438
- Motivating issue (this crate addresses it): https://github.com/tokio-rs/tracing/issues/2658

## What it is

`tracing_subscriber_reload_arcswap::ArcSwapLayer` is intended as a pragmatic replacement for
[`tracing_subscriber::reload::Layer`](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/reload/struct.Layer.html).

It provides the same core behavior:
- wrap a `Layer` or per-layer `Filter`
- update it at runtime (`reload`/`modify`)
- rebuild the callsite interest/max-level cache after updates (so changes take effect promptly)

The primary difference is implementation strategy:
- `tracing_subscriber::reload::Layer` uses an `RwLock` (every span/event hits the lock on the read path)
- this crate uses `arc-swap` for a lock-free read path (reload/modify are serialized; they’re expected to be rare)

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

On an Apple M4 Pro (14 cores, 48GB; macOS 26.2; `rustc 1.92.0`), Criterion point estimates for the
benchmarks that originally motivated this crate were:

| Benchmark | Baseline (no reload) | `reload::Layer` (`RwLock`) | `ArcSwapLayer` (`ArcSwap`) |
| --- | ---: | ---: | ---: |
| `single_threaded` | 4.88 ns | 8.90 ns (1.82x) | 9.58 ns (1.96x) |
| `multithreaded_16x1000` (`std::thread`) | 67.2 µs | 11.9 ms (177x) | 71.7 µs (1.07x) |
| `tokio_spawn_blocking_16x1000` | 57.1 µs | 12.8 ms (223x) | 62.8 µs (1.10x) |
| `rayon_16x1000` | 39.4 µs | 15.0 ms (380x) | 51.9 µs (1.32x) |

These results show why the crate exists:
- in “normal” single-threaded paths, `ArcSwapLayer` is in the same ballpark as `reload::Layer`
- under high OS-thread parallelism (a setup that can happen in real services via `spawn_blocking`, Rayon, or other thread pools),
  the `RwLock` read-side overhead can dominate even when you never reload — `ArcSwapLayer` avoids that contention

## Optional features

- `tracing-log`: updates `log`'s max-level after reload/modify.

## References

- `tokio-rs/tracing` issue: https://github.com/tokio-rs/tracing/issues/2658

## Author

Michael Ingley <michael.ingley@gmail.com>
