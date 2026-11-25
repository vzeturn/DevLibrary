//! Read-only schema collector (RT-1183, phase 1).
//!
//! Reads only SQL Server catalog views (`sys.*`): tables, columns, keys, indexes, foreign
//! keys, views, procedures, triggers and estimated row counts. It never selects from a user
//! table, so no customer row can enter a [`Snapshot`]. [`queries::ensure_catalog_only`]
//! enforces that on every statement before it is sent.

pub mod collect;
pub mod model;
pub mod queries;
pub mod upload;

pub use collect::{Error, MetaSource, collect_database};
pub use model::{Column, Database, ForeignKey, Index, Routine, Snapshot, Table};
pub use upload::upload_body;
