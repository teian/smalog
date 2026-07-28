//! Localization: locale parsing and per-locale tag translation.

use smalog::connection::smadata2::tags::{self, Locale};

#[test]
fn parses_full_and_bare_locale_names() {
    assert_eq!(Locale::parse("en-US"), Some(Locale::EnUs));
    assert_eq!(Locale::parse("en"), Some(Locale::EnUs));
    assert_eq!(Locale::parse("de"), Some(Locale::DeDe));
    assert_eq!(Locale::parse("DE-DE"), Some(Locale::DeDe));
    assert_eq!(Locale::parse("es-ES"), Some(Locale::EsEs));
    assert_eq!(Locale::parse("fr"), Some(Locale::FrFr));
    assert_eq!(Locale::parse("it"), Some(Locale::ItIt));
    assert_eq!(Locale::parse("nl"), Some(Locale::NlNl));
    assert_eq!(Locale::parse("xx"), None);
    assert_eq!(Locale::parse(""), None);
}

#[test]
fn tag_text_is_translated_per_locale() {
    // Tag 35 = "Fault" (en-US) / "Fehler" (de-DE) / "Warnung"-family etc.
    assert_eq!(tags::desc_in(Locale::EnUs, 35), Some("Fault"));
    assert_eq!(tags::desc_in(Locale::DeDe, 35), Some("Fehler"));
    // Tag 455 = "Warning" / "Warnung".
    assert_eq!(tags::desc_in(Locale::EnUs, 455), Some("Warning"));
    assert_eq!(tags::desc_in(Locale::DeDe, 455), Some("Warnung"));
}

#[test]
fn every_locale_table_loads() {
    for loc in [
        Locale::EnUs,
        Locale::DeDe,
        Locale::EsEs,
        Locale::FrFr,
        Locale::ItIt,
        Locale::NlNl,
    ] {
        // A very common tag ("Ok", id 307) exists in every language file.
        assert!(
            tags::desc_in(loc, 307).is_some(),
            "locale {loc:?} missing tag 307"
        );
    }
}
