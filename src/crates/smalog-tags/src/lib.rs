//! Localized SMA tag definitions (status codes, device types, event texts).
//!
//! Adapted from SBFspot's tag lists into structured UTF-8 JSON files embedded
//! at compile time with `include_str!`. Every tag has the explicit fields
//! `id`, `short`, `unit_id` and `long`. The active locale (SBFspot `Locale`) is
//! chosen once at startup via [`set_locale`]; it selects which table [`desc`]
//! reads, so event text and CSV headers come out translated. Each locale's
//! table is parsed once, lazily, and cached for the process lifetime.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

use serde::Deserialize;

/// SMA UI languages shipped with SBFspot (one TagList file each).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// English (United States).
    EnUs = 0,
    /// German (Germany).
    DeDe = 1,
    /// Spanish (Spain).
    EsEs = 2,
    /// French (France).
    FrFr = 3,
    /// Italian (Italy).
    ItIt = 4,
    /// Dutch (Netherlands).
    NlNl = 5,
}

impl Locale {
    /// The embedded UTF-8 JSON tag file for this locale.
    fn file(self) -> &'static str {
        match self {
            Locale::EnUs => include_str!("data/tags-en-US.json"),
            Locale::DeDe => include_str!("data/tags-de-DE.json"),
            Locale::EsEs => include_str!("data/tags-es-ES.json"),
            Locale::FrFr => include_str!("data/tags-fr-FR.json"),
            Locale::ItIt => include_str!("data/tags-it-IT.json"),
            Locale::NlNl => include_str!("data/tags-nl-NL.json"),
        }
    }

    fn code(self) -> &'static str {
        match self {
            Locale::EnUs => "en-US",
            Locale::DeDe => "de-DE",
            Locale::EsEs => "es-ES",
            Locale::FrFr => "fr-FR",
            Locale::ItIt => "it-IT",
            Locale::NlNl => "nl-NL",
        }
    }

    fn from_index(i: u8) -> Locale {
        match i {
            1 => Locale::DeDe,
            2 => Locale::EsEs,
            3 => Locale::FrFr,
            4 => Locale::ItIt,
            5 => Locale::NlNl,
            _ => Locale::EnUs,
        }
    }

    /// Parse a locale from config: accepts full SBFspot names
    /// ("en-US", "de-DE", …) or the bare language ("en", "de", …),
    /// case-insensitively.
    pub fn parse(s: &str) -> Option<Locale> {
        let lang: String = s
            .trim()
            .chars()
            .take(2)
            .flat_map(|c| c.to_lowercase())
            .collect();
        match lang.as_str() {
            "en" => Some(Locale::EnUs),
            "de" => Some(Locale::DeDe),
            "es" => Some(Locale::EsEs),
            "fr" => Some(Locale::FrFr),
            "it" => Some(Locale::ItIt),
            "nl" => Some(Locale::NlNl),
            _ => None,
        }
    }
}

/// A tag's short label and long description text.
#[derive(Debug, Clone)]
pub struct Tag {
    /// Short label (e.g. "Alm").
    pub short: String,
    /// Referenced unit tag, or zero when the tag has no unit reference.
    pub unit_id: u32,
    /// Long description (e.g. "Fault").
    pub long: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TagFile {
    locale: String,
    source: TagSource,
    tags: Vec<TagRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TagSource {
    project: String,
    file: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TagRecord {
    id: u32,
    short: String,
    unit_id: u32,
    long: String,
}

/// Index of the active locale (see [`Locale::from_index`]); defaults to
/// `EnUs` (0) until [`set_locale`] is called.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Select the locale that [`desc`] / [`desc_or`] read from. Call once at
/// startup, before any export runs.
pub fn set_locale(locale: Locale) {
    ACTIVE.store(locale as u8, Ordering::Relaxed);
}

/// Parse and cache the tag table for one locale (each locale keeps its own
/// table for the process lifetime).
fn table_for(locale: Locale) -> &'static HashMap<u32, Tag> {
    static TABLES: [OnceLock<HashMap<u32, Tag>>; 6] = [const { OnceLock::new() }; 6];
    TABLES[locale as usize].get_or_init(|| parse_table(locale))
}

fn parse_table(locale: Locale) -> HashMap<u32, Tag> {
    let file: TagFile = serde_json::from_str(locale.file())
        .unwrap_or_else(|error| panic!("invalid tag JSON for {locale:?}: {error}"));
    assert_eq!(
        file.locale,
        locale.code(),
        "locale mismatch in {}",
        locale.code()
    );
    assert_eq!(file.source.project, "SBFspot");
    assert!(!file.source.file.is_empty());

    let mut map = HashMap::new();
    for record in file.tags {
        let id = record.id;
        assert!(
            map.insert(
                id,
                Tag {
                    short: record.short,
                    unit_id: record.unit_id,
                    long: record.long,
                }
            )
            .is_none(),
            "duplicate tag id {id} for {locale:?}"
        );
    }
    map
}

fn active_table() -> &'static HashMap<u32, Tag> {
    table_for(Locale::from_index(ACTIVE.load(Ordering::Relaxed)))
}

/// Long description for a tag id in the active locale (SBFspot
/// `tagdefs.getDesc`).
pub fn desc(tag: u32) -> Option<&'static str> {
    active_table().get(&tag).map(|t| t.long.as_str())
}

/// Long description for a tag id in the active locale, or `fallback` when
/// the id is unknown.
pub fn desc_or(tag: u32, fallback: &'static str) -> &'static str {
    desc(tag).unwrap_or(fallback)
}

/// Long description for a tag id in a specific locale, independent of the
/// active one (used by tests and CSV header rendering).
pub fn desc_in(locale: Locale, tag: u32) -> Option<&'static str> {
    table_for(locale).get(&tag).map(|t| t.long.as_str())
}

/// Device class names (DEVICECLASS enum).
pub fn device_class_name(class: u32) -> &'static str {
    match class {
        8000 => "All Devices",
        8001 => "Solar Inverter",
        8002 => "Wind Turbine Inverter",
        8007 => "Battery Inverter",
        8008 => "Charging Station",
        8009 => "Hybrid Inverter",
        8033 => "Consumer",
        8064 => "Sensor System",
        8065 => "Electricity Meter",
        8066 => "Gas Meter",
        8067 => "Generic Meter",
        8096 => "Tracker",
        8128 => "Communication Product",
        _ => "UNKNOWN CLASS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_json_table_is_complete_and_parseable() {
        for (locale, expected_entries) in [
            (Locale::EnUs, 8_487),
            (Locale::DeDe, 8_485),
            (Locale::EsEs, 8_485),
            (Locale::FrFr, 8_485),
            (Locale::ItIt, 8_485),
            (Locale::NlNl, 8_485),
        ] {
            let table = table_for(locale);
            assert_eq!(table.len(), expected_entries, "{locale:?}");
            assert_eq!(table.get(&1).map(|tag| tag.short.as_str()), Some("[%]"));
            assert_eq!(table.get(&1).map(|tag| tag.unit_id), Some(0));
            assert!(table
                .get(&16_777_214)
                .is_some_and(|tag| tag.long == "EndOfTagLst"));
        }

        let country = table_for(Locale::EnUs).get(&53).expect("country tag");
        assert_eq!(country.short, "Cntry");
        assert_eq!(country.unit_id, 8_855_040);
        assert_eq!(country.long, "Country standard");
    }
}
