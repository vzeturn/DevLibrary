//! Hub jobs (the write side of `MM.Membership.Agent`): the payload, the write gate, the
//! dispatcher and the ack sent back to the hub.
//!
//! A job runs only when the module is not read-only. The dispatcher asks the gate first,
//! parses the payload, then hands SMILE writes to a [`dispatch::PmsWriter`]; every outcome
//! becomes an [`result::Ack`] carrying the delivery's lease (`Attempts`).

pub mod bed_terms;
pub mod beds;
pub mod dispatch;
#[cfg(test)]
pub(crate) mod fakes;
pub mod gate;
pub mod guest;
pub mod payload;
pub mod plan;
pub mod preflight;
pub mod result;
pub mod room_diff;
pub mod travel_agent;

/// `JobAction`, in the hub's enum order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobAction {
    /// `Create` (0).
    Create,
    /// `Modify` (1).
    Modify,
    /// `Cancel` (2).
    Cancel,
    /// `EarnPoints` (3).
    EarnPoints,
    /// `RedeemVoucher` (4).
    RedeemVoucher,
    /// `Availability` (5).
    Availability,
    /// `GrantReward` (6).
    GrantReward,
    /// `LinkFolio` (7).
    LinkFolio,
}

impl JobAction {
    /// Every action, in enum order.
    pub const ALL: [JobAction; 8] = [
        JobAction::Create,
        JobAction::Modify,
        JobAction::Cancel,
        JobAction::EarnPoints,
        JobAction::RedeemVoucher,
        JobAction::Availability,
        JobAction::GrantReward,
        JobAction::LinkFolio,
    ];

    /// The .NET enum name.
    pub fn name(self) -> &'static str {
        match self {
            JobAction::Create => "Create",
            JobAction::Modify => "Modify",
            JobAction::Cancel => "Cancel",
            JobAction::EarnPoints => "EarnPoints",
            JobAction::RedeemVoucher => "RedeemVoucher",
            JobAction::Availability => "Availability",
            JobAction::GrantReward => "GrantReward",
            JobAction::LinkFolio => "LinkFolio",
        }
    }

    /// The action from the hub's wire value: the enum name (any case) or its number.
    pub fn parse(raw: &str) -> Option<JobAction> {
        let raw = raw.trim();
        if let Ok(n) = raw.parse::<usize>() {
            return JobAction::ALL.get(n).copied();
        }
        JobAction::ALL
            .into_iter()
            .find(|a| a.name().eq_ignore_ascii_case(raw))
    }
}

impl std::fmt::Display for JobAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_parse_by_name_or_number() {
        assert_eq!(JobAction::parse("Create"), Some(JobAction::Create));
        assert_eq!(JobAction::parse("linkfolio"), Some(JobAction::LinkFolio));
        assert_eq!(JobAction::parse("4"), Some(JobAction::RedeemVoucher));
        assert_eq!(JobAction::parse("8"), None);
        assert_eq!(JobAction::parse("Teleport"), None);
    }
}
