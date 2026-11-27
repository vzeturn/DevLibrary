//! Shared SMILE read layer (RT-1432). Every object is prefixed `SA_`, versioned here and
//! (re)installed by dl-agent; projects call the objects and never re-implement the join.
//!
//! Slice 1: `dbo.SA_TrnKey` + `dbo.SA_TrnCode_Resolve` (SMILE_FO `TransactionCode`).

use dl_tds::Client;

/// Bump when any script changes so agents can tell an installed layer is stale.
pub const LAYER_VERSION: u32 = 1;

/// Embedded scripts in deploy order.
pub const SCRIPTS: &[(&str, &str)] =
    &[("001_trn_code.sql", include_str!("../sql/001_trn_code.sql"))];

/// Splits a script on lines holding only `GO`, dropping blank batches.
#[must_use]
pub fn split_batches(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in body.split_inclusive('\n') {
        if line.trim().eq_ignore_ascii_case("go") {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(line);
        }
    }
    out.push(cur);
    out.into_iter().filter(|b| !b.trim().is_empty()).collect()
}

/// Installs (drop + create, idempotent) every script into the connection's database.
///
/// # Errors
/// The first failing batch, named by script and batch number.
pub fn install_into(c: &mut Client) -> Result<(), String> {
    for (name, body) in SCRIPTS {
        for (i, batch) in split_batches(body).iter().enumerate() {
            c.batch(batch)
                .map_err(|e| format!("smile-shared: {name} batch #{}: {e}", i + 1))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_drops_go_and_blank_batches() {
        let b = split_batches("A\nGO\n  \ngo\nB\n");
        assert_eq!(b, vec!["A\n".to_owned(), "B\n".to_owned()]);
    }

    #[test]
    fn trn_code_script_has_four_batches_and_no_try_convert() {
        let body = SCRIPTS[0].1;
        assert_eq!(split_batches(body).len(), 4);
        assert!(!body.to_ascii_uppercase().contains("TRY_CONVERT"));
    }
}
