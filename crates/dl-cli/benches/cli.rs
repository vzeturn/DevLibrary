//! `cargo bench -p dl-cli`

use dl_bench::Bench;
use dl_cli::{Arg, Command};

fn cli() -> Command {
    Command::new("hotel")
        .version("1.2.0")
        .about("Hotel PMS tools")
        .arg(Arg::flag("verbose").short('v').count().global())
        .arg(
            Arg::option("port")
                .short('p')
                .default("8080")
                .env("HOTEL_PORT"),
        )
        .arg(
            Arg::option("format")
                .choices(["json", "text"])
                .default("text"),
        )
        .arg(Arg::option("include").short('I').multiple())
        .arg(Arg::positional("input").required())
        .arg(Arg::positional("rest").multiple())
        .subcommand(Command::new("serve").arg(Arg::option("bind")))
}

fn main() {
    let mut b = Bench::from_args();
    let cmd = cli();
    let argv = [
        "hotel",
        "-vv",
        "--port=9000",
        "-I",
        "a",
        "-Ib",
        "--format",
        "json",
        "in.txt",
        "x",
        "y",
    ];
    b.run("parse 10 arguments", || {
        cmd.parse_from_with_env(argv, |_| None).is_ok()
    });
    b.run("parse with subcommand", || {
        cmd.parse_from_with_env(["hotel", "f", "serve", "--bind", "0.0.0.0", "-v"], |_| None)
            .is_ok()
    });
    b.run("unknown option with suggestion", || {
        cmd.parse_from_with_env(["hotel", "--prot", "1"], |_| None)
            .is_err()
    });
    b.run("render help", || cmd.render_help());
}
