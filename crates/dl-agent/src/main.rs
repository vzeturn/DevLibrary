//! `dl-agent`: the shared agent with only the built-in `sys` module.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn main() {
    let app = dl_agent::cli::App::new("dl-agent", "DevLibrary Agent");
    std::process::exit(dl_agent::cli::main(app));
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn main() {}
