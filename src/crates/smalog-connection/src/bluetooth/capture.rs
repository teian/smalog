//! Optional raw-frame capture for field debugging.
//!
//! When a capture path is configured, every Bluetooth L1 frame the client
//! sends or receives is appended as one hex line. That is the byte stream
//! *before* de-escaping and reassembly, so a capture shows fragmentation,
//! escaping and FCS problems that the decoded logs cannot.
//!
//! Capturing never fails a session: an unwritable file is reported once
//! and then ignored.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tracing::warn;

use crate::error::{Error, Result};

/// Direction of a captured frame, as written to the capture file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Host → inverter.
    Tx,
    /// Inverter → host.
    Rx,
}

impl Direction {
    fn tag(self) -> &'static str {
        match self {
            Direction::Tx => "TX",
            Direction::Rx => "RX",
        }
    }
}

/// An open capture file. Share it with `Arc` — recording takes `&self`.
pub struct Capture {
    file: Mutex<std::fs::File>,
    /// Set after the first write error, so a broken capture is reported
    /// once instead of on every frame.
    broken: AtomicBool,
}

impl Capture {
    /// Open (or create) `path` for appending and write a session header.
    /// `peer` is the inverter MAC, for captures shared by several links.
    pub fn create(path: &Path, peer: &str) -> Result<Capture> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| {
                Error::Protocol(format!(
                    "cannot open Bluetooth capture {}: {e}",
                    path.display()
                ))
            })?;
        let capture = Capture {
            file: Mutex::new(file),
            broken: AtomicBool::new(false),
        };
        capture.write_line(&format!(
            "# smalog bluetooth capture — peer {peer}, opened {}",
            timestamp()
        ));
        Ok(capture)
    }

    /// Append one frame as `<timestamp> <TX|RX> <len> <hex>`.
    pub fn record(&self, direction: Direction, frame: &[u8]) {
        let mut line = String::with_capacity(frame.len() * 2 + 48);
        line.push_str(&timestamp());
        line.push(' ');
        line.push_str(direction.tag());
        line.push(' ');
        line.push_str(&frame.len().to_string());
        line.push(' ');
        for b in frame {
            line.push_str(&format!("{b:02X}"));
        }
        self.write_line(&line);
    }

    fn write_line(&self, line: &str) {
        if self.broken.load(Ordering::Relaxed) {
            return;
        }
        let mut file = match self.file.lock() {
            Ok(f) => f,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(e) = writeln!(file, "{line}") {
            self.broken.store(true, Ordering::Relaxed);
            warn!(error = %e, "bluetooth capture disabled after a write error");
        }
    }
}

fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_both_directions_as_hex_lines() {
        let path = std::env::temp_dir().join(format!(
            "smalog-capture-{}-{}.log",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&path);

        let capture = Capture::create(&path, "00:80:25:2E:45:D6").expect("capture opens");
        capture.record(Direction::Tx, &[0x7E, 0x1F, 0x00]);
        capture.record(Direction::Rx, &[0xAB]);
        drop(capture);

        let written = std::fs::read_to_string(&path).expect("capture is readable");
        let lines: Vec<_> = written.lines().collect();
        assert!(lines[0].starts_with("# smalog bluetooth capture"));
        assert!(lines[1].contains(" TX 3 7E1F00"), "{}", lines[1]);
        assert!(lines[2].contains(" RX 1 AB"), "{}", lines[2]);

        let _ = std::fs::remove_file(&path);
    }
}
