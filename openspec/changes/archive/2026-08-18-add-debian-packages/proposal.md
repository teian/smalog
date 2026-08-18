## Why

A release ships tar.gz archives and a multi-architecture Docker image.
Neither fits the deployment this project was built for: a Raspberry Pi running
Raspberry Pi OS or Debian, polling inverters on the local network.

The archive leaves every integration step to the reader — create a system
user, place the binary, install the unit, create `/var/lib/smalog`, set
permissions on the secrets file — and leaves upgrades entirely manual, with no
record of what was installed. Docker is the wrong shape here: Speedwire
discovery needs multicast on `239.12.255.254:9522`, which bridge networking
does not pass, and Bluetooth needs the host's adapter, so a Pi user ends up
with `--network host` and device passthrough to reach what a native install
has by default.

`apt install ./smalog_…_arm64.deb` does the integration steps once, correctly,
and makes the upgrade path `dpkg` instead of a remembered sequence of `install`
commands.

## What Changes

- Build a `.deb` for **armhf** (`armv7-unknown-linux-gnueabihf`, Pi 2/3/4 on a
  32-bit OS) and **arm64** (`aarch64-unknown-linux-gnu`, Pi 3/4/5 on a 64-bit
  OS) from the binaries the release job already cross-compiles. No new build
  targets, no second toolchain: `cargo-deb` packages the existing artifact with
  `--no-build --target`.
- Package contents: `/usr/bin/smalog`, the systemd unit at
  `/lib/systemd/system/smalog.service`, `config.example.toml` and the docs
  under `/etc/smalog/` and `/usr/share/doc/smalog/`.
- Create the `smalog` system user and `/var/lib/smalog` on install, and
  register the unit — but **do not enable or start it**. Without a
  `/etc/smalog/config.toml` naming real inverters the first start is
  guaranteed to fail, and `Restart=on-failure` with `RestartSec=30` would turn
  that into a permanent error every half minute. The package prints what to do
  next instead.
- Ship `config.example.toml` to `/etc/smalog/config.example.toml` and **never**
  create `/etc/smalog/config.toml`. The operator's real configuration is then
  a file dpkg has never seen, so no upgrade can prompt about it or overwrite
  it.
- Attach both packages to the GitHub release next to the archives, and include
  them in `SHA256SUMS`.
- Correct `Documentation=` in the systemd unit, which points at the SBFspot
  repository rather than this one. The unit is what the package installs, so
  the wrong link would ship to every installed host.
- **BREAKING for the unit file:** `ExecStart` moves from `/usr/local/bin/smalog`
  to `/usr/bin/smalog`, because Debian policy forbids a package writing to
  `/usr/local`. Tarball users who already installed to `/usr/local/bin` and
  copied the unit keep a working setup only if they adjust one line; the
  release notes and README say so.

## Capabilities

### New Capabilities

- `debian-packages`: what a smalog `.deb` contains, where it puts it, what it
  does to the system on install, upgrade, removal and purge, and which
  architectures are published.

### Modified Capabilities

<!-- None: no existing capability describes packaging or release artifacts. -->

## Impact

- `.github/workflows/ci.yml`: the `release-build` job gains a packaging step
  for the two ARM targets and uploads the `.deb` alongside the archive; the
  `release` job adds them to `SHA256SUMS` and to the uploaded assets.
- `src/crates/smalog/Cargo.toml`: a `[package.metadata.deb]` section declaring
  assets, the systemd unit, conffiles, maintainer scripts and dependencies.
- `packaging/`: maintainer scripts (`postinst`, `prerm`, `postrm`), and the
  corrected `smalog.service`.
- `docs/operations.md` and `README.md`: an install path via `apt install ./…`
  next to the archive and Docker instructions, and the changed `ExecStart`.
- No change to the binary, its configuration, its schema or its behavior. This
  is packaging only: the same binary, installed properly.
