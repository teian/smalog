# Rust: Project Structure & Code Style — Best Practices

These are the Rust conventions this workspace follows. Examples are drawn
from smalog's own crates so they stay honest and verifiable; see
[architecture.md](architecture.md) for how the pieces fit together.

## 1. Workspace Layout

For anything beyond a single binary/library, use a Cargo **workspace**. This
is the standard pattern for multi-crate projects:

```
my-project/
├── Cargo.toml              # [workspace] root — no [package]
├── Cargo.lock
├── crates/
│   ├── my-project-core/    # domain types, business logic (no I/O)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── lib.rs
│   ├── my-project-cli/     # binary crate, thin wrapper
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs
│   ├── my-project-api/     # HTTP/gRPC layer
│   └── my-project-storage/ # DB / persistence adapters
├── xtask/                  # optional: cargo-xtask for build automation
└── README.md
```

Root `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/*", "xtask"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT OR Apache-2.0"
rust-version = "1.75"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
thiserror = "1"
anyhow = "1"
tracing = "0.1"
```

Member crates then inherit shared metadata and pin dependency versions
centrally:

```toml
[package]
name = "my-project-core"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
thiserror.workspace = true
```

**Why split into crates at all?**
- Enforces architectural boundaries (core logic can't accidentally reach into the DB layer)
- Faster incremental builds — only changed crates + downstream deps recompile
- Enables independent versioning/publishing if some crates become reusable libraries
- Makes dependency graphs explicit instead of "one giant `mod` tree with everything visible to everything"

**When *not* to split:** small tools, prototypes, or apps under ~3–5k LOC. A
single crate with a clean module tree is often better than premature
multi-crate ceremony.

> **How smalog applies this.** smalog is a workspace with `resolver = "2"`:
> the reusable [`smalog-connection`](../src/crates/smalog-connection/)
> library (no I/O in its shared decoder layer),
> [`smalog-observation`](../src/crates/smalog-observation/) for the canonical
> protocol-neutral Poll Cycle contract,
> [`smalog-storage`](../src/crates/smalog-storage/) for persistence,
> [`smalog-export`](../src/crates/smalog-export/) for external output, and the
> [`smalog`](../src/crates/smalog/) binary that wraps it. Two intentional
> deviations from the template above: crates live
> under `src/crates/<name>` (not `crates/`), and the license is
> `EUPL-1.2`, not MIT/Apache. Dependency
> versions are currently pinned per-crate rather than via
> `[workspace.dependencies]`; centralising them is a reasonable future
> cleanup.

---

## 2. Module Structure Inside a Crate

Prefer the modern (2018+) module style — **no `mod.rs`**. A module with
submodules is a file (`smadata1.rs`) sitting *next to* a directory of the same
name (`smadata1/`).

smalog's `smalog-connection` crate:

```
src/crates/smalog-connection/src/
├── lib.rs                # table of contents + re-exports
├── error.rs              # crate-wide error type
├── collector.rs          # the unified poll loop
├── connection.rs         # common Connection trait
├── smadata2.rs           # shared SMA Data2+ application protocol
├── smadata2/
│   ├── commands.rs       # command / LRI constants
│   ├── decode.rs         # record decoding
│   ├── archive.rs        # day/month/event parsing
│   ├── inverter.rs       # InverterData state
│   ├── tags.rs           # SMA tag text lookup
│   └── data/             # embedded localized UTF-8 JSON tag documents
├── bluetooth.rs          # SMA Data 2 Plus over RFCOMM
├── bluetooth/
│   ├── frame.rs
│   ├── socket.rs         # the BtSocket trait + platform selection
│   ├── linux.rs          # #[cfg(target_os = "linux")]
│   ├── windows.rs        # #[cfg(target_os = "windows")]
│   └── unsupported.rs
├── speedwire.rs          # Ethernet/Speedwire implementation
├── speedwire/
│   ├── conn.rs           # UDP socket
│   └── packet.rs         # Speedwire datagram framing
├── smadata1.rs           # shared SMA Data V1 abstraction
└── smadata1/
    ├── rs232.rs          # point-to-point serial boundary
    ├── rs485.rs          # SMA-Net multi-point boundary
    └── powerline.rs      # Sunny-Net/Powerline boundary
```

Rules of thumb:
- `lib.rs` should mostly be `pub mod` declarations + re-exports (a "table of contents"), not logic.
- One concept per file. If a file exceeds ~500–800 lines, it's a signal to split.
- Keep `pub` surface minimal — default to private, widen deliberately. Use `pub(crate)` for cross-module-but-not-external visibility.

### 2018+ module style in practice (no `mod.rs`)

**`src/lib.rs`** — declarations and re-exports only, no logic:

```rust
//! Shared SMA inverter connection library.
//!
//! Exposes one [`Connection`] interface for Speedwire, Bluetooth and
//! SMA Data V1 transports.

pub mod collector;
pub mod bluetooth;
pub mod connection;
pub mod error;
pub mod smadata1;
pub mod smadata2;
pub mod speedwire;

pub use collector::{Collector, PollOptions};
pub use connection::{Connection, DeviceId, UserGroup};
pub use error::{Error, Result};
```

**`src/connection.rs`** — the crate-wide interface:

```rust
//! Shared interface for every supported SMA connection type.

/// A transport session against one or more SMA inverters.
#[async_trait::async_trait]
pub trait Connection: Send {
    /// The inverters this connector talks to, known after [`begin`].
    fn devices(&self) -> Vec<DeviceId>;

    /// Start a poll session (open/reuse socket, enumerate).
    async fn begin(&mut self) -> Result<()>;
    // …
}
```

**`src/smadata1.rs`** — the SMA Data V1 protocol-family interface:

```rust
pub mod powerline;
pub mod rs232;
pub mod rs485;

pub trait SmaData1Connection: Connection {
    fn medium(&self) -> SmaData1Medium;
}
```

**`src/error.rs`** — one crate-wide error type:

```rust
//! Error type for SMA connections — transport/protocol failures only.

use thiserror::Error;

/// Errors from talking to SMA inverters.
#[derive(Debug, Error)]
pub enum Error {
    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The device reported a protocol-level error.
    #[error("SMA connection protocol error: {0}")]
    Protocol(String),

    /// No response within the retry budget.
    #[error("timeout waiting for inverter response")]
    Timeout,

    /// Login rejected (wrong password?).
    #[error("inverter {serial}: login failed (wrong password?)")]
    LoginFailed { serial: u32 },
}

pub type Result<T> = std::result::Result<T, Error>;
```

Key points:

- **`smadata1.rs` + `smadata1/` living side by side** is the 2018+ replacement for `smadata1/mod.rs`. Rustfmt and rust-analyzer both handle this natively.
- **`//!` inner doc comments** at the top of each file document the module itself (shown as the module's landing page in `cargo doc`).
- **`///` outer doc comments** document the item immediately following them — types, functions, fields.
- Cross-references like `` [`Error::Timeout`] `` become clickable intra-doc links — this only works if the referenced item is `pub` and in scope or given a full path.
- `# Errors` and `# Panics` sections are a convention, checked by `clippy::missing_errors_doc` if enabled — worth turning on for any API others consume.

---

## 3. Error Handling

- **Libraries**: define your own error enum with `thiserror`. Never leak `anyhow::Error` from a library's public API.
- **Binaries/applications**: `anyhow` (or `color-eyre`) at the top level is fine — you don't need typed errors if nothing downstream matches on them.

smalog follows exactly this split: the `smalog-connection` crate exposes a
typed `Error` enum (above), and the `smalog` binary wraps it via `#[from]`.

```rust
// smalog (app) error.rs — wraps the library error
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error(transparent)]
    Connection(#[from] smalog_connection::Error),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}
```

Never use `.unwrap()`/`.expect()` outside of tests and truly-infallible
invariants (document *why* with a comment when you do).

---

## 4. Style & Formatting Standards

### Tooling (non-negotiable baseline)
- `rustfmt` — run in CI (`cargo fmt --check`), never hand-format.
- `clippy` — run with `cargo clippy --all-targets --all-features -- -D warnings` in CI.
- Optional but recommended: `cargo deny` (license/advisory checks), `cargo audit` (vulnerability scanning).

`rustfmt.toml` (project-level overrides, keep minimal):
```toml
edition = "2021"
max_width = 100
use_small_heuristics = "Max"
imports_granularity = "Module"
group_imports = "StdExternalCrate"
```

> Note: `imports_granularity` and `group_imports` are **nightly-only** rustfmt
> options — stable `cargo fmt` ignores them with a warning. smalog runs
> stable rustfmt with defaults, and organises imports by hand into the three
> groups shown below.

### Naming conventions
| Item | Convention | Example |
|---|---|---|
| Crates | `kebab-case` | `my-project-core` |
| Modules/files | `snake_case` | `global_policy.rs` |
| Types, Traits, Enums | `UpperCamelCase` | `SpeedwireConnection`, `UserGroup` |
| Functions, variables | `snake_case` | `request_all()` |
| Constants, statics | `SCREAMING_SNAKE_CASE` | `MAX_RETRY` |
| Type parameters | short `UpperCamelCase` | `T`, `E`, `S` |
| Lifetimes | short lowercase | `'a`, `'de` |

Prefer full words over cryptic abbreviations for local names
(`password` over `pw`, `index` over `idx`); the exception is established
domain/protocol terms (`susy_id`, `pckt_id`).

### Imports
Group and order (std, then external crates, then `crate`), a blank line
between groups:
```rust
use std::collections::HashMap;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::Error;
```
Avoid glob imports (`use foo::*`) except in test modules or well-scoped preludes.

### Documentation
- Every `pub` item gets a `///` doc comment.
- Crate-level docs go at the top of `lib.rs` with `//!`.
- Run `cargo doc --open` periodically; treat missing docs as a lint (`#![warn(missing_docs)]` for libraries).

```rust
/// One request to all devices; response frames are normalized to the
/// ethernet datagram layout and keyed by inverter serial.
///
/// # Errors
/// Returns [`Error::Timeout`] if no device answers within the retry budget.
async fn request_all(&mut self, command: u32) -> Result<HashMap<u32, Vec<Vec<u8>>>> {
    // …
}
```

### Type & API design
- Prefer `impl Trait` / generics over `Box<dyn Trait>` unless dynamic dispatch is genuinely needed (plugin-style systems, heterogeneous collections). smalog's `BluetoothConnection<S: BtSocket>` is generic over the OS socket; the app boxes it as `Box<dyn Connection>` only where runtime transport selection requires it.
- Use the newtype pattern for domain concepts instead of raw primitives:
  ```rust
  pub struct Serial(u32);
  pub struct SecretPath(String);
  ```
- Builder pattern for structs with many optional fields, rather than constructors with 6+ positional args.
- Derive generously and consistently: `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]` as a starting default, trimmed to what's actually needed.

### Testing
```
src/…/kv.rs           # #[cfg(test)] mod tests at bottom for unit tests
tests/
└── integration_*.rs  # black-box integration tests against public API
```
- Unit tests live in the same file as the code (`#[cfg(test)] mod tests`).
- Integration tests go in `tests/` — they only see the crate's public API, which is a good forcing function for API design. (smalog keeps its tests here — protocol decode, config, csv, storage round-trips.)
- Use `rstest` or table-driven tests for parametrized cases.

---

## 5. CI Baseline (GitHub Actions example)

```yaml
name: CI
on: [push, pull_request]
jobs:
  check:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          components: rustfmt, clippy
      - run: cargo fmt --check
      - run: cargo clippy --all-targets --all-features -- -D warnings
      - run: cargo test --workspace
      - run: cargo doc --no-deps
```

---

## 6. Quick Checklist

- [ ] Workspace with `resolver = "2"` if multi-crate
- [ ] Shared deps/metadata via `[workspace.dependencies]` / `[workspace.package]`
- [ ] One concept per module, minimal `pub` surface
- [ ] `thiserror` for library errors, `anyhow` only at binary boundary
- [ ] `rustfmt` + `clippy -D warnings` enforced in CI
- [ ] Doc comments on all public items
- [ ] Newtypes over raw `String`/`u64` for domain identifiers
- [ ] Unit tests co-located, integration tests in `tests/`
