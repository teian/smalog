//! Shared SMA Data2+ application protocol.
//!
//! Speedwire/Ethernet and Bluetooth use different transports and outer
//! framing, but carry the same commands, logical records, archives and
//! inverter model. Bluetooth frames are normalized to the internal
//! Speedwire datagram representation before this layer decodes them.

pub mod archive;
pub mod commands;
pub mod decode;
pub mod inverter;
pub mod tags;
