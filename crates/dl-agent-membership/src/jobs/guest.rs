//! `GuestDataResolver`: fills the guest fields a booker left blank from the member, once,
//! before any branch writes, so every folio, additional name and profile sees the same
//! result.

use super::payload::{Contact, JobPayload, Room};

/// `Blank`: trimmed, `None` when blank.
pub fn blank(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
}

fn split(full: &str) -> (Option<String>, Option<String>) {
    let t = full.trim();
    match t.rfind(' ') {
        Some(cut) if cut > 0 => (blank(Some(&t[..cut])), blank(Some(&t[cut + 1..]))),
        _ => (blank(Some(t)), None),
    }
}

/// `SplitFullName`: last name before the last space, first name after it.
pub fn split_full_name(full: Option<&str>) -> (Option<String>, Option<String>) {
    match full {
        Some(f) if !f.trim().is_empty() => split(f),
        _ => (None, None),
    }
}

/// `SmileGender`: 1 male, 0 female.
pub fn smile_gender(gender: Option<&str>) -> Option<u8> {
    match gender.map(|g| g.trim().to_lowercase()).as_deref() {
        Some("male" | "m") => Some(1),
        Some("female" | "f") => Some(0),
        _ => None,
    }
}

/// `Apply`.
pub fn apply(payload: JobPayload) -> JobPayload {
    if payload.member.is_none() && payload.rooms.iter().all(|r| r.guest.is_none()) {
        return payload;
    }
    let member = payload.member.clone();
    JobPayload {
        rooms: payload
            .rooms
            .into_iter()
            .map(|r| resolve(r, member.as_ref()))
            .collect(),
        ..payload
    }
}

/// `Resolve`.
pub fn resolve(room: Room, member: Option<&Contact>) -> Room {
    let empty = Contact::default();
    let guest = room.guest.clone().unwrap_or_default();
    let m = member.unwrap_or(&empty);
    let (mut last, mut first) = (
        blank(Some(&room.guest_last_name)),
        blank(Some(&room.guest_first_name)),
    );
    if last.is_none()
        && first.is_none()
        && let Some(full) = &guest.full_name
    {
        (last, first) = split(full);
    }
    if last.is_none()
        && first.is_none()
        && let Some(full) = &m.full_name
    {
        (last, first) = split(full);
    }
    let full = format!(
        "{} {}",
        last.as_deref().unwrap_or(""),
        first.as_deref().unwrap_or("")
    );
    let resolved = Contact {
        full_name: blank(Some(&full)),
        phone: guest.phone.or_else(|| m.phone.clone()),
        email: guest.email.or_else(|| m.email.clone()),
        date_of_birth: guest.date_of_birth.or(m.date_of_birth),
        gender: guest.gender.or_else(|| m.gender.clone()),
        nationality: guest.nationality.or_else(|| m.nationality.clone()),
    };
    Room {
        guest_last_name: last.unwrap_or_default(),
        guest_first_name: first.unwrap_or_default(),
        guest: Some(resolved),
        ..room
    }
}

#[cfg(test)]
#[path = "guest_tests.rs"]
mod tests;
