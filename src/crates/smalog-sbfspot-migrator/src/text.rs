//! Lossless decoding and inventory of text in legacy SQLite sources.

use serde::Serialize;
use sqlx::sqlite::{SqliteConnection, SqliteRow};
use sqlx::{Row, TypeInfo, ValueRef};

use smalog_storage::error::{Error, Result};

const MIGRATED_TEXT_COLUMNS: &[(&str, &[&str])] = &[
    (
        "Inverters",
        &["Name", "Type", "SW_Version", "Status", "GridRelay"],
    ),
    ("SpotData", &["Status", "GridRelay"]),
    (
        "EventData",
        &[
            "EventType",
            "Category",
            "EventGroup",
            "Tag",
            "OldValue",
            "NewValue",
            "UserGroup",
        ],
    ),
];

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct TextDecodingReport {
    pub source_utf8_count: u64,
    pub iso_8859_1_transcode_count: u64,
    pub iso_8859_1_transcoded_fields: Vec<TextFieldReference>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TextFieldReference {
    pub source_table: &'static str,
    pub source_key: i64,
    pub source_column: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceEncoding {
    Utf8,
    Iso8859_1,
}

pub(crate) fn source_text(
    row: &SqliteRow,
    source_table: &'static str,
    source_key: i64,
    source_column: &'static str,
) -> Result<Option<String>> {
    let bytes = row
        .try_get::<Option<Vec<u8>>, _>(source_column)
        .map_err(|error| {
            Error::Migration(format!(
                "cannot read SQLite TEXT/BLOB bytes from \
                 {source_table}[rowid={source_key}].{source_column}: {error}"
            ))
        })?;
    bytes
        .map(|bytes| decode_text(bytes, source_table, source_key, source_column))
        .transpose()
        .map(|value| value.map(|(text, _)| text))
}

pub(crate) async fn inspect_source_text(
    connection: &mut SqliteConnection,
) -> Result<TextDecodingReport> {
    let mut report = TextDecodingReport::default();
    for &(source_table, columns) in MIGRATED_TEXT_COLUMNS {
        let selected_columns = columns
            .iter()
            .map(|column| format!("\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let rows = sqlx::query(&format!(
            "SELECT rowid, {selected_columns} FROM \"{source_table}\" ORDER BY rowid"
        ))
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| {
            Error::Migration(format!(
                "cannot inspect migrated text in source table {source_table}: {error}"
            ))
        })?;
        for row in rows {
            let source_key = row.try_get::<i64, _>("rowid").map_err(|error| {
                Error::Migration(format!(
                    "cannot read source key for text in {source_table}: {error}"
                ))
            })?;
            for &source_column in columns {
                let bytes = row
                    .try_get::<Option<Vec<u8>>, _>(source_column)
                    .map_err(|error| {
                        Error::Migration(format!(
                            "cannot read SQLite TEXT/BLOB bytes from \
                             {source_table}[rowid={source_key}].{source_column}: {error}; \
                             SQLite storage class was {}",
                            row.try_get_raw(source_column)
                                .map(|value| value.type_info().name().to_owned())
                                .unwrap_or_else(|_| "unknown".into())
                        ))
                    })?;
                let Some(bytes) = bytes else {
                    continue;
                };
                let (_, encoding) = decode_text(bytes, source_table, source_key, source_column)?;
                match encoding {
                    SourceEncoding::Utf8 => report.source_utf8_count += 1,
                    SourceEncoding::Iso8859_1 => {
                        report.iso_8859_1_transcode_count += 1;
                        report
                            .iso_8859_1_transcoded_fields
                            .push(TextFieldReference {
                                source_table,
                                source_key,
                                source_column,
                            });
                    }
                }
            }
        }
    }
    Ok(report)
}

fn decode_text(
    bytes: Vec<u8>,
    source_table: &'static str,
    source_key: i64,
    source_column: &'static str,
) -> Result<(String, SourceEncoding)> {
    if let Some(byte_offset) = bytes.iter().position(|byte| *byte == 0) {
        return Err(Error::Migration(format!(
            "embedded NUL in migrated text at \
             {source_table}[rowid={source_key}].{source_column} byte {byte_offset}; \
             remove the NUL from the source value and rerun preflight"
        )));
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok((text, SourceEncoding::Utf8)),
        Err(error) => {
            let bytes = error.into_bytes();
            let text = bytes.into_iter().map(char::from).collect();
            Ok((text, SourceEncoding::Iso8859_1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_text, SourceEncoding};

    #[test]
    fn preserves_utf8_bytes_and_losslessly_maps_every_latin1_byte() {
        let utf8 = "Grüße 東京".as_bytes().to_vec();
        let (decoded, encoding) = decode_text(utf8.clone(), "T", 1, "C").unwrap();
        assert_eq!(encoding, SourceEncoding::Utf8);
        assert_eq!(decoded.as_bytes(), utf8);

        let latin1 = vec![0x41, 0x80, 0xa0, 0xe4, 0xff];
        let (decoded, encoding) = decode_text(latin1, "T", 1, "C").unwrap();
        assert_eq!(encoding, SourceEncoding::Iso8859_1);
        assert_eq!(
            decoded.chars().map(u32::from).collect::<Vec<_>>(),
            [0x41, 0x80, 0xa0, 0xe4, 0xff]
        );
        assert!(!decoded.contains('\u{fffd}'));
    }
}
