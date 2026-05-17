# OS Threads + Crossbeam Channels Over Full Async

TermSDR uses OS threads with `crossbeam-channel` for inter-thread communication instead of running everything on a tokio async runtime. The only async code is the WebSocket server (`server/`), which runs its own `tokio::Runtime`.

The core pipeline — capture → DSP/FFT → audio demodulation → sink — is CPU-bound and blocking. `cpal` (audio output) is synchronous and callback-driven. `crossterm`/`ratatui` (TUI) run a blocking event loop. Async adds no value here and would require `spawn_blocking` everywhere, adding complexity for zero benefit.

Tokio enters only where it belongs: the WebSocket server, which is I/O-bound and benefits from async connection handling.

## Considered Options

- **Full async with tokio** — rejected because the DSP pipeline is CPU-bound, `cpal` is synchronous, and wrapping everything in `spawn_blocking` adds indirection without improving throughput or latency.
- **OS threads + `std::sync::mpsc`** — viable, but `crossbeam-channel` offers bounded channels, `select!`, and better performance under contention, which matters for the real-time audio pipeline.

## Consequences

- Adding new pipeline stages (new source types, demod modes) follows the existing thread + channel pattern.
- The WebSocket server is the only module that needs async awareness.
- If we ever need async in more places (e.g., HTTP API for remote control), it stays isolated in `server/` — the DSP core remains synchronous.
