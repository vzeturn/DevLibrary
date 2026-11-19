//! The legacy bundle contract (port of `MM.PdaOrder.Sync.Contracts/Legacy`): supported
//! builds, table classification, `manifest.json`, verified reading and the SQL Server
//! store map of the oldest builds.

pub mod builds;
pub mod bundle;
pub mod bundle_files;
pub mod sql_store;

pub use builds::LegacyBuildStanding;
pub use bundle::{
    LegacyBundleFile, LegacyBundleMaintenance, LegacyBundleManifest, LegacyBundleSource,
    LegacyDisposition, LegacyLeftOut,
};
pub use bundle_files::{LegacyBundleError, RowReader, VerifiedLegacyBundle};
pub use sql_store::{LegacySqlColumn, LegacySqlKind, LegacySqlTable, LegacyValue};
