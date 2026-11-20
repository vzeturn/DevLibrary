//! The SQL Server connections the older per-product agents on this machine used
//! (PROTOCOL.md §9.8.1), so the installer can reuse one instead of asking.
//!
//! Seven products: Check-In (MM-Arrival), Membership, PDA SyncAgent, SMILE-PMS site agent,
//! HotelLink, E-Invoice and SRBackup. Each is looked for in three places: its folder as
//! installed, the folders a bench run renamed (`<dir>.bench-old-<stamp>`), and the folders a
//! bench record (`<data>/bench-removed-*.json`, `dirs[].from → to`) lists. Secrets sealed with
//! DPAPI LocalMachine are opened; a CurrentUser blob is skipped with a warning (the service
//! account could never open it).
//!
//! The formats are read here, not through the module crates: those depend on this crate
//! (`dl-agent-bin` composes them), so the installer reads the few fields it needs itself.
//! Nothing here is ever printed with its password: [`Found`]'s `Debug` hides the connection.

mod formats;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use dl_json::Value;

/// Where a product keeps its files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Base {
    /// `%ProgramData%`.
    ProgramData,
    /// Every Program Files folder (64-bit and x86).
    ProgramFiles,
}

/// Which of the three places a connection was found in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// The folder as the product installed it.
    InPlace,
    /// A folder a bench record says it renamed.
    BenchRecord,
    /// A `<dir>.bench-old-<stamp>` sibling.
    BenchOld,
}

impl Location {
    /// Wire and log name.
    pub fn name(self) -> &'static str {
        match self {
            Location::InPlace => "in_place",
            Location::BenchRecord => "bench_record",
            Location::BenchOld => "bench_old",
        }
    }
}

/// One connection an old agent used.
#[derive(Clone, PartialEq, Eq)]
pub struct Found {
    /// Product key (`checkin`, `membership`, `pda_sync`, `pms`, `hotellink`, `einvoice`,
    /// `srbackup`).
    pub product: &'static str,
    /// Product name for people.
    pub label: &'static str,
    /// Where the folder was.
    pub location: Location,
    /// The file the connection came from.
    pub file: PathBuf,
    /// ADO or go-mssqldb URL connection, password included: never print it.
    pub connection: String,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Found")
            .field("product", &self.product)
            .field("location", &self.location)
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

/// What a scan found, and what it could not read.
#[derive(Debug, Default)]
pub struct Scan {
    /// In priority order: in place before renamed, products in [`PRODUCTS`] order.
    pub found: Vec<Found>,
    /// Unreadable files, CurrentUser blobs, configs without SQL (never a secret).
    pub warnings: Vec<String>,
}

/// The folders to search.
#[derive(Clone, Debug)]
pub struct Roots {
    /// `%ProgramData%`.
    pub program_data: PathBuf,
    /// `%ProgramW6432%`, `%ProgramFiles%`, `%ProgramFiles(x86)%` (deduplicated).
    pub program_files: Vec<PathBuf>,
    /// dl-agent's data folder, where bench records are written.
    pub data_dir: PathBuf,
}

impl Roots {
    /// The folders of this machine.
    pub fn of_this_machine(data_dir: &Path) -> Roots {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        let mut program_files: Vec<PathBuf> = Vec::new();
        for k in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(p) = var(k)
                && !program_files.iter().any(|q| same_path(q, &p))
            {
                program_files.push(p);
            }
        }
        Roots {
            program_data: var("ProgramData").unwrap_or_else(|| PathBuf::from(r"C:\ProgramData")),
            program_files,
            data_dir: data_dir.to_path_buf(),
        }
    }

    fn bases(&self, base: Base) -> Vec<PathBuf> {
        match base {
            Base::ProgramData => vec![self.program_data.clone()],
            Base::ProgramFiles => self.program_files.clone(),
        }
    }
}

/// Opens DPAPI blobs (a fake in tests).
pub trait Unseal {
    /// Opens a LocalMachine `blob` sealed with `entropy` (empty = none).
    ///
    /// # Errors
    /// The DPAPI error, in words.
    fn unseal(&self, blob: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String>;
}

/// DPAPI on Windows; elsewhere every blob fails (there is no DPAPI to open it).
pub struct Dpapi;

impl Unseal for Dpapi {
    fn unseal(&self, blob: &[u8], entropy: &[u8]) -> Result<Vec<u8>, String> {
        dl_winsvc::dpapi::unprotect_with_entropy(
            blob,
            dl_winsvc::dpapi::Scope::LocalMachine,
            entropy,
        )
        .map_err(|e| e.to_string())
    }
}

/// `CRYPTPROTECT_LOCAL_MACHINE`, kept in a DPAPI blob's `dwFlags`.
const LOCAL_MACHINE_FLAG: u32 = 0x4;
/// `dwVersion` + provider GUID + master key version + master key GUID, then `dwFlags`.
const FLAGS_AT: usize = 40;

/// The scope a DPAPI blob was sealed with: `Some(true)` LocalMachine, `Some(false)`
/// CurrentUser, `None` when `blob` is not a DPAPI blob at all.
pub fn machine_scope(blob: &[u8]) -> Option<bool> {
    let version = u32::from_le_bytes(blob.get(..4)?.try_into().ok()?);
    let flags = u32::from_le_bytes(blob.get(FLAGS_AT..FLAGS_AT + 4)?.try_into().ok()?);
    (version == 1).then_some(flags & LOCAL_MACHINE_FLAG != 0)
}

/// Opens `blob` from `file` when it is LocalMachine; a CurrentUser blob is a warning.
pub(crate) fn open(
    unseal: &dyn Unseal,
    blob: &[u8],
    entropy: &[u8],
    file: &Path,
    warnings: &mut Vec<String>,
) -> Option<Vec<u8>> {
    if machine_scope(blob) == Some(false) {
        warnings.push(format!(
            "{}: sealed for one Windows user (DPAPI CurrentUser), the service cannot use it; skipped",
            file.display()
        ));
        return None;
    }
    match unseal.unseal(blob, entropy) {
        Ok(p) => Some(p),
        Err(e) => {
            warnings.push(format!("{}: cannot be decrypted here: {e}", file.display()));
            None
        }
    }
}

/// Reads one product folder: `(file, connection)` pairs.
type Reader = fn(&Path, &dyn Unseal, &mut Vec<String>) -> Vec<(PathBuf, String)>;

/// One older agent: where it lives and how its SQL connection is read.
pub struct Product {
    /// Key ([`Found::product`]).
    pub key: &'static str,
    /// Name for people.
    pub label: &'static str,
    /// Folders, each cited from the product's installer (see also the bench `$LegacyDirs`).
    pub places: &'static [(Base, &'static str)],
    read: Reader,
}

/// The seven older agents, in the order their connections are tried.
pub const PRODUCTS: [Product; 7] = [
    Product {
        key: "checkin",
        label: "The old Check-In agent",
        places: &[(Base::ProgramFiles, "MM-Arrival Agent")],
        read: formats::checkin,
    },
    Product {
        key: "membership",
        label: "The old Membership agent",
        places: &[
            (Base::ProgramData, "MM.Membership.Agent"),
            (Base::ProgramFiles, "MM.Membership.Agent"),
        ],
        read: formats::membership,
    },
    Product {
        key: "pda_sync",
        label: "The old PDA SyncAgent",
        places: &[(Base::ProgramData, r"MM\SyncAgent")],
        read: formats::pda_sync,
    },
    Product {
        key: "pms",
        label: "The old SMILE-PMS site agent",
        places: &[(Base::ProgramData, r"SmilePMS\Agent")],
        read: formats::pms,
    },
    Product {
        key: "hotellink",
        label: "The old HotelLink agent",
        places: &[(Base::ProgramData, "SMILE-HL-Agent")],
        read: formats::hotellink,
    },
    Product {
        key: "einvoice",
        label: "The old E-Invoice agent",
        places: &[(Base::ProgramData, "MM-EInvoice")],
        read: formats::einvoice,
    },
    Product {
        key: "srbackup",
        label: "The old SRBackup agent",
        places: &[(Base::ProgramData, "srbackup")],
        read: formats::srbackup,
    },
];

fn same_path(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    n(a) == n(b)
}

/// `(from, to)` of every bench record in `data_dir`, newest record first.
fn bench_moves(data_dir: &Path, warnings: &mut Vec<String>) -> Vec<(PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(data_dir) else {
        return Vec::new();
    };
    let mut records: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("bench-removed-") && n.ends_with(".json"))
        })
        .collect();
    records.sort();
    records.reverse();
    let mut moves = Vec::new();
    for r in records {
        let doc = match crate::store::read_json(&r) {
            Ok(Some(d)) => d,
            Ok(None) => continue,
            Err(e) => {
                warnings.push(format!("{}: bench record not readable: {e}", r.display()));
                continue;
            }
        };
        for d in doc
            .get("dirs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let s = |k: &str| d.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
            if let (Some(from), Some(to)) = (s("from"), s("to")) {
                moves.push((PathBuf::from(from), PathBuf::from(to)));
            }
        }
    }
    moves
}

/// `<parent>/<leaf>.bench-old-*` folders, newest first.
fn bench_old(dir: &Path) -> Vec<PathBuf> {
    let (Some(parent), Some(leaf)) = (dir.parent(), dir.file_name().and_then(|l| l.to_str()))
    else {
        return Vec::new();
    };
    let prefix = format!("{}.bench-old-", leaf.to_ascii_lowercase());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.to_ascii_lowercase().starts_with(&prefix))
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out.reverse();
    out
}

/// Every folder of `product`, by location, each once.
fn folders(
    product: &Product,
    roots: &Roots,
    moves: &[(PathBuf, PathBuf)],
) -> Vec<(Location, PathBuf)> {
    let mut out: Vec<(Location, PathBuf)> = Vec::new();
    let add = |loc: Location, p: PathBuf, out: &mut Vec<(Location, PathBuf)>| {
        if p.is_dir() && !out.iter().any(|(_, q)| same_path(q, &p)) {
            out.push((loc, p));
        }
    };
    let homes: Vec<PathBuf> = product
        .places
        .iter()
        .flat_map(|(base, rel)| {
            roots
                .bases(*base)
                .into_iter()
                .map(move |b| b.join(rel.replace('\\', std::path::MAIN_SEPARATOR_STR)))
        })
        .collect();
    for h in &homes {
        add(Location::InPlace, h.clone(), &mut out);
    }
    for h in &homes {
        for (from, to) in moves {
            if same_path(from, h) {
                add(Location::BenchRecord, to.clone(), &mut out);
            }
        }
    }
    for h in &homes {
        for old in bench_old(h) {
            add(Location::BenchOld, old, &mut out);
        }
    }
    out
}

/// Looks for every older agent's SQL connection on this machine.
pub fn scan(roots: &Roots, unseal: &dyn Unseal) -> Scan {
    let mut scan = Scan::default();
    let moves = bench_moves(&roots.data_dir, &mut scan.warnings);
    let mut by_location: Vec<Found> = Vec::new();
    for product in &PRODUCTS {
        for (location, dir) in folders(product, roots, &moves) {
            for (file, connection) in (product.read)(&dir, unseal, &mut scan.warnings) {
                by_location.push(Found {
                    product: product.key,
                    label: product.label,
                    location,
                    file,
                    connection,
                });
            }
        }
    }
    // In place first (the agent was running with it), then what a bench moved aside.
    for loc in [Location::InPlace, Location::BenchRecord, Location::BenchOld] {
        scan.found
            .extend(by_location.iter().filter(|f| f.location == loc).cloned());
    }
    scan
}
