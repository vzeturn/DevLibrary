//! `Adapters/SimulatedLockAdapter.cs`.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use dl_json::{Value, json};
use dl_time::DateTime;

use super::{Cx, DeviceAdapter, SIMULATED_LOCK, adapter_failed};
use crate::contracts::{
    AdapterError, CancelAllRequest, EraseRequest, KeyCardRequest, ReadRequest, codes,
    format_wall_clock,
};

/// Deterministic in-memory lock encoder for development and tests (SFP-I1). Card numbers
/// are `SIM-<room>-<n>`, `n` counting per adapter. It keeps the lock's view of each room:
/// a new key (issue) invalidates the room's earlier cards, a copy (duplicate) adds a card
/// beside them, extend re-encodes with a new end time, and cancelAll invalidates every card
/// of the room. Read returns the last encoded card with its `valid` state. Never the
/// production default: it touches no hardware.
#[derive(Debug, Default)]
pub struct SimulatedLockAdapter {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    issued: HashMap<String, KeyCard>,
    live: HashSet<String>,
    next: i32,
    last_card: Option<String>,
}

#[derive(Debug, Clone)]
struct KeyCard {
    card_number: String,
    room: Option<String>,
    from: DateTime,
    to: DateTime,
}

impl KeyCard {
    fn to_json(&self) -> Value {
        json!({
            "cardNumber": self.card_number.as_str(),
            "room": room_value(&self.room),
            "from": format_wall_clock(self.from),
            "to": format_wall_clock(self.to),
        })
    }
}

/// Above this many cards in one request the simulator refuses (C# would try to allocate
/// them all; `Enumerable.Range` refuses a negative count with the same message).
const MAX_CARDS: i32 = 10_000;

fn room_value(room: &Option<String>) -> Value {
    room.as_deref().map_or(Value::Null, Value::from)
}

impl SimulatedLockAdapter {
    /// An encoder with no card issued yet.
    pub fn new() -> SimulatedLockAdapter {
        SimulatedLockAdapter::default()
    }

    /// True while the lock would still open for this card.
    pub fn is_valid(&self, card_number: &str) -> bool {
        self.state().live.contains(card_number)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn encode(
        &self,
        request: &KeyCardRequest,
        invalidate_room: bool,
    ) -> Result<Value, AdapterError> {
        let mut s = self.state();
        if invalidate_room {
            s.invalidate_room(&request.room);
        }
        if !(0..=MAX_CARDS).contains(&request.cards) {
            return Err(adapter_failed(
                "ArgumentOutOfRangeException",
                "Specified argument was out of the range of valid values. (Parameter 'count')",
            ));
        }
        let mut cards = Vec::new();
        for _ in 0..request.cards {
            s.next = s.next.wrapping_add(1);
            let card = KeyCard {
                card_number: format!("SIM-{}-{}", request.room.as_deref().unwrap_or(""), s.next),
                room: request.room.clone(),
                from: request.from,
                to: request.to,
            };
            s.live.insert(card.card_number.clone());
            s.issued.insert(card.card_number.clone(), card.clone());
            cards.push(card);
        }
        if let Some(last) = cards.last() {
            s.last_card = Some(last.card_number.clone());
        }
        Ok(json!({"cards": Value::Array(cards.iter().map(KeyCard::to_json).collect())}))
    }
}

impl State {
    fn invalidate_room(&mut self, room: &Option<String>) {
        let issued = &self.issued;
        self.live
            .retain(|n| issued.get(n).is_none_or(|c| &c.room != room));
    }
}

impl DeviceAdapter for SimulatedLockAdapter {
    fn family(&self) -> &str {
        SIMULATED_LOCK
    }

    fn issue(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.encode(request, true)
    }

    fn duplicate(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.encode(request, false)
    }

    fn extend(&self, request: &KeyCardRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.encode(request, false)
    }

    fn read(&self, _: &ReadRequest, _: &Cx) -> Result<Value, AdapterError> {
        let s = self.state();
        let card = s
            .last_card
            .as_ref()
            .and_then(|n| s.issued.get(n))
            .ok_or_else(|| AdapterError::new(codes::NO_CARD, "No card on the simulated encoder"))?;
        let mut v = card.to_json();
        if let Some(map) = v.as_object_mut() {
            map.insert("valid", s.live.contains(&card.card_number));
        }
        Ok(v)
    }

    fn cancel_all(&self, request: &CancelAllRequest, _: &Cx) -> Result<Value, AdapterError> {
        self.state().invalidate_room(&request.room);
        Ok(json!({"room": room_value(&request.room), "cancelled": true}))
    }

    fn erase(&self, request: &EraseRequest, _: &Cx) -> Result<Value, AdapterError> {
        // C# Dictionary.ContainsKey(null) throws, caught as an unexpected failure (kept).
        let Some(card_number) = request.card_number.as_deref() else {
            return Err(adapter_failed(
                "ArgumentNullException",
                "Value cannot be null. (Parameter 'key')",
            ));
        };
        let mut s = self.state();
        if !s.issued.contains_key(card_number) {
            return Err(AdapterError::new(
                codes::NO_CARD,
                format!("Card {card_number} is not on the simulated encoder"),
            ));
        }
        s.live.remove(card_number);
        Ok(json!({"cardNumber": card_number, "erased": true}))
    }
}
