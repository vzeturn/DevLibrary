//! A small hotel-PMS maintenance tool with two subcommands.
//!
//! ```text
//! cargo run -p dl-cli --example demo -- --help
//! cargo run -p dl-cli --example demo -- serve -vv --port 9000
//! cargo run -p dl-cli --example demo -- import -H SGN01 --dry-run bookings.csv -
//! DEMO_PORT=7000 cargo run -p dl-cli --example demo -- serve
//! ```

use dl_cli::{Arg, Command, Error, ErrorKind, Matches, ValueSource};

fn cli() -> Command {
    Command::new("demo")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Hotel PMS maintenance tool")
        .arg(
            Arg::flag("verbose")
                .short('v')
                .count()
                .global()
                .help("Increase log verbosity (-v, -vv, -vvv)"),
        )
        .arg(
            Arg::option("config")
                .short('c')
                .value_name("FILE")
                .env("DEMO_CONFIG")
                .default("demo.toml")
                .global()
                .help("Configuration file"),
        )
        .subcommand_required()
        .subcommand(
            Command::new("serve")
                .about("Run the HTTP API server")
                .arg(
                    Arg::option("port")
                        .short('p')
                        .value_name("PORT")
                        .default("8080")
                        .env("DEMO_PORT")
                        .help("Listen port"),
                )
                .arg(
                    Arg::option("bind")
                        .default("127.0.0.1")
                        .help("Bind address"),
                )
                .arg(
                    Arg::option("log-format")
                        .choices(["json", "text"])
                        .default("text")
                        .help("Log line format"),
                ),
        )
        .subcommand(
            Command::new("import")
                .about("Import reservations from CSV files")
                .arg(
                    Arg::flag("dry-run")
                        .short('n')
                        .help("Validate the files without writing to the database"),
                )
                .arg(
                    Arg::option("hotel")
                        .short('H')
                        .required()
                        .help("Hotel code, e.g. SGN01"),
                )
                .arg(
                    Arg::positional("files")
                        .required()
                        .multiple()
                        .help("CSV files to import; '-' reads standard input"),
                ),
        )
}

fn run(m: &Matches) -> Result<(), Error> {
    let verbosity = m.count("verbose");
    let config = m.value("config").unwrap_or_default();
    if verbosity > 0 {
        eprintln!("[debug] verbosity={verbosity} config={config}");
    }
    match m.subcommand() {
        Some(("serve", serve)) => {
            let port: u16 = serve.required("port")?;
            if port < 1024 {
                return Err(Error::new(
                    ErrorKind::InvalidValue,
                    format!("port {port} is privileged; use 1024 or above"),
                ));
            }
            let bind = serve.value("bind").unwrap_or_default();
            let from = match serve.source("port") {
                Some(ValueSource::Env) => " (from DEMO_PORT)",
                Some(ValueSource::Default) => " (default)",
                _ => "",
            };
            println!(
                "serving on {bind}:{port}{from}, log format {}",
                serve.value("log-format").unwrap_or_default()
            );
        }
        Some(("import", import)) => {
            let hotel = import.value("hotel").unwrap_or_default();
            let mode = if import.flag("dry-run") {
                "checking"
            } else {
                "importing"
            };
            for file in import.values("files") {
                let name = if file == "-" { "<stdin>" } else { file };
                println!("{mode} {name} for hotel {hotel}");
            }
        }
        // `subcommand_required()` makes the parser reject a missing subcommand.
        _ => {}
    }
    Ok(())
}

fn main() {
    let matches = cli().parse_env_or_exit();
    if let Err(e) = run(&matches) {
        e.exit();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn definition_is_valid() {
        super::cli().debug_assert();
    }
}
