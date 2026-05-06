//! Query execution engine.
//!
//! Translates parsed SQL statements into B+ tree operations via the catalog.

mod executor;

pub(crate) use executor::{Engine, QueryResult};
