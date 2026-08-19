//! smalog — SMA inverter logger.
//!
//! Inspired by SBFspot (https://github.com/SBFspot/SBFspot), but independently
//! structured rather than a 1:1 port. Licensed under the EUPL-1.2.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Parser, Subcommand, ValueEnum};
use std::sync::Arc;

use tracing::error;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use smalog::applog::{CaptureLayer, LogBuffer};
use smalog::config::{Config, InverterConfig, LogFormat};
use smalog::diagnostics::WriteQueue;
use smalog::migrate::{self, MigrateOptions, MigrationMode, PvOutputStateMode};
use smalog::service::Service;
use smalog::storage::{DailyYieldStatus, Db};
use smalog_connection::{
    BluetoothConnection, ClockMode, Collector, Connection, PollOptions, SpeedwireConnection,
    SyncOutcome,
};

#[derive(Parser)]
#[command(name = "smalog", version, about = "smalog — SMA inverter logger")]
struct Cli {
    /// Path to the TOML configuration file.
    #[arg(short, long, default_value = "/etc/smalog/config.toml", global = true)]
    config: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service (default).
    Run,
    /// Run a single poll cycle, then exit.
    Once,
    /// Scan the network for SMA devices and print them.
    Discover,
    /// Test configured Bluetooth inverters without writing exports.
    TestBluetooth {
        /// Query and print all available spot values instead of stopping
        /// after the first successful representative query.
        #[arg(long)]
        all: bool,
        /// Append every raw Bluetooth frame to this file, overriding
        /// `capture_file` from the configuration.
        #[arg(long, value_name = "FILE")]
        capture: Option<std::path::PathBuf>,
    },
    /// Validate the configuration file and exit.
    CheckConfig,
    /// Rebuild canonical daily yields for one inverter and a half-open local-date range.
    RebuildDailyYields {
        /// Inverter serial number.
        #[arg(long)]
        inverter: u32,
        /// Inclusive local start date (YYYY-MM-DD).
        #[arg(long)]
        start: NaiveDate,
        /// Exclusive local end date (YYYY-MM-DD).
        #[arg(long)]
        end: NaiveDate,
    },
    /// Preflight or migrate an SBFspot schema-v1 SQLite database.
    MigrateSbfspot {
        /// Read-only SBFspot schema-version-1 SQLite source URL.
        #[arg(long)]
        source: String,
        /// Distinct smalog schema-v1 SQLite or PostgreSQL target URL.
        #[arg(long)]
        target: String,
        /// Required IANA timezone used for legacy local-date conversion.
        #[arg(long)]
        timezone: String,
        /// Run complete read-only preflight and do not migrate data.
        #[arg(long, visible_alias = "preflight", conflicts_with = "verify_only")]
        dry_run: bool,
        /// Resume an interrupted migration of this same source.
        #[arg(long, conflicts_with = "verify_only")]
        resume: bool,
        /// Verify an existing migration without writing either database.
        #[arg(long)]
        verify_only: bool,
        /// Rebuild the optional daily-statistics cache after raw migration.
        #[arg(long)]
        daily_statistics: bool,
        /// Import legacy DayData.PVoutput state into the optional operational table.
        #[arg(long, value_enum)]
        pvoutput_state: Option<PvOutputStateArg>,
    },
    /// Probe the running service's /healthz endpoint (Docker healthcheck).
    Healthcheck,
    /// Set the inverter clock to this host's time, then exit. Bluetooth
    /// only (SBFspot `-settime`): reads the inverter clock, writes the host
    /// time and verifies it.
    SetTime,
    /// Blind clock-set fallback for inverters where `set-time` fails
    /// (Bluetooth only; SBFspot `-settime2`): writes without a read-back.
    SetTime2,
}

#[derive(Clone, Copy, ValueEnum)]
enum PvOutputStateArg {
    /// Interpret only legacy NULL/0/1 flags; other values fail migration.
    LegacyFlag,
}

/// Install the global subscriber: the configured stdout format, plus the
/// in-memory capture layer when `capture` is given.
///
/// Both layers sit behind the same `EnvFilter`, so `[log] level` keeps
/// controlling stdout and the captured ring alike, and the ring can never
/// hold a record stdout did not also receive.
fn init_logging(cfg: Option<&Config>, capture: Option<Arc<LogBuffer>>) {
    let (level, format) = cfg
        .map(|c| (c.log.level.clone(), c.log.format))
        .unwrap_or_else(|| ("info".into(), LogFormat::Text));
    let filter = EnvFilter::try_new(&level)
        .or_else(|_| EnvFilter::try_new("info"))
        .expect("valid default filter");
    // Boxing lets the JSON and text formatters share one layer type.
    let output = match format {
        LogFormat::Json => tracing_subscriber::fmt::layer().json().boxed(),
        LogFormat::Text => tracing_subscriber::fmt::layer().boxed(),
    };
    tracing_subscriber::registry()
        .with(filter)
        .with(output)
        .with(capture.map(CaptureLayer::new))
        .init();
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Run);

    match command {
        Command::Discover => {
            init_logging(None, None);
            // Enumerate every transport represented in the configuration;
            // without a readable config, fall back to Ethernet discovery.
            let config = Config::load(&cli.config).ok();
            let result = match config {
                Some(config) => discover_configured(&config).await,
                None => discover_ethernet().await,
            };
            match result {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    error!(error = %e, "discovery failed");
                    ExitCode::FAILURE
                }
            }
        }
        Command::CheckConfig => match Config::load(&cli.config) {
            Ok(_) => {
                println!("{}: OK", cli.config.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}: {e}", cli.config.display());
                ExitCode::FAILURE
            }
        },
        Command::Healthcheck => {
            // Resolve the listen address from config; probe /healthz.
            let addr = match Config::load(&cli.config) {
                Ok(c) => c.service.listen,
                Err(e) => {
                    eprintln!("config error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let Some(addr) = addr else {
                eprintln!("service.listen not configured — cannot health-check");
                return ExitCode::FAILURE;
            };
            match healthcheck(addr).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("healthcheck failed: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::TestBluetooth { all, capture } => {
            test_bluetooth(&cli.config, all, capture.as_deref()).await
        }
        Command::MigrateSbfspot {
            source,
            target,
            timezone,
            dry_run,
            resume,
            verify_only,
            daily_statistics,
            pvoutput_state,
        } => {
            let mode = if dry_run {
                MigrationMode::Preflight
            } else if verify_only {
                MigrationMode::VerifyOnly
            } else if resume {
                MigrationMode::Resume
            } else {
                MigrationMode::Execute
            };
            let options = MigrateOptions {
                source,
                target,
                timezone,
                mode,
                daily_statistics,
                pvoutput_state: pvoutput_state.map(|state| match state {
                    PvOutputStateArg::LegacyFlag => PvOutputStateMode::LegacyFlag,
                }),
            };
            if mode == MigrationMode::Preflight {
                match migrate::preflight(&options).await {
                    Ok(report) => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&report)
                                .expect("serialize preflight report")
                        );
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        eprintln!("SBFspot migration preflight failed: {error}");
                        ExitCode::FAILURE
                    }
                }
            } else if mode == MigrationMode::VerifyOnly {
                match migrate::verify(&options).await {
                    Ok(report) => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&report)
                                .expect("serialize verification report")
                        );
                        if report.passed {
                            ExitCode::SUCCESS
                        } else {
                            ExitCode::FAILURE
                        }
                    }
                    Err(error) => {
                        eprintln!("SBFspot migration verification failed: {error}");
                        ExitCode::FAILURE
                    }
                }
            } else {
                match migrate::migrate(&options).await {
                    Ok(report) => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&report)
                                .expect("serialize migration report")
                        );
                        ExitCode::SUCCESS
                    }
                    Err(error) => {
                        let message = error.to_string();
                        match message.find('{') {
                            Some(json_start)
                                if serde_json::from_str::<serde_json::Value>(
                                    &message[json_start..],
                                )
                                .is_ok() =>
                            {
                                eprintln!("{}", &message[json_start..]);
                            }
                            _ => eprintln!("SBFspot migration failed: {message}"),
                        }
                        ExitCode::FAILURE
                    }
                }
            }
        }
        Command::RebuildDailyYields {
            inverter,
            start,
            end,
        } => rebuild_daily_yields(&cli.config, inverter, start, end).await,
        Command::SetTime => set_inverter_time(&cli.config, ClockMode::Force).await,
        Command::SetTime2 => set_inverter_time(&cli.config, ClockMode::Blind).await,
        Command::Run | Command::Once => {
            let config = match Config::load(&cli.config) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("config error: {e}");
                    return ExitCode::FAILURE;
                }
            };
            // The queue feeds the persisted transmission ring; the log ring
            // is process memory and is handed to the capture layer directly,
            // so a log call never reaches a database.
            let queue = WriteQueue::new(smalog::diagnostics::QUEUE_CAPACITY);
            let log_buffer = LogBuffer::new(
                config.service.application_log_retention_hours,
                config.service.application_log_max_entries,
            );
            let capture = log_buffer.enabled().then(|| log_buffer.clone());
            init_logging(Some(&config), capture);
            tracing::info!(version = smalog::VERSION, "smalog — SMA inverter logger");
            let once = matches!(command, Command::Once);
            match Service::new(config, queue, log_buffer).await {
                Ok(mut service) => {
                    let res = if once {
                        service.tick_once().await;
                        Ok(())
                    } else {
                        service.run().await
                    };
                    match res {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(e) => {
                            error!(error = %e, "service failed");
                            ExitCode::FAILURE
                        }
                    }
                }
                Err(e) => {
                    error!(error = %e, "startup failed");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

async fn rebuild_daily_yields(
    config_path: &std::path::Path,
    inverter: u32,
    start: NaiveDate,
    end: NaiveDate,
) -> ExitCode {
    let config = match Config::load(config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("config error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let timezone = match config.timezone() {
        Ok(timezone) => timezone,
        Err(error) => {
            eprintln!("config error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let db = match Db::connect(&config.database.url, timezone).await {
        Ok(db) => db,
        Err(error) => {
            eprintln!("database error: {error}");
            return ExitCode::FAILURE;
        }
    };
    match db
        .rebuild_daily_yields(inverter, start, end, chrono::Utc::now().timestamp())
        .await
    {
        Ok(results) => {
            let mut rebuilt = 0;
            let mut missing = 0;
            let mut incomplete = 0;
            for result in results {
                let label = match result.status {
                    DailyYieldStatus::Rebuilt => {
                        rebuilt += 1;
                        "rebuilt"
                    }
                    DailyYieldStatus::Missing => {
                        missing += 1;
                        "missing"
                    }
                    DailyYieldStatus::Incomplete => {
                        incomplete += 1;
                        "incomplete"
                    }
                };
                println!(
                    "{}: {label} total_wh={} daily_wh={}",
                    result.date,
                    result
                        .total_energy_wh
                        .map_or_else(|| "-".into(), |value| value.to_string()),
                    result
                        .daily_energy_wh
                        .map_or_else(|| "-".into(), |value| value.to_string())
                );
            }
            println!(
                "daily-yield rebuild complete: rebuilt={rebuilt} missing={missing} \
                 incomplete={incomplete}"
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("daily-yield rebuild failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn test_bluetooth(
    config_path: &std::path::Path,
    all: bool,
    capture: Option<&std::path::Path>,
) -> ExitCode {
    let config = match Config::load(config_path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("config error: {error}");
            return ExitCode::FAILURE;
        }
    };
    init_logging(Some(&config), None);
    let tz = match config.timezone() {
        Ok(tz) => tz,
        Err(error) => {
            eprintln!("config error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let bluetooth = config
        .inverters
        .iter()
        .filter(|inverter| !inverter.is_ethernet())
        .collect::<Vec<_>>();
    if bluetooth.is_empty() {
        eprintln!("no Bluetooth inverter configured");
        return ExitCode::FAILURE;
    }

    let options = PollOptions {
        calc_missing_spot: config.service.calc_missing_spot,
        poll_consumption: config.service.poll_consumption,
    };
    let mut failed = false;
    let mut groups: Vec<(smalog_connection::BluetoothParams, Vec<&InverterConfig>)> = Vec::new();
    for configured in bluetooth {
        let params = match configured.to_bluetooth_params(tz) {
            Ok(mut params) => {
                if let Some(path) = capture {
                    params.capture = Some(path.to_path_buf());
                }
                params
            }
            Err(error) => {
                eprintln!("{}: config error: {error}", configured.name);
                failed = true;
                continue;
            }
        };
        if let Some((_, configured_inverters)) = groups
            .iter_mut()
            .find(|(existing, _)| existing.address == params.address)
        {
            configured_inverters.push(configured);
        } else {
            groups.push((params, vec![configured]));
        }
    }

    for (params, configured_inverters) in groups {
        if configured_inverters.iter().skip(1).any(|configured| {
            configured.password != configured_inverters[0].password
                || configured.user_group != configured_inverters[0].user_group
        }) {
            eprintln!(
                "{}: Bluetooth inverters sharing one address must use the same password \
                 and user_group",
                format_bluetooth_address(params.address)
            );
            failed = true;
            continue;
        }
        let connector: BluetoothConnection = BluetoothConnection::new(params);
        let mut collector = Collector::new(Box::new(connector), tz, options);
        let result = if all {
            collector.probe_all().await
        } else {
            collector.probe().await
        };
        match result {
            Ok((inverters, received_frames)) if received_frames > 0 => {
                let configured = configured_inverters[0];
                for inverter in inverters {
                    if all {
                        print_all_spot_values(configured.name.as_str(), received_frames, &inverter);
                    } else {
                        println!(
                            "{} (serial {}): OK — {} response frames; model {}; firmware {}; \
                             AC power {} W; today {} Wh; total {} Wh",
                            configured.name,
                            inverter.serial,
                            received_frames,
                            display_or_unknown(&inverter.device_type),
                            display_or_unknown(&inverter.sw_version),
                            inverter.total_pac,
                            inverter.e_today,
                            inverter.e_total,
                        );
                    }
                }
            }
            Ok((_inverters, _)) => {
                for configured in configured_inverters {
                    eprintln!(
                        "{}: login succeeded, but no spot-data responses were received",
                        configured.name
                    );
                }
                failed = true;
            }
            Err(error) => {
                for configured in configured_inverters {
                    eprintln!("{}: FAILED: {error}", configured.name);
                }
                failed = true;
            }
        }
    }

    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn print_all_spot_values(
    name: &str,
    received_frames: usize,
    inverter: &smalog_connection::smadata2::inverter::InverterData,
) {
    println!(
        "{name} (serial {}): OK — {received_frames} response frames",
        inverter.serial
    );
    println!("  address: {}", inverter.ip);
    println!("  SUSyID: {}", inverter.susy_id);
    println!(
        "  device name: {}",
        display_or_unknown(inverter.display_name())
    );
    println!("  model: {}", display_or_unknown(&inverter.device_type));
    println!(
        "  device class: {} ({})",
        display_or_unknown(&inverter.device_class),
        inverter.dev_class
    );
    println!("  firmware: {}", display_or_unknown(&inverter.sw_version));
    println!("  inverter timestamp: {}", inverter.inverter_datetime);
    println!(
        "  wake-up / sleep: {} / {}",
        inverter.wakeup_time, inverter.sleep_time
    );
    println!(
        "  energy today / total: {} Wh / {} Wh",
        inverter.e_today, inverter.e_total
    );
    println!(
        "  operation / feed-in time: {} s / {} s",
        inverter.operation_time, inverter.feed_in_time
    );
    for (tracker, mppt) in &inverter.mpp {
        println!(
            "  MPPT {tracker}: {} W, {}, {}",
            mppt.pdc,
            format_scaled(mppt.udc, 100.0, "V"),
            format_scaled(mppt.idc, 1000.0, "A")
        );
    }
    println!(
        "  AC power total / L1 / L2 / L3: {} / {} / {} / {} W",
        inverter.total_pac, inverter.pac1, inverter.pac2, inverter.pac3
    );
    println!(
        "  AC voltage L1 / L2 / L3: {} / {} / {}",
        format_scaled(inverter.uac1, 100.0, "V"),
        format_scaled(inverter.uac2, 100.0, "V"),
        format_scaled(inverter.uac3, 100.0, "V")
    );
    println!(
        "  AC current L1 / L2 / L3: {} / {} / {}",
        format_scaled(inverter.iac1, 1000.0, "A"),
        format_scaled(inverter.iac2, 1000.0, "A"),
        format_scaled(inverter.iac3, 1000.0, "A")
    );
    println!(
        "  grid frequency: {}",
        format_scaled(inverter.grid_freq, 100.0, "Hz")
    );
    println!(
        "  status / grid relay: {} / {}",
        inverter.device_status, inverter.grid_relay_status
    );
    println!(
        "  temperature: {}",
        format_scaled(inverter.temperature, 100.0, "°C")
    );
    println!(
        "  grid metering out / in: {} / {} W",
        inverter.metering_grid_ms_tot_w_out, inverter.metering_grid_ms_tot_w_in
    );
    println!(
        "  consumption energy / power: {} Wh / {} W (available: {})",
        inverter.csmp_tot_wh_in, inverter.csmp_tot_w_in, inverter.has_consumption
    );
    println!(
        "  battery: available {}; charge {} %; cycles {}; Ah in/out {} / {}; \
         temperature {} °C/10; voltage {} V/100; current {} mA",
        inverter.has_battery,
        inverter.bat_cha_stt,
        inverter.bat_diag_capac_thrp_cnt,
        inverter.bat_diag_tot_ah_in,
        inverter.bat_diag_tot_ah_out,
        inverter.bat_tmp_val,
        inverter.bat_vol,
        inverter.bat_amp
    );
    println!(
        "  derived DC / AC / efficiency: {} W / {} W / {:.2} %",
        inverter.cal_pdc_tot, inverter.cal_pac_tot, inverter.cal_efficiency
    );
}

fn format_scaled(value: i32, divisor: f64, unit: &str) -> String {
    if value == smalog_connection::smadata2::inverter::NAN_S32 {
        "unknown".to_string()
    } else {
        format!("{:.2} {unit}", f64::from(value) / divisor)
    }
}

fn format_bluetooth_address(address: [u8; 6]) -> String {
    address
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn display_or_unknown(value: &str) -> &str {
    if value.is_empty() {
        "unknown"
    } else {
        value
    }
}

async fn healthcheck(addr: std::net::SocketAddr) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // Connect to loopback rather than a bind-all address like 0.0.0.0.
    let target = if addr.ip().is_unspecified() {
        std::net::SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), addr.port())
    } else {
        addr
    };
    let mut sock = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(target),
    )
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))??;
    sock.write_all(
        format!("GET /healthz HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await?;
    let mut resp = String::new();
    sock.read_to_string(&mut resp).await?;
    if resp.starts_with("HTTP/1.1 200") {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "unexpected response: {}",
            resp.lines().next().unwrap_or("")
        )))
    }
}

/// Set the inverter clock (SBFspot SetPlantTime). Bluetooth only —
/// `ClockMode::Force` is `-settime` (read/verify), `ClockMode::Blind` is
/// `-settime2`.
async fn set_inverter_time(config_path: &std::path::Path, mode: ClockMode) -> ExitCode {
    let config = match Config::load(config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    init_logging(Some(&config), None);
    let bluetooth: Vec<_> = config
        .inverters
        .iter()
        .filter(|inverter| !inverter.is_ethernet())
        .collect();
    if bluetooth.is_empty() {
        eprintln!(
            "clock-sync is Bluetooth only (SMA Speedwire devices get their \
             time from the network); configure a Bluetooth inverter to use it"
        );
        return ExitCode::FAILURE;
    }
    let tz = match config.timezone() {
        Ok(tz) => tz,
        Err(e) => {
            eprintln!("config error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut failed = false;
    for inverter in bluetooth {
        let params = match inverter.to_bluetooth_params(tz) {
            Ok(params) => params,
            Err(e) => {
                eprintln!("config error for {:?}: {e}", inverter.name);
                failed = true;
                continue;
            }
        };
        let mut connector: smalog_connection::BluetoothConnection =
            smalog_connection::BluetoothConnection::new(params);
        let outcome = async {
            connector.begin().await?;
            connector.login_all().await?;
            let outcome = connector.set_clock(mode).await?;
            connector.end().await;
            Ok::<_, smalog_connection::Error>(outcome)
        }
        .await;

        match outcome {
            Ok(SyncOutcome::Set) => println!("{}: inverter clock set", inverter.name),
            Ok(SyncOutcome::Skipped(reason)) => {
                println!("{}: clock unchanged: {reason}", inverter.name)
            }
            Ok(SyncOutcome::VerifyFailed { drift }) => {
                eprintln!(
                    "{}: clock written but not confirmed (drift {drift}s); try set-time2",
                    inverter.name
                );
                failed = true;
            }
            Ok(SyncOutcome::Unsupported) => {
                eprintln!("{}: clock-sync is not supported", inverter.name);
                failed = true;
            }
            Err(e) => {
                error!(inverter = %inverter.name, error = %e, "set-time failed");
                failed = true;
            }
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn discover_configured(config: &Config) -> smalog::Result<()> {
    let tz = config.timezone()?;
    let mut bluetooth_addresses = Vec::new();
    for inverter in &config.inverters {
        if inverter.is_ethernet() {
            continue;
        }
        let params = inverter.to_bluetooth_params(tz)?;
        if bluetooth_addresses.contains(&params.address) {
            continue;
        }
        bluetooth_addresses.push(params.address);
        discover_bluetooth(inverter, tz).await?;
    }
    if config.inverters.iter().any(InverterConfig::is_ethernet) {
        discover_ethernet().await?;
    }
    Ok(())
}

/// Enumerate inverters over a configured Bluetooth link.
async fn discover_bluetooth(inverter: &InverterConfig, tz: chrono_tz::Tz) -> smalog::Result<()> {
    let params = inverter.to_bluetooth_params(tz)?;
    // The RFCOMM socket is blocking; run the handshake off the executor.
    let devices =
        tokio::task::spawn_blocking(move || smalog_connection::bluetooth::enumerate(&params))
            .await
            .map_err(|e| smalog::Error::Protocol(e.to_string()))??;
    if devices.is_empty() {
        println!("No inverters found on the Bluetooth network.");
        return Ok(());
    }
    println!("{:<20} {:>7} {:>12}", "BT address", "SUSyID", "Serial");
    for device in devices {
        println!(
            "{:<20} {:>7} {:>12}",
            device.address, device.susy_id, device.serial
        );
    }
    Ok(())
}

async fn discover_ethernet() -> smalog::Result<()> {
    let devices = SpeedwireConnection::discover().await?;
    if devices.is_empty() {
        println!("No SMA devices found (multicast 239.12.255.254:9522).");
        return Ok(());
    }
    println!("{:<16} {:>7} {:>12}", "IP", "SUSyID", "Serial");
    for device in devices {
        println!(
            "{:<16} {:>7} {:>12}",
            device.address, device.susy_id, device.serial
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use smalog::domain::{InverterIdentity, InverterMeasurement, Transport, UnixSeconds};

    #[test]
    fn parses_full_bluetooth_test_option() {
        let cli =
            Cli::try_parse_from(["smalog", "test-bluetooth", "--all"]).expect("valid command");
        assert!(matches!(
            cli.command,
            Some(Command::TestBluetooth { all: true, .. })
        ));
    }

    #[test]
    fn bluetooth_test_is_quick_by_default() {
        let cli = Cli::try_parse_from(["smalog", "test-bluetooth"]).expect("valid command");
        assert!(matches!(
            cli.command,
            Some(Command::TestBluetooth { all: false, .. })
        ));
    }

    #[test]
    fn rebuild_command_parses_half_open_dates_and_rejects_invalid_dates() {
        let cli = Cli::try_parse_from([
            "smalog",
            "rebuild-daily-yields",
            "--inverter",
            "42",
            "--start",
            "2024-03-30",
            "--end",
            "2024-04-02",
        ])
        .expect("valid rebuild command");
        assert!(matches!(
            cli.command,
            Some(Command::RebuildDailyYields {
                inverter: 42,
                start,
                end,
            }) if start == NaiveDate::from_ymd_opt(2024, 3, 30).unwrap()
                && end == NaiveDate::from_ymd_opt(2024, 4, 2).unwrap()
        ));
        assert!(Cli::try_parse_from([
            "smalog",
            "rebuild-daily-yields",
            "--inverter",
            "42",
            "--start",
            "2024-02-30",
            "--end",
            "2024-03-01",
        ])
        .is_err());
    }

    #[test]
    fn migrate_sbfspot_help_and_all_arguments_are_exposed() {
        use clap::CommandFactory;

        let help = Cli::command()
            .find_subcommand_mut("migrate-sbfspot")
            .expect("migrate-sbfspot command")
            .render_long_help()
            .to_string();
        for option in [
            "--source",
            "--target",
            "--timezone",
            "--dry-run",
            "--preflight",
            "--resume",
            "--verify-only",
            "--daily-statistics",
            "--pvoutput-state",
        ] {
            assert!(help.contains(option), "missing {option} from help:\n{help}");
        }

        let cli = Cli::try_parse_from([
            "smalog",
            "migrate-sbfspot",
            "--source",
            "sqlite:///source.db",
            "--target",
            "postgresql://localhost/smalog",
            "--timezone",
            "Europe/Berlin",
            "--resume",
            "--daily-statistics",
            "--pvoutput-state",
            "legacy-flag",
        ])
        .expect("valid migration command");
        assert!(matches!(
            cli.command,
            Some(Command::MigrateSbfspot {
                source,
                target,
                timezone,
                dry_run: false,
                resume: true,
                verify_only: false,
                daily_statistics: true,
                pvoutput_state: Some(PvOutputStateArg::LegacyFlag),
            }) if source == "sqlite:///source.db"
                && target == "postgresql://localhost/smalog"
                && timezone == "Europe/Berlin"
        ));
    }

    #[test]
    fn migrate_sbfspot_requires_endpoints_and_timezone_and_rejects_conflicting_modes() {
        assert!(Cli::try_parse_from(["smalog", "migrate-sbfspot", "--dry-run"]).is_err());
        assert!(Cli::try_parse_from([
            "smalog",
            "migrate-sbfspot",
            "--source",
            "sqlite:///source.db",
            "--target",
            "sqlite:///target.db",
            "--timezone",
            "UTC",
            "--dry-run",
            "--verify-only",
        ])
        .is_err());
    }

    #[tokio::test]
    async fn rebuild_command_executes_the_shared_rollup_path_and_is_repeatable() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("smalog.db");
        let database_url = format!("sqlite://{}", db_path.display());
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[service]\ntimezone = \"UTC\"\n\
                 [plant]\nlatitude = 0\nlongitude = 0\n\
                 [database]\nurl = {database_url:?}\n\
                 [[inverter]]\nname = \"Test\"\npassword = \"0000\"\n\
                 communication = \"ethernet\"\nserial = 42\n"
            ),
        )
        .unwrap();
        let db = Db::connect(&database_url, chrono_tz::Tz::UTC)
            .await
            .unwrap();
        let identity = InverterIdentity {
            serial_number: 42,
            susy_id: Some(125),
            configured_name: None,
            device_name: None,
            model: None,
            firmware_version: None,
            transport: Some(Transport::Ethernet),
        };
        let measurement = InverterMeasurement {
            measured_at: UnixSeconds::new(1),
            ac_power: [None; 3],
            ac_current: [None; 3],
            ac_voltage: [None; 3],
            grid_frequency: None,
            grid_import_power: None,
            grid_export_power: None,
            energy_today: None,
            energy_total: None,
            operating_time: None,
            feed_in_time: None,
            device_status: None,
            grid_relay_status: None,
            temperature: None,
            bluetooth_signal: None,
            mppts: Vec::new(),
            battery: None,
        };
        db.write_poll(&identity, &measurement).await.unwrap();
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!(),
        };
        let inverter_id: i64 =
            sqlx::query_scalar("SELECT inverter_id FROM inverters WHERE serial_number=42")
                .fetch_one(pool)
                .await
                .unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let start = chrono_tz::Tz::UTC
            .from_local_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
            .unwrap()
            .timestamp();
        for (measured_at, total_energy_wh) in [(start - 300, 1_000_i64), (start + 300, 1_250)] {
            sqlx::query(
                "INSERT INTO inverter_energy_samples
                 (inverter_id,measured_at,total_energy_wh,power_w)
                 VALUES ($1,$2,$3,0)",
            )
            .bind(inverter_id)
            .bind(measured_at)
            .bind(total_energy_wh)
            .execute(pool)
            .await
            .unwrap();
        }
        drop(db);

        let end = date.succ_opt().unwrap();
        assert_eq!(
            rebuild_daily_yields(&config_path, 42, date, end).await,
            ExitCode::SUCCESS
        );
        assert_eq!(
            rebuild_daily_yields(&config_path, 42, date, end).await,
            ExitCode::SUCCESS
        );
        let db = Db::connect(&database_url, chrono_tz::Tz::UTC)
            .await
            .unwrap();
        let pool = match &db {
            Db::Sqlite { pool, .. } => pool,
            Db::Postgres { .. } => unreachable!(),
        };
        assert_eq!(
            sqlx::query_scalar::<_, i64>(
                "SELECT daily_energy_wh FROM inverter_daily_yields WHERE yield_date='2024-01-01'",
            )
            .fetch_one(pool)
            .await
            .unwrap(),
            250
        );
        assert_eq!(
            rebuild_daily_yields(&config_path, 999, date, end).await,
            ExitCode::FAILURE
        );
        assert_eq!(
            rebuild_daily_yields(&config_path, 42, end, date).await,
            ExitCode::FAILURE
        );
    }
}
