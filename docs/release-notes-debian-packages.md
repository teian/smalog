# Debian packages, and a changed `ExecStart`

Releases now publish a Debian package for `armhf` and `arm64` — the two
Raspberry Pi architectures — beside the existing tar.gz archives and the
container image, covered by the same `SHA256SUMS`.

```bash
sudo apt install ./smalog_X.Y.Z-1_arm64.deb
```

## Breaking: the bundled unit now runs `/usr/bin/smalog`

`packaging/smalog.service` previously ran `/usr/local/bin/smalog`. Debian
policy reserves `/usr/local` for the local administrator and forbids a package
from writing there, so the packaged binary lives at `/usr/bin/smalog` and the
unit names it.

There is one unit file, shared by the package and by the manual-install
instructions, so the change affects both.

**If you installed by hand and copied the unit,** the service will fail to
start after taking the new unit, with `No such file or directory`. Pick one:

```bash
# Either move the binary to where the unit now looks,
sudo mv /usr/local/bin/smalog /usr/bin/smalog

# or keep your path and edit your copy of the unit.
sudo sed -i 's,/usr/bin/smalog,/usr/local/bin/smalog,' \
  /etc/systemd/system/smalog.service
sudo systemctl daemon-reload
```

Your configuration, database and service name are unaffected either way.

## What the package does

It installs `/usr/bin/smalog`, the systemd unit,
`/etc/smalog/config.example.toml` and the documentation; creates the `smalog`
system user and `/var/lib/smalog`; and registers the unit with systemd.

It does **not** start the service. Without `/etc/smalog/config.toml` naming
your inverters the first start cannot succeed, and `Restart=on-failure` with
`RestartSec=30` would turn that into a failure every half minute. The install
prints the two remaining steps.

It never creates, modifies, prompts about or deletes `/etc/smalog/config.toml`
or `/etc/smalog/smalog.env`. Those are yours; dpkg is not told about them, so
no upgrade or purge can touch them. Purging removes the state directory and
the system user the package created, and nothing else — a database you
configured to live elsewhere is not found or deleted.

## If you already run a hand-built install

Installing the package over one is fine, but two leftovers bite:

- A unit copied into `/etc/systemd/system/smalog.service` **overrides** the
  packaged one at `/lib/systemd/system/`. systemd keeps using your copy,
  including its old `ExecStart`. Remove it or update it.
- An old binary at `/usr/local/bin/smalog` stays where it is. Nothing uses it
  once the unit points at `/usr/bin`, but it will drift out of date.

See [operations](operations.md) for the full install, upgrade, removal and
purge behaviour.
