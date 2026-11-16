//! Prints every column of every query (one per line of a file) as a `String`, in the same
//! format as the go-mssqldb reference program used to check conversions:
//!
//! `cargo run -p dl-tds --example golden -- "<connection string>" queries.sql`

fn go_quote(s: &str) -> String {
    // Go's %q for valid UTF-8 text of the characters these queries produce.
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{FFFD}' => out.push('\u{FFFD}'),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cfg: dl_tds::Config = args[1].parse().expect("connection string");
    let mut client = dl_tds::Client::connect(&cfg).expect("connect");
    let text = std::fs::read_to_string(&args[2]).expect("queries file");
    let mut q = 0;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        q += 1;
        let results = match client.query(line, &[]) {
            Ok(r) => r,
            Err(e) => {
                println!("q{q} error: {e}");
                continue;
            }
        };
        let Some(set) = results.first() else { continue };
        for row in set.rows() {
            for i in 0..row.values().len() {
                match row.get::<Option<String>>(i) {
                    Ok(None) => println!("q{q} c{i} NULL"),
                    Ok(Some(s)) => {
                        let hex: String = s.bytes().map(|b| format!("{b:02x}")).collect();
                        println!("q{q} c{i} {} {hex}", go_quote(&s));
                    }
                    Err(e) => println!("q{q} c{i} scan error: {e}"),
                }
            }
        }
    }
}
