//! Export capability catalog.
//!
//! Planned targets are represented explicitly so configuration and UI work
//! can refer to them without exposing placeholder implementations.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportTarget {
    Csv,
    Mqtt,
    WebboxCsv,
    Solar123,
    PvOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStatus {
    Available,
    Planned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportCapability {
    pub target: ExportTarget,
    pub status: ExportStatus,
}

pub const EXPORT_CAPABILITIES: &[ExportCapability] = &[
    ExportCapability {
        target: ExportTarget::Csv,
        status: ExportStatus::Available,
    },
    ExportCapability {
        target: ExportTarget::Mqtt,
        status: ExportStatus::Available,
    },
    ExportCapability {
        target: ExportTarget::WebboxCsv,
        status: ExportStatus::Planned,
    },
    ExportCapability {
        target: ExportTarget::Solar123,
        status: ExportStatus::Planned,
    },
    ExportCapability {
        target: ExportTarget::PvOutput,
        status: ExportStatus::Planned,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn implemented_and_planned_targets_are_explicit() {
        assert_eq!(
            EXPORT_CAPABILITIES
                .iter()
                .filter(|capability| capability.status == ExportStatus::Available)
                .count(),
            2
        );
        assert!(EXPORT_CAPABILITIES.iter().any(|capability| {
            capability.target == ExportTarget::PvOutput
                && capability.status == ExportStatus::Planned
        }));
    }
}
