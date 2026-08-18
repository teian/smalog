# Debian Packages Specification

## Purpose

Defines the Debian packages smalog publishes for Raspberry Pi hosts: what they
install, what they do to the system, and what they promise across upgrade,
removal and purge.

## Requirements

### Requirement: Published architectures

Every release SHALL publish a `.deb` for `armhf` and for `arm64`, built from
the same cross-compiled binaries as the release archives for
`armv7-unknown-linux-gnueabihf` and `aarch64-unknown-linux-gnu`.

Each package SHALL declare the Debian architecture matching its binary, so
`dpkg` refuses an install on the wrong hardware rather than producing a
service that cannot start. Package file names SHALL carry the release version
and the architecture.

#### Scenario: Both Raspberry Pi architectures are published

- **WHEN** a release is published
- **THEN** the release assets include one `armhf` and one `arm64` `.deb`, each
  named with the release version

#### Scenario: A package refuses the wrong architecture

- **WHEN** the `armhf` package is installed on an `arm64` host that does not
  enable multi-arch
- **THEN** `dpkg` refuses the install, naming the architecture mismatch

#### Scenario: Packages are checksummed with the archives

- **WHEN** a release is published
- **THEN** `SHA256SUMS` covers both `.deb` files as well as the archives

### Requirement: Package contents and paths

A package SHALL install:

- the binary as `/usr/bin/smalog`, executable;
- the systemd unit as `/lib/systemd/system/smalog.service`;
- `config.example.toml` as `/etc/smalog/config.example.toml`;
- the README and the licence under `/usr/share/doc/smalog/`.

A package SHALL NOT write anything under `/usr/local`, which Debian policy
reserves for the local administrator.

The unit's `Documentation=` SHALL point at this project's repository, and its
`ExecStart` SHALL name the packaged binary path.

#### Scenario: Files land where Debian expects them

- **WHEN** the package is installed
- **THEN** `/usr/bin/smalog` is executable, the unit is at
  `/lib/systemd/system/smalog.service`, and nothing was written under
  `/usr/local`

#### Scenario: The installed unit runs the installed binary

- **WHEN** the installed unit is read
- **THEN** its `ExecStart` names `/usr/bin/smalog` and its `Documentation=`
  names this project's repository

### Requirement: System preparation on install

On install the package SHALL create the `smalog` system user and group used by
the unit, create `/var/lib/smalog` owned by that user, and make systemd aware
of the new unit.

The system user SHALL have no login shell and no home directory beyond its
state directory. Installing twice, or upgrading, SHALL NOT fail because the
user or the directory already exists.

#### Scenario: A fresh install prepares the system

- **WHEN** the package is installed on a host that has never had smalog
- **THEN** the `smalog` system user exists, `/var/lib/smalog` exists and is
  owned by it, and systemd lists the unit

#### Scenario: Reinstalling is not an error

- **WHEN** the package is installed again, or upgraded, on a host that already
  has the user and the state directory
- **THEN** the install succeeds and neither the user nor the directory's
  ownership is changed

### Requirement: The service is not started by the package

The package SHALL NOT enable or start the service.

Without an operator-written `/etc/smalog/config.toml` the service cannot start
successfully, and the unit's restart policy would turn that into a repeating
failure. The package SHALL instead tell the operator what remains to be done:
write the configuration, then enable and start the service.

#### Scenario: Installing leaves the service stopped

- **WHEN** the package is installed
- **THEN** the service is neither running nor enabled, and nothing has been
  written to the journal by it

#### Scenario: The operator is told what is missing

- **WHEN** the package is installed
- **THEN** the install output names the configuration file to create and the
  command that starts the service afterwards

#### Scenario: An enabled service survives an upgrade

- **WHEN** an operator has enabled and started the service, and the package is
  then upgraded
- **THEN** the service is running the new binary afterwards, and is still
  enabled

### Requirement: Operator configuration is never touched

The package SHALL ship `config.example.toml` and SHALL NOT create
`/etc/smalog/config.toml`.

Because the operator's configuration is a file the package never declares, no
install, upgrade, removal or purge SHALL modify, overwrite, prompt about, or
delete it. The same SHALL hold for the secrets file the unit reads through
`EnvironmentFile`.

#### Scenario: The real configuration is left alone across an upgrade

- **WHEN** the operator has written `/etc/smalog/config.toml` and the package
  is upgraded
- **THEN** the file is unchanged, and dpkg asks nothing about it

#### Scenario: Purging keeps the operator's own files

- **WHEN** the package is purged
- **THEN** `/etc/smalog/config.toml` and the secrets file are still present

#### Scenario: A modified example is handled as a conffile

- **WHEN** the operator has edited `/etc/smalog/config.example.toml` and the
  package is upgraded with a changed example
- **THEN** dpkg applies its usual conffile handling rather than silently
  discarding the edit

### Requirement: Removal and purge leave the host consistent

On removal the package SHALL stop the service if it is running and SHALL
deregister the unit from systemd, while leaving collected data in place.

On purge the package SHALL additionally remove what it created and nothing
else: the state directory it created and the system user it added. Purging
SHALL NOT remove a database an operator configured outside `/var/lib/smalog`.

#### Scenario: Removing stops the service and keeps the data

- **WHEN** the package is removed while the service is running
- **THEN** the service is stopped, systemd no longer offers the unit, and
  `/var/lib/smalog` and its contents still exist

#### Scenario: Purging removes what the package created

- **WHEN** the package is purged
- **THEN** the state directory the package created and the `smalog` system
  user are gone

#### Scenario: Purging does not touch a database elsewhere

- **WHEN** the operator configured a database outside `/var/lib/smalog` and
  the package is purged
- **THEN** that database file is untouched

### Requirement: Packages install on the supported systems

A package SHALL install on current Raspberry Pi OS and Debian stable for its
architecture using only their default repositories, and SHALL declare the
runtime dependencies it actually needs so that a missing one is reported by
the package manager rather than by a failing service.

#### Scenario: Installing needs no extra repository

- **WHEN** the package is installed on a default Raspberry Pi OS or Debian
  stable host with `apt install ./smalog_<version>_<arch>.deb`
- **THEN** the install completes, resolving any dependency from the
  distribution's own repositories

#### Scenario: The installed binary runs

- **WHEN** `smalog --version` is run after installing
- **THEN** it reports the version the package declares
