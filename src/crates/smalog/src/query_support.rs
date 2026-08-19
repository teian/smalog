//! The read/write path for the remembered query refusals.
//!
//! The collector asks this synchronously, inside a protocol conversation, so
//! it must not wait on a database. The refusals are therefore held in memory:
//! loaded once at startup and consulted from there, while a new refusal is
//! written back on a detached task.
//!
//! Losing a write costs one wasted query in a later cycle — the inverter
//! refuses again and the answer is recorded again — so a failed write is
//! logged and otherwise ignored.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use chrono::Utc;
use smalog_connection::query_support::{QuerySupportStore, SUPPORT_RECHECK_DAYS};
use smalog_storage::query_support::QuerySupportRow;
use smalog_storage::storage::Db;
use tracing::{debug, warn};

/// In-memory view of `inverter_query_support`, backed by the database.
pub struct QuerySupport {
    db: Arc<Db>,
    /// serial → query identifiers the inverter has refused.
    refused: RwLock<HashMap<u32, BTreeSet<String>>>,
}

impl QuerySupport {
    /// Load the refusals recorded within the recheck window.
    ///
    /// Anything older is left out, so the query is asked once more and the
    /// answer re-recorded — which is how a firmware update that adds a value
    /// gets noticed.
    pub async fn load(db: Arc<Db>) -> Arc<QuerySupport> {
        let cutoff = Utc::now().timestamp() - SUPPORT_RECHECK_DAYS * 86_400;
        let mut refused: HashMap<u32, BTreeSet<String>> = HashMap::new();
        match db.read_query_support(cutoff).await {
            Ok(rows) => {
                for row in rows {
                    refused
                        .entry(row.serial_number)
                        .or_default()
                        .insert(row.query);
                }
                debug!(
                    inverters = refused.len(),
                    "loaded remembered query refusals"
                );
            }
            Err(error) => warn!(%error, "cannot read remembered query refusals"),
        }
        Arc::new(QuerySupport {
            db,
            refused: RwLock::new(refused),
        })
    }
}

impl QuerySupportStore for QuerySupport {
    fn unsupported(&self, serial: u32) -> BTreeSet<String> {
        self.refused
            .read()
            .map(|refused| refused.get(&serial).cloned().unwrap_or_default())
            .unwrap_or_default()
    }

    fn remember(&self, serial: u32, query: &str, model: Option<&str>) {
        let known = self.refused.read().is_ok_and(|refused| {
            refused
                .get(&serial)
                .is_some_and(|queries| queries.contains(query))
        });
        if let Ok(mut refused) = self.refused.write() {
            refused.entry(serial).or_default().insert(query.to_owned());
        }
        if known {
            // Already stored, and only its date would change. Skipping the
            // write keeps a steady poll cycle from touching the database.
            return;
        }
        let row = QuerySupportRow {
            serial_number: serial,
            query: query.to_owned(),
            model: model.map(str::to_owned),
            recorded_at_s: Utc::now().timestamp(),
        };
        let db = Arc::clone(&self.db);
        tokio::spawn(async move {
            if let Err(error) = db.write_query_support(&row).await {
                warn!(%error, query = row.query, "cannot store a query refusal");
            }
        });
    }
}
