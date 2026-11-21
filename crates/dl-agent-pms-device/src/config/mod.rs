//! Agent configuration (`Agent/AgentOptions.cs`, `AgentOptionsValidator.cs`,
//! `Adapters/Lock/LockOptions.cs`) and the .NET-compatible configuration reader that
//! `Host.CreateApplicationBuilder` provides to the C# agent.
//!
//! [`load`] reads the same sources in the same order, [`bind_agent_options`] converts
//! `Agent:*` like `ConfigurationBinder`, and [`validate`] applies the start-up checks. All
//! three fail with the text of the .NET exception ([`DotnetError`]).

mod binder;
mod dotnet;
mod options;
mod reader;
mod timespan;
mod validator;

pub use binder::bind_agent_options;
pub use dotnet::DotnetError;
pub use options::{AgentOptions, LockOptions, SECTION};
pub use reader::{
    Configuration, HostInputs, LoadedConfiguration, environment_variables, load,
    parse_command_line, parse_json_settings,
};
pub use timespan::{TimeSpan, TimeSpanParseError};
pub use validator::{MAX_COMMAND_TIMEOUT, ValidationErrors, validate};

pub(crate) use validator::{api_base_url_error, is_valid_agent_id};

pub(crate) use dotnet::eq_ignore_case;
