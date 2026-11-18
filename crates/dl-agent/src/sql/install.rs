//! Installs a signed SQL bundle into one database (PROTOCOL.md §9.7): all or nothing in
//! one transaction, refused before any change when the site's engine or compatibility level
//! is older than the bundle's syntax, every replaced definition saved first, and the
//! bundle's `requires` probed before commit so a half-working schema is never left behind.

use std::path::{Path, PathBuf};

use dl_json::{Value, json};

use super::bundle::{Bundle, Kind, Object, object_too_new, with_verb};
use super::pool::Connection;
use super::{probe, problem_code, survey};
use crate::Error;
use crate::manifest::TrustedKeys;

/// Extended property stamped on every installed object: `product@version sha256`.
pub const STAMP: &str = "dl_bundle";

/// How to install.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Run everything, then roll back: reports what would change and what would fail.
    pub dry_run: bool,
    /// Replace objects whose definition cannot be read (`WITH ENCRYPTION`, no
    /// VIEW DEFINITION), so no backup is possible.
    pub force: bool,
    /// Where replaced definitions are saved (`<dir>/<schema>.<name>.sql`); required to
    /// replace an existing object unless `force`.
    pub backup_dir: Option<PathBuf>,
}

const STATE_SQL: &str = "SELECT CAST(OBJECT_ID(@n) AS nvarchar(20)) AS id, \
OBJECT_DEFINITION(OBJECT_ID(@n)) AS def, \
(SELECT CAST(p.value AS nvarchar(400)) FROM sys.extended_properties p \
 WHERE p.class = 1 AND p.major_id = OBJECT_ID(@n) AND p.minor_id = 0 AND p.name = N'dl_bundle') AS stamp";

const STAMP_SQL: &str = "IF EXISTS (SELECT 1 FROM sys.extended_properties \
WHERE class = 1 AND major_id = OBJECT_ID(@obj) AND minor_id = 0 AND name = N'dl_bundle') \
EXEC sys.sp_updateextendedproperty @name = N'dl_bundle', @value = @v, \
@level0type = N'SCHEMA', @level0name = @s, @level1type = @t, @level1name = @n \
ELSE EXEC sys.sp_addextendedproperty @name = N'dl_bundle', @value = @v, \
@level0type = N'SCHEMA', @level0name = @s, @level1type = @t, @level1name = @n";

/// One object's state in the database.
struct State {
    exists: bool,
    definition: Option<String>,
    stamp: Option<String>,
}

fn state(conn: &mut impl Connection, o: &Object) -> Result<State, Error> {
    let q = o.name.quoted();
    let row = conn
        .query_json(STATE_SQL, &[("n", q.as_str())])?
        .into_iter()
        .next()
        .unwrap_or(Value::Null);
    let text = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(State {
        exists: text("id").is_some(),
        definition: text("def"),
        stamp: text("stamp"),
    })
}

/// `min(engine major × 10, compatibility level)`: the T-SQL level the site accepts.
pub fn site_level(conn: &mut impl Connection) -> Result<u64, Error> {
    let s = survey::survey(conn, None)?;
    let major = s["engine"]["major"].as_u64().unwrap_or(0);
    let compat = s["database"]["compatibility_level"].as_u64().unwrap_or(0);
    Ok((major * 10).min(compat))
}

fn stamp_of(b: &Bundle, o: &Object) -> String {
    format!("{} {}", b.stamp(), o.sha256())
}

fn backup(dir: &Path, o: &Object, def: &str) -> Result<PathBuf, Error> {
    std::fs::create_dir_all(dir).map_err(|e| Error::Io("creating the SQL backup folder", e))?;
    let file = dir.join(format!("{}.{}.sql", o.name.schema, o.name.name).replace(['/', '\\'], "_"));
    std::fs::write(&file, def).map_err(|e| Error::Io("writing an SQL backup", e))?;
    Ok(file)
}

/// Installs `bundle` (already verified) through `conn`. `{"ok", "dry_run", "level",
/// "applied": [{kind, name, action}], "unchanged": [name], "refused"?, "failed"?, "probe"?,
/// "backups": [path]}`; `ok` is `false` when anything was refused or failed, and then
/// nothing was changed.
///
/// # Errors
/// Session errors outside the install itself (survey, state, backups, transaction control).
pub fn install(
    conn: &mut impl Connection,
    bundle: &Bundle,
    opts: &Options,
) -> Result<Value, Error> {
    let level = site_level(conn)?;
    let refused: Vec<Value> = bundle
        .objects
        .iter()
        .filter_map(|o| {
            let f = object_too_new(o, level);
            (!f.is_empty()).then(|| {
                let names: Vec<String> =
                    f.iter().map(|(n, l)| format!("{n} (level {l})")).collect();
                json!({"name": o.name.to_string(), "features": names})
            })
        })
        .collect();
    if !refused.is_empty() {
        return Ok(
            json!({"ok": false, "dry_run": opts.dry_run, "level": level, "refused": refused,
            "error": "syntax newer than the site's SQL Server / compatibility level"}),
        );
    }

    // Plan: what changes, and the definitions it replaces.
    let mut plan: Vec<(&Object, Option<bool>)> = Vec::new(); // Some(exists) for objects
    let mut unchanged = Vec::new();
    let mut unreadable = Vec::new();
    let mut replaced: Vec<(&Object, String)> = Vec::new();
    for o in &bundle.objects {
        if o.kind == Kind::Script {
            plan.push((o, None));
            continue;
        }
        let st = state(conn, o)?;
        if st.stamp.as_deref() == Some(stamp_of(bundle, o).as_str()) {
            unchanged.push(Value::from(o.name.to_string()));
            continue;
        }
        if st.exists {
            match st.definition {
                Some(d) => replaced.push((o, d)),
                None if !opts.force => unreadable.push(Value::from(o.name.to_string())),
                None => {}
            }
        }
        plan.push((o, Some(st.exists)));
    }
    if !unreadable.is_empty() {
        return Ok(json!({"ok": false, "dry_run": opts.dry_run, "level": level,
            "unreadable": unreadable,
            "error": "existing definitions cannot be read (WITH ENCRYPTION or no VIEW DEFINITION): no backup possible; pass force to replace them"}));
    }
    let mut backups = Vec::new();
    if !opts.dry_run && !replaced.is_empty() {
        let Some(dir) = &opts.backup_dir else {
            return Err(Error::Config(
                "bundle install: replacing objects needs a backup folder".into(),
            ));
        };
        for (o, def) in &replaced {
            backups.push(Value::from(backup(dir, o, def)?.display().to_string()));
        }
    }

    conn.run("BEGIN TRANSACTION", &[])?;
    let outcome = apply(conn, bundle, &plan);
    let (applied, failed, probed) = match outcome {
        Ok(v) => v,
        Err(e) => {
            conn.run("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION", &[])?;
            return Err(e);
        }
    };
    let probe_ok = probed
        .as_ref()
        .is_none_or(|p| p["ok"].as_bool() == Some(true));
    let ok = failed.is_none() && probe_ok;
    if ok && !opts.dry_run {
        conn.run("COMMIT TRANSACTION", &[])?;
    } else {
        conn.run("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION", &[])?;
    }
    let mut out = json!({"ok": ok, "dry_run": opts.dry_run, "level": level, "applied": applied,
        "unchanged": unchanged, "backups": backups});
    if let Some(o) = out.as_object_mut() {
        if let Some(f) = failed {
            o.insert("failed", f);
        }
        if let Some(p) = probed {
            if !probe_ok {
                o.insert(
                    "error",
                    "installed objects do not satisfy the bundle's requires",
                );
            }
            o.insert("probe", p);
        }
    }
    Ok(out)
}

type Applied = (Vec<Value>, Option<Value>, Option<Value>);

/// Runs the plan inside the open transaction; stops at the first failing object.
fn apply(
    conn: &mut impl Connection,
    b: &Bundle,
    plan: &[(&Object, Option<bool>)],
) -> Result<Applied, Error> {
    let mut applied = Vec::new();
    for (o, exists) in plan {
        let (sql, action) = match exists {
            None => (o.sql.clone(), "script"),
            Some(e) => (with_verb(o, *e)?, if *e { "alter" } else { "create" }),
        };
        let mut step = conn.run(&sql, &[]);
        if step.is_ok()
            && let Some(t) = o.kind.level1()
        {
            let (q, v) = (o.name.quoted(), stamp_of(b, o));
            step = conn.run(
                STAMP_SQL,
                &[
                    ("obj", &q),
                    ("v", &v),
                    ("s", &o.name.schema),
                    ("t", t),
                    ("n", &o.name.name),
                ],
            );
        }
        if let Err(e) = step {
            let failed = json!({"kind": o.kind.as_str(), "name": o.name.to_string(),
                "code": problem_code(&e, "sql_error"), "error": e.to_string()});
            return Ok((applied, Some(failed), None));
        }
        applied
            .push(json!({"kind": o.kind.as_str(), "name": o.name.to_string(), "action": action}));
    }
    let probed = match b.requirement()? {
        Some(req) => Some(probe::probe(conn, &req)?),
        None => None,
    };
    Ok((applied, None, probed))
}

/// Runs `f`; when it fails because an object is missing (`sql_missing_object`), verifies
/// and installs `bundle`, then runs `f` once more. The first error is returned when the
/// install does not succeed.
///
/// # Errors
/// `f`'s error, or the verification / install error.
pub fn with_repair<C: Connection, T>(
    conn: &mut C,
    bundle: &Bundle,
    trusted: &TrustedKeys,
    opts: &Options,
    mut f: impl FnMut(&mut C) -> Result<T, Error>,
) -> Result<T, Error> {
    match f(conn) {
        Err(e) if problem_code(&e, "") == "sql_missing_object" => {
            bundle.verify(trusted)?;
            let r = install(
                conn,
                bundle,
                &Options {
                    dry_run: false,
                    ..opts.clone()
                },
            )?;
            if r["ok"].as_bool() != Some(true) {
                return Err(e);
            }
            f(conn)
        }
        other => other,
    }
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;
