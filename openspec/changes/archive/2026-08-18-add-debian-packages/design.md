## Context

See [proposal.md](proposal.md) — Why. What shapes the approach:

- `release-build` already cross-compiles all four targets with `cross`, after
  downloading the `ui-dist` artifact, and uploads one tar.gz per architecture.
  The two ARM targets it produces are exactly the two the packages need, so
  packaging is a step in that job rather than a job of its own.
- The runner is `ubuntu-latest` on x86-64. Everything about the ARM packages
  is therefore produced by a machine that cannot execute their contents —
  which rules out any packaging step that wants to run the binary, and makes
  dependency detection the one genuinely awkward part.
- The workspace release profile sets `strip = true`, so the binary arrives
  already stripped and `cargo-deb`'s own strip step has nothing to do.
- `packaging/smalog.service` expects a `smalog` system user, reads secrets
  through `EnvironmentFile=-/etc/smalog/smalog.env`, uses
  `StateDirectory=smalog`, and today points `ExecStart` at
  `/usr/local/bin/smalog` — a path Debian policy reserves for the local
  administrator and forbids a package from writing.
- `config.example.toml` is the documented starting point for a configuration,
  and `docs/configuration.md` states that an unset `${VAR}` is a hard error.
  A configuration that has not been written yet cannot start the service.

## Goals / Non-Goals

**Goals:**

- One `apt install ./…deb` that leaves a Raspberry Pi ready to be configured,
  with the integration steps the README currently spells out already done.
- Upgrades that keep the operator's configuration and data untouched, and that
  restart an already-enabled service on the new binary.
- No new build target, no second toolchain, no separate release job.

**Non-Goals:**

- No APT repository, no signing, no `apt update` upgrade path. The packages
  are release assets installed by file, like the archives beside them.
- No `.rpm`, no Alpine, no other package format.
- No i386 or amd64 package. Both targets exist in the release matrix and could
  be added later by one matrix flag; neither is a Raspberry Pi.
- No change to the binary or its behavior.

## Decisions

### `cargo-deb`, packaging the artifact `cross` already built

`cargo deb --no-build --target <triple>` takes the binary from
`target/<triple>/release/` and packages it. That keeps one build of one binary
per architecture: the `.deb` and the tar.gz for a given release contain the
same bytes, because they come from the same `cross build`.

Metadata lives in `[package.metadata.deb]` in `src/crates/smalog/Cargo.toml`,
next to the version, description, licence and repository the package needs
anyway — so a version bump cannot leave the package describing the previous
release.

*Alternative — `nfpm`:* language-agnostic and equally capable, but it puts the
package metadata in a second file that has to be kept in step with
`Cargo.toml` by hand, and adds a Go tool to a release path that currently has
only Rust and `cross`. Rejected.

*Alternative — `dpkg-deb` by hand:* no new dependency, but `control`,
`md5sums`, file modes, the `.deb` layout and the maintainer-script conventions
all become ours to maintain, for a package whose contents are four files.
Rejected.

### Explicit dependencies, not `$auto`

`cargo-deb`'s `$auto` resolves dependencies by inspecting the built binary
with the host's linker tooling. On an x86-64 runner packaging an ARM binary
that either fails or, worse, silently produces the host's dependency names.

The dependencies are therefore written out. The binary is a Rust program with
`rustls-ring` rather than OpenSSL, SQLite bundled through `libsqlite3-sys`,
and Bluetooth over raw sockets rather than `libbluetooth`, so what it needs
from the distribution is glibc and the GCC runtime. The task list verifies
that claim against the actual `NEEDED` entries with
`objdump -p` per architecture rather than trusting this paragraph — if the
list is wrong, `apt` reports it at install time, which is exactly the failure
mode the package exists to prevent.

### The package registers the unit but does not start it

`[package.metadata.deb.systemd-units]` with `enable = false` and
`start = false` generates the maintainer-script fragments that make systemd
aware of the unit, restart it on upgrade if it is already running, and stop
and deregister it on removal — without turning it on.

That last part is the reason for using the generated fragments rather than
writing them: `deb-systemd-invoke` and the restart-on-upgrade dance are fiddly
to get right by hand, and getting them wrong is only visible on the operator's
machine during an upgrade.

Not starting is a deliberate departure from what many service packages do.
`config.toml` cannot be shipped — it names inverters, addresses and passwords —
so on a fresh install the service would fail immediately, and `RestartSec=30`
would make that a failure every thirty seconds until someone noticed. The
package prints the two remaining steps instead.

### `config.example.toml` is the conffile; `config.toml` is never created

Shipping the example as `/etc/smalog/config.example.toml` gives dpkg something
it owns and can reason about, while the operator's real file at
`/etc/smalog/config.toml` is one dpkg has never heard of. Nothing in the
package lifecycle can then modify, prompt about or delete it — not upgrade,
not purge.

The alternative, shipping `config.toml` itself as a conffile, is friendlier on
the very first install and worse on every one after it: dpkg would prompt on
each upgrade that touches a file the operator has necessarily edited, over a
file whose shipped content cannot work anyway.

The same reasoning covers `/etc/smalog/smalog.env`: secrets are the operator's
to place, and a package that created it would own it.

### `ExecStart` moves to `/usr/bin/smalog`

Debian policy forbids a package writing under `/usr/local`, so the packaged
binary goes to `/usr/bin` and the unit must name it.

There is one unit file, shared by the package and by the tarball instructions.
Rather than keep two, the unit moves to `/usr/bin` and the archive
instructions move with it: an operator installing by hand now places the
binary in `/usr/bin` too, and their `config.toml`, data and service name are
unaffected.

This breaks exactly one thing: an existing hand-built install that put the
binary in `/usr/local/bin` and copied the unit. That is a one-line edit, and
the release notes and README say so rather than letting the service fail to
start after an upgrade with a confusing "No such file or directory".

*Alternative — parameterise the path:* systemd has no include mechanism that
would help here, and shipping two units invites installing the wrong one.
Rejected.

### Verification runs the packages under emulation

An x86-64 runner cannot execute an ARM binary, but it can inspect and install
the package: `dpkg-deb --contents` and `--info` check the layout and metadata
without emulation, and `podman run --platform linux/arm64` with
`qemu-user-static` installs the package in a real Debian container and runs
`smalog --version`.

That covers what actually breaks in packaging — wrong paths, wrong
permissions, missing dependencies, maintainer scripts that fail — on the
architectures that matter, without a Pi in the loop. Install, upgrade, remove
and purge are each exercised there.

### The lintian tags we keep

Three tags remain after fixing what was fixable, each accepted for a stated
reason rather than silenced:

- `copyright-not-using-common-license-for-lgpl` — lintian sees LGPL wording
  inside the EUPL-1.2 text and asks us to reference
  `/usr/share/common-licenses` instead of shipping it. Debian does not ship
  EUPL there, so the full text is the only correct thing to include.
- `initial-upload-closes-no-bugs` — the changelog should close an ITP bug.
  That applies to an upload to the Debian archive; these packages are release
  assets installed by file.
- `no-manual-page` — `smalog --help` documents every subcommand and flag, and
  the reference material is `docs/`. A man page duplicating either would be
  one more thing to keep in step.

Fixed rather than accepted: the synopsis no longer repeats the package name,
`adduser` is declared because `postinst` calls it and it is a separate package
since bookworm, and a Debian changelog is shipped — CI rewrites it from the
release tag so it cannot describe the previous version.

## Risks / Trade-offs

- **A wrong dependency list only shows on the operator's machine.** → The
  emulated install is a real `apt install` in a clean Debian container: a
  missing dependency fails there. The `objdump -p` check names what the binary
  actually links, rather than what this document assumes.
- **`cargo-deb` derives the Debian architecture from the target triple**, and a
  mismatch would produce a package that installs on the wrong hardware. → The
  emulated install runs per architecture, and `dpkg-deb --info` asserts the
  declared architecture.
- **Not starting the service will surprise someone** who expects a Debian
  service package to come up on install. → The install prints the remaining
  steps, and the README and operations doc describe the sequence. The opposite
  default — a service failing every thirty seconds on a fresh install — is
  worse and harder to diagnose.
- **The `ExecStart` change breaks existing hand-built installs.** → Called out
  in the proposal, the release notes and the README, with the one-line fix.
  It is a breaking change to a file we ship, not to configuration or data.
- **A `cargo-deb` upgrade could change generated maintainer scripts.** → The
  tool is pinned in CI like `cross` already is, so the scripts change when we
  change the pin, not when upstream releases.
- **Packages are installed by file, so there is no automatic upgrade path.** →
  Stated as a non-goal; an APT repository is a separate piece of work with its
  own signing and hosting decisions.

## Migration Plan

Nothing to migrate for existing data or configuration: the package touches
neither, and an operator upgrading from a tarball install keeps
`/etc/smalog/config.toml` and `/var/lib/smalog` as they are.

For an operator moving from a hand-built install to the package: install the
package, then remove the old `/usr/local/bin/smalog` and any hand-copied unit
file, since the package's unit at `/lib/systemd/system/smalog.service`
supersedes a copy in `/etc/systemd/system/`. A copy left in `/etc` wins over
the package's unit and would keep pointing at the removed binary — the
operations doc says so explicitly.

Rollback is `apt remove smalog` plus reinstating the previous binary; data and
configuration survive both directions because the package owns neither.
