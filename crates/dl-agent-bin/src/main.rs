//! `dl-agent`: the shared agent with the built-in `sys` module and every product module.
//! Product modules are opt-in: each runs only where the remote config enables it
//! (`modules.<name>.enabled = true`).
//!
//! The executable lives in its own package because product modules depend on the
//! `dl-agent` library; registering them from the library's own binary would be a cycle.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn main() {
    use dl_agent_membership::{membership_enroll_tool, membership_module};
    let app = dl_agent::cli::App::new("dl-agent", "DevLibrary Agent")
        .module(dl_agent_einvoice::einvoice_module())
        .tool(dl_agent_einvoice::einvoice_enroll_tool())
        .module(membership_module())
        .tool(membership_enroll_tool())
        .module(dl_agent_pms::pms_site_module())
        .tool(dl_agent_pms::pms_migrate_tool())
        .tool(dl_agent_pms::pms_enroll_tool())
        .module(dl_agent_pms_device::pms_device_module());
    std::process::exit(dl_agent::cli::main(app));
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn main() {}
