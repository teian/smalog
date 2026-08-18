## 1. Fix the shipped systemd unit

- [x] 1.1 Point `Documentation=` in `packaging/smalog.service` at
      `https://github.com/teian/smalog` instead of the SBFspot repository.
- [x] 1.2 Change `ExecStart` to `/usr/bin/smalog`, since a package may not
      write under `/usr/local`.
- [x] 1.3 Update the comment above `User=` so the manual-install instructions
      still match what the unit expects.

## 1b. Unblock the 32-bit build (found while implementing)

- [x] 1b.1 Widen `f_bavail` and `f_frsize` to `u64` before multiplying in
      `smalog-sbfspot-migrator`. On 32-bit targets both are `u32`, so the
      armv7 release build did not compile at all — and had it compiled, the
      product would saturate at 4 GiB and understate the free space on any
      card larger than that. Present since the initial commit, so the armv7
      release artifact has never been produced.

## 2. Package metadata (`src/crates/smalog/Cargo.toml`)

- [x] 2.1 Add `[package.metadata.deb]` with maintainer, section, priority and
      an extended description; the package name, version, licence and homepage
      come from the existing `[package]` keys so a version bump cannot leave
      them stale.
- [x] 2.2 Declare `assets`: the binary to `usr/bin/` (0755),
      `config.example.toml` to `etc/smalog/` (0644), and `README.md` plus
      `LICENSE.md` to `usr/share/doc/smalog/` (0644).
- [x] 2.3 Declare `conf-files` for `/etc/smalog/config.example.toml` only, so
      dpkg never owns, prompts about or deletes the operator's
      `/etc/smalog/config.toml`.
- [x] 2.4 Determine the real runtime dependencies with
      `objdump -p target/<triple>/release/smalog | grep NEEDED` for both ARM
      targets and write them out explicitly; `$auto` cannot work when an
      x86-64 runner packages an ARM binary.
- [x] 2.5 Add `[package.metadata.deb.systemd-units]` with `enable = false` and
      `start = false`, so the unit is registered and restarted on upgrade but
      never started by the package.

## 3. Maintainer scripts (`packaging/`)

- [x] 3.1 Add a `postinst` fragment that creates the `smalog` system user and
      group with no login shell, idempotently, and creates `/var/lib/smalog`
      owned by it — both must succeed on a reinstall and an upgrade.
- [x] 3.2 Print the two remaining steps after a fresh install: write
      `/etc/smalog/config.toml`, then `systemctl enable --now smalog`.
- [x] 3.3 Add a `postrm` fragment that on `purge` removes `/var/lib/smalog`
      and the `smalog` user, and on plain removal removes neither.
- [x] 3.4 Verify the fragments compose with the ones `cargo-deb` generates for
      the systemd unit rather than replacing them.

## 4. Release pipeline (`.github/workflows/ci.yml`)

- [x] 4.1 Add a `deb` flag to the `release-build` matrix, set for the two ARM
      targets only, so the packaging step is skipped for i686 and amd64
      without duplicating the job.
- [x] 4.2 Install `cargo-deb` pinned to an exact version, the way `cross` is
      already pinned, so generated maintainer scripts change only when the pin
      changes.
- [x] 4.3 Run `cargo deb --no-build --target <triple> --package smalog` after
      the existing `cross build`, so the package contains the same binary as
      the archive.
- [x] 4.4 Upload the `.deb` with the existing release artifact for that
      architecture.
- [x] 4.5 In the `release` job, include the `.deb` files in `SHA256SUMS` and
      in the assets uploaded to the release.
- [x] 4.6 Confirm the non-release CI path is unchanged: packaging must run only
      for `github.event_name == 'release'`.

## 5. Verification

- [x] 5.1 Build both packages locally with `cross` plus `cargo deb --no-build`
      and inspect them with `dpkg-deb --info` and `--contents`: declared
      architecture, paths, modes, and nothing under `/usr/local`.
- [x] 5.2 Run `lintian` on both packages and resolve or consciously accept
      every tag it reports.
- [x] 5.3 Install each package in a clean Debian container for its
      architecture (`podman run --platform linux/arm64` with
      `qemu-user-static`) using `apt install ./…deb`, and confirm it resolves
      dependencies from the distribution's own repositories.
- [x] 5.4 In that container, confirm the install created the user and
      `/var/lib/smalog`, registered the unit, and left the service neither
      running nor enabled.
- [x] 5.5 Run `smalog --version` under emulation and confirm it reports the
      packaged version.
- [x] 5.6 Exercise upgrade: install, write a `/etc/smalog/config.toml`, enable
      the service, install a newer package, and confirm the configuration is
      untouched, dpkg asked nothing, and the service is still enabled.
- [x] 5.7 Exercise removal and purge: removal stops the service and keeps
      `/var/lib/smalog`; purge removes the state directory and the user but
      leaves `/etc/smalog/config.toml` and the secrets file.
- [x] 5.8 Confirm the archive and the package for one architecture contain
      byte-identical binaries.

## 6. Documentation

- [x] 6.1 `README.md`: add the package as an install option beside the release
      archive and Docker, with the `apt install ./…deb` line and the two steps
      that follow it.
- [x] 6.2 `README.md` and `docs/operations.md`: update the manual-install
      instructions for the `/usr/bin` path, and state what an operator with an
      existing `/usr/local/bin` install has to change.
- [x] 6.3 `docs/operations.md`: describe what the package does on install,
      upgrade, removal and purge, and warn that a unit copied into
      `/etc/systemd/system/` overrides the packaged one.
- [x] 6.4 State in the release notes that `ExecStart` changed, since a
      hand-built install breaks on upgrade without the one-line edit.
