# Docker

smalog ships a multi-stage [`Dockerfile`](../Dockerfile) and an example
[`docker-compose.yml`](../docker-compose.yml). The runtime image is a
slim Debian with a single static-ish binary, running as a non-root user
(`smalog`, uid 10001).

## Building

### Multi-arch with buildx

Release images are published to both
[`fgehann/smalog`](https://hub.docker.com/r/fgehann/smalog) on Docker Hub and
[`ghcr.io/teian/smalog`](https://github.com/teian/smalog/pkgs/container/smalog)
for the same architectures as the binary releases: **386**, **amd64**,
**arm/v7** and **arm64**. Each Rust target is compiled under QEMU, so the
`ring` crypto crate and bundled SQLite compile for the target platform
with no cross-toolchain to configure:

```bash
docker buildx build \
  --platform linux/386,linux/amd64,linux/arm/v7,linux/arm64 \
  -t ghcr.io/teian/smalog:latest \
  -t fgehann/smalog:latest --push .
```

### Single-arch local build

```bash
docker build -t smalog:latest .
```

## Running with Docker Compose

The bundled compose file mounts your config read-only and persists the
SQLite database in a named volume:

```yaml
services:
  smalog:
    image: smalog:latest
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - ./config.toml:/etc/smalog/config.toml:ro
      - smalog-data:/var/lib/smalog
    environment:
      SMALOG_INV1_PASSWORD: ${SMALOG_INV1_PASSWORD:-0000}
      SMALOG_BT_PASSWORD: ${SMALOG_BT_PASSWORD:-0000}

volumes:
  smalog-data:
```

Bring it up with:

```bash
docker compose up -d
docker compose logs -f smalog
```

The compose file also includes a commented-out `postgres:16-alpine`
service if you prefer PostgreSQL over the default SQLite backend — see
[database.md](database.md).

## Networking: bridge vs host

This is the most important Docker decision, and it depends on how your
inverters are configured in [`[[inverter]]`](configuration.md#inverter):

### Fixed IPs → bridge networking (default)

If every Ethernet inverter has an `address` (a fixed IP), the default
**bridge** network works. smalog opens outbound connections to those IPs
and there is nothing special to configure. The example compose file uses
bridge networking and maps port `8080` for the status endpoint.

### Discovery by serial → host networking

If any Ethernet inverter is configured **by `serial` only** (no `address`), smalog
locates it via **multicast discovery** (`239.12.255.254:9522`). Multicast
does not cross Docker's bridge NAT, so you must run the container on the
host network:

```yaml
services:
  smalog:
    image: smalog:latest
    restart: unless-stopped
    network_mode: host          # required for multicast discovery
    volumes:
      - ./config.toml:/etc/smalog/config.toml:ro
      - smalog-data:/var/lib/smalog
    environment:
      SMALOG_INV1_PASSWORD: ${SMALOG_INV1_PASSWORD}
```

With `network_mode: host` the `ports:` mapping is ignored — the service
listens directly on the host's `service.listen` address.

### Bluetooth → host networking

The Linux Bluetooth transport opens an `AF_BLUETOOTH` RFCOMM socket
directly and therefore needs the host network namespace. Uncomment
`network_mode: host` in `docker-compose.yml` and remove the `ports:`
section:

```yaml
services:
  smalog:
    image: smalog:latest
    network_mode: host
    volumes:
      - ./config.toml:/etc/smalog/config.toml:ro
      - smalog-data:/var/lib/smalog
    environment:
      SMALOG_BT_PASSWORD: ${SMALOG_BT_PASSWORD:-0000}
```

Then power and configure the adapter on the host before starting smalog:

```bash
bluetoothctl power on
export SMALOG_BT_PASSWORD='0000'
docker compose up -d
docker compose logs -f smalog
```

Host network mode exposes `service.listen` directly on the host, so it
cannot be combined with the `ports:` mapping. smalog does not need
privileged mode, a D-Bus mount, or a `/dev` device mapping: it uses the
kernel RFCOMM socket API and does not manage the adapter. Pair, trust,
power, and troubleshoot the adapter on the host with BlueZ tools.

Host network mode cannot resolve other Compose services by name. If
Bluetooth is combined with PostgreSQL, configure a database address
reachable from the host namespace instead of the optional `db` service
hostname.

See [bluetooth.md](bluetooth.md) for inverter configuration and
[operations.md](operations.md) for native/systemd deployment.

## Volumes

- `/var/lib/smalog` holds the SQLite database and is declared as a
  `VOLUME` in the image. Mount a named volume or host path here so the
  database survives container recreation.
- `/etc/smalog/config.toml` is the config path baked into the image's
  entrypoint. Mount your file there (read-only is fine).

## Healthcheck

The image defines a `HEALTHCHECK` that runs the
[`healthcheck`](operations.md#healthcheck) subcommand every 60s:

```dockerfile
HEALTHCHECK --interval=60s --timeout=5s --start-period=20s --retries=3 \
    CMD ["/usr/local/bin/smalog", "--config", "/etc/smalog/config.toml", "healthcheck"]
```

This probes the service's own `/healthz` endpoint, so
[`service.listen`](configuration.md#service) **must be set** in your
config (e.g. `0.0.0.0:8080`) or the healthcheck cannot run. The container
`EXPOSE`s `8080`.

## Injecting secrets

Reference secrets as `${VAR}` in `config.toml` and provide the values
through the container environment — an `.env` file next to the compose
file, or your orchestrator's secret store — rather than inline literals:

```dotenv
# .env
SMALOG_INV1_PASSWORD=xxxxxxxx
SMALOG_MQTT_PASSWORD=xxxxxxxx
```

Remember that an unset referenced variable is a startup error, even for
disabled sections — see [Secrets](configuration.md#secrets).

See also: [configuration.md](configuration.md) ·
[operations.md](operations.md) · [database.md](database.md).
