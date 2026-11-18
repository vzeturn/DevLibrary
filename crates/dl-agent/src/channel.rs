//! Release channels and staged rollout (PROTOCOL.md §5).

use core::fmt;
use core::str::FromStr;

use dl_hash::{Digest, Sha256};

use crate::Error;

/// Where an agent takes its releases from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// New builds first: agents of demo tenants and agents marked for canary.
    Canary,
    /// Builds that passed the canary gate.
    Stable,
}

impl Channel {
    /// Wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Canary => "canary",
            Channel::Stable => "stable",
        }
    }
}

impl FromStr for Channel {
    type Err = Error;

    fn from_str(s: &str) -> Result<Channel, Error> {
        match s {
            "canary" => Ok(Channel::Canary),
            "stable" => Ok(Channel::Stable),
            other => Err(Error::Config(format!("unknown channel {other:?}"))),
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The channel the hub assigns: demo tenants and explicit canary agents get canary.
pub fn channel_for(tenant_is_demo: bool, override_channel: Option<Channel>) -> Channel {
    match override_channel {
        Some(c) => c,
        None if tenant_is_demo => Channel::Canary,
        None => Channel::Stable,
    }
}

/// The agent's stable bucket 0–99 for one release, so a rollout percentage picks a fixed,
/// evenly spread subset that grows as the percentage grows.
pub fn rollout_bucket(agent_id: &str, product: &str, version: &str) -> u8 {
    let h = Sha256::digest(format!("{agent_id}|{product}|{version}"));
    (u16::from_be_bytes([h[0], h[1]]) % 100) as u8 // < 100, fits u8
}

/// Whether the agent is inside a rollout of `percent` (0–100).
pub fn in_rollout(agent_id: &str, product: &str, version: &str, percent: u8) -> bool {
    rollout_bucket(agent_id, product, version) < percent.min(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_tenants_go_canary_unless_overridden() {
        assert_eq!(channel_for(true, None), Channel::Canary);
        assert_eq!(channel_for(false, None), Channel::Stable);
        assert_eq!(channel_for(true, Some(Channel::Stable)), Channel::Stable);
        assert_eq!(channel_for(false, Some(Channel::Canary)), Channel::Canary);
    }

    #[test]
    fn rollout_is_monotonic_and_spread() {
        let ids: Vec<String> = (0..1000).map(|i| format!("agent-{i}")).collect();
        let count = |p: u8| ids.iter().filter(|id| in_rollout(id, "dl-agent", "1.4.0", p)).count();
        assert_eq!(count(0), 0);
        assert_eq!(count(100), 1000);
        let ten = count(10);
        assert!((50..=150).contains(&ten), "10% picked {ten}");
        for id in &ids {
            if in_rollout(id, "dl-agent", "1.4.0", 10) {
                assert!(in_rollout(id, "dl-agent", "1.4.0", 50));
            }
        }
    }
}
