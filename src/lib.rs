//! SakuraDB — a lightweight embedded SQL database for learning purposes.
//!
//! Provides a `Database` facade that chains SQL parsing with query execution.

mod engine;
mod sql;
mod storage;

use anyhow::Result;

/// Public API entry point.
///
/// Wraps an `Engine` and delegates SQL execution.
pub struct Database {
    /// The query execution engine.
    engine: engine::Engine,
}

impl Database {
    /// Create a new `Database` instance.
    ///
    /// # Arguments
    ///
    /// * `data_dir` — path to the data directory.
    ///
    /// # Returns
    ///
    /// * `Database` — a new instance with no database selected.
    pub fn new(data_dir: &str) -> Self {
        Self {
            engine: engine::Engine::open(data_dir).expect("failed to initialize engine"),
        }
    }

    /// Parse and execute a SQL string.
    ///
    /// # Arguments
    ///
    /// * `sql` — the SQL statement to parse and execute.
    ///
    /// # Returns
    ///
    /// * `Result<String>` — a human-readable result string.
    pub fn execute(&mut self, sql: &str) -> Result<String> {
        let ast = sql::parser(sql)?;
        self.engine.execute(ast).map(|result| result.into())
    }
}

impl From<engine::QueryResult> for String {
    fn from(result: engine::QueryResult) -> Self {
        match result {
            engine::QueryResult::Success => "Query executed successfully".to_string(),
            engine::QueryResult::Rows(rows) => rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|value| format!("{:?}", value))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .collect::<Vec<_>>()
                .join("\n"),
            engine::QueryResult::RowsAffected(count) => {
                format!("{} rows affected", count)
            }
        }
    }
}
