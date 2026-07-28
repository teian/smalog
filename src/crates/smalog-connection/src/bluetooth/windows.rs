//! Windows Bluetooth socket — Winsock `AF_BTH` / RFCOMM.
//!
//! Written against the documented Winsock Bluetooth API (`SOCKADDR_BTH`,
//! `BTHPROTO_RFCOMM`). It has **not** been exercised on Windows hardware
//! from this build; treat it as needing a first run with debug logging.
//! `local_adapter` selection is not implemented on Windows (the default
//! radio is used).

use std::sync::Once;
use std::time::Duration;

use windows_sys::Win32::Devices::Bluetooth::{AF_BTH, BTHPROTO_RFCOMM, SOCKADDR_BTH};
use windows_sys::Win32::Networking::WinSock::{
    closesocket, connect, recv, send, setsockopt, socket, WSAGetLastError, WSAStartup,
    INVALID_SOCKET, SOCKADDR, SOCKET, SOCKET_ERROR, SOCK_STREAM, SOL_SOCKET, SO_RCVTIMEO, WSADATA,
    WSAETIMEDOUT,
};

use super::socket::BtSocket;
use crate::error::{Error, Result};

/// Initialise Winsock once per process.
fn wsa_startup() {
    static START: Once = Once::new();
    START.call_once(|| {
        // SAFETY: WSADATA is written by WSAStartup; version 2.2.
        unsafe {
            let mut data: WSADATA = std::mem::zeroed();
            let _ = WSAStartup(0x0202, &mut data);
        }
    });
}

pub struct WindowsRfcomm {
    sock: SOCKET,
}

impl BtSocket for WindowsRfcomm {
    fn connect(dest: [u8; 6], _local: Option<[u8; 6]>, timeout: Duration) -> Result<WindowsRfcomm> {
        wsa_startup();
        // BTH_ADDR is the 48-bit address in the low bits of a u64,
        // most-significant display byte first.
        let bt_addr = dest.iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
        // SAFETY: raw Winsock calls; the sockaddr outlives connect().
        unsafe {
            let s = socket(AF_BTH as i32, SOCK_STREAM, BTHPROTO_RFCOMM as i32);
            if s == INVALID_SOCKET {
                return Err(wsa_err("socket(AF_BTH)"));
            }
            let mut addr: SOCKADDR_BTH = std::mem::zeroed();
            addr.addressFamily = AF_BTH;
            addr.btAddr = bt_addr;
            addr.port = 1; // RFCOMM channel 1
            if connect(
                s,
                &addr as *const _ as *const SOCKADDR,
                std::mem::size_of::<SOCKADDR_BTH>() as i32,
            ) == SOCKET_ERROR
            {
                closesocket(s);
                return Err(wsa_err("connect(RFCOMM channel 1)"));
            }
            // SO_RCVTIMEO on Windows is a DWORD of milliseconds.
            let ms: u32 = timeout.as_millis().min(u32::MAX as u128) as u32;
            setsockopt(
                s,
                SOL_SOCKET,
                SO_RCVTIMEO,
                &ms as *const u32 as *const u8,
                std::mem::size_of::<u32>() as i32,
            );
            Ok(WindowsRfcomm { sock: s })
        }
    }

    fn send(&self, data: &[u8]) -> Result<()> {
        let mut sent = 0usize;
        while sent < data.len() {
            // SAFETY: writing `data[sent..]` from a live slice.
            let n = unsafe {
                send(
                    self.sock,
                    data[sent..].as_ptr(),
                    (data.len() - sent) as i32,
                    0,
                )
            };
            if n == SOCKET_ERROR || n <= 0 {
                return Err(wsa_err("send"));
            }
            sent += n as usize;
        }
        Ok(())
    }

    fn read_exact(&self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        let mut got = 0usize;
        while got < n {
            // SAFETY: reading into `buf[got..]` of a live, sized vec.
            let r = unsafe { recv(self.sock, buf[got..].as_mut_ptr(), (n - got) as i32, 0) };
            if r == SOCKET_ERROR {
                // SAFETY: no aliasing; reads thread-local last error.
                let code = unsafe { WSAGetLastError() };
                if code == WSAETIMEDOUT {
                    return Err(Error::Timeout);
                }
                return Err(Error::Protocol(format!("recv: WSA error {code}")));
            }
            if r == 0 {
                return Err(Error::Protocol("bluetooth peer closed connection".into()));
            }
            got += r as usize;
        }
        Ok(buf)
    }
}

impl Drop for WindowsRfcomm {
    fn drop(&mut self) {
        // SAFETY: closing our own socket handle exactly once.
        unsafe {
            closesocket(self.sock);
        }
    }
}

fn wsa_err(what: &str) -> Error {
    // SAFETY: reads the thread-local Winsock error.
    let code = unsafe { WSAGetLastError() };
    Error::Protocol(format!("{what}: WSA error {code}"))
}
