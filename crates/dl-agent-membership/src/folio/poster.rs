//! `FolioObservationPoster`: posts observations in chunks, skips what the hub already
//! acknowledged unchanged, and halves the chunk on a timeout.

use std::collections::{HashMap, VecDeque};

use dl_decimal::Decimal;
use dl_hash::{Digest, Sha256};

use crate::hub::{HubApi, HubError};
use crate::settings::MAX_OBSERVATION_BATCH;
use crate::wire::FolioObservation;

/// The smallest chunk after timeouts.
pub const MIN_CHUNK: usize = 25;
/// How many acknowledged folios are remembered.
pub const ACK_CAPACITY: usize = 5000;

type Key = (Decimal, Decimal);

/// What one post did (`FolioObservationPostResult`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PostOutcome {
    /// Observations sent.
    pub sent: usize,
    /// Observations skipped as already acknowledged unchanged.
    pub skipped: usize,
    /// The hub's counts, summed over the chunks.
    pub accepted: i64,
    /// Observations that earned points.
    pub earned: i64,
    /// Observations that reversed points.
    pub reversed: i64,
    /// Observations that changed nothing.
    pub unchanged: i64,
}

/// The poster. One per scanner; not shared between threads.
#[derive(Debug)]
pub struct Poster {
    max_chunk: usize,
    chunk: usize,
    acked: HashMap<Key, String>,
    order: VecDeque<Key>,
}

impl Default for Poster {
    fn default() -> Poster {
        Poster::new(MAX_OBSERVATION_BATCH)
    }
}

impl Poster {
    /// A poster with chunks of at most `max_chunk` (at least [`MIN_CHUNK`]).
    pub fn new(max_chunk: usize) -> Poster {
        let max_chunk = max_chunk.max(MIN_CHUNK);
        Poster {
            max_chunk,
            chunk: max_chunk,
            acked: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// The current chunk size.
    pub fn chunk(&self) -> usize {
        self.chunk
    }

    /// Posts `observations`. A failure (after a timeout, with the chunk halved) is returned
    /// as is, so the caller does not move its cursor.
    pub fn post(
        &mut self,
        hub: &dyn HubApi,
        observations: &[FolioObservation],
        agent_version: &str,
        db_identity_fingerprint: Option<&str>,
    ) -> Result<PostOutcome, HubError> {
        let mut out = PostOutcome::default();
        let mut pending: Vec<(&FolioObservation, String)> = Vec::new();
        for o in observations {
            let hash = hash(o);
            if self.acked.get(&key(o)) == Some(&hash) {
                out.skipped += 1;
            } else {
                pending.push((o, hash));
            }
        }
        let mut start = 0;
        while start < pending.len() {
            let end = (start + self.chunk).min(pending.len());
            let chunk = &pending[start..end];
            let batch: Vec<FolioObservation> = chunk.iter().map(|(o, _)| (*o).clone()).collect();
            let result =
                match hub.post_folio_observations(&batch, agent_version, db_identity_fingerprint) {
                    Ok(r) => r,
                    Err(e) => {
                        if e.is_timeout() {
                            self.chunk = (chunk.len() / 2).max(MIN_CHUNK).min(self.max_chunk);
                        }
                        return Err(e);
                    }
                };
            out.sent += chunk.len();
            out.accepted += result.accepted;
            out.earned += result.earned;
            out.reversed += result.reversed;
            out.unchanged += result.unchanged;
            let hashes: HashMap<Key, &String> = chunk.iter().map(|(o, h)| (key(o), h)).collect();
            for item in &result.items {
                let k = (item.folio_num, item.folio_sub_num);
                if item.rejected {
                    self.acked.remove(&k);
                } else if let Some(h) = hashes.get(&k) {
                    if !self.acked.contains_key(&k) {
                        self.order.push_back(k);
                    }
                    self.acked.insert(k, (*h).clone());
                }
            }
            self.evict();
            start = end;
        }
        if !pending.is_empty() && self.chunk < self.max_chunk {
            self.chunk = (self.chunk * 2).min(self.max_chunk);
        }
        Ok(out)
    }

    fn evict(&mut self) {
        while self.acked.len() > ACK_CAPACITY {
            let Some(k) = self.order.pop_front() else {
                break;
            };
            self.acked.remove(&k);
        }
        if self.order.len() > ACK_CAPACITY * 2 {
            let mut seen = std::collections::HashSet::new();
            let acked = &self.acked;
            self.order
                .retain(|k| acked.contains_key(k) && seen.insert(*k));
        }
    }
}

fn key(o: &FolioObservation) -> Key {
    (o.folio_num, o.folio_sub_num)
}

/// Upper-case hex SHA-256 of the observation's JSON.
fn hash(o: &FolioObservation) -> String {
    dl_encoding::hex::encode_upper(Sha256::digest(o.to_j().to_json().as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use dl_time::{Date, OffsetDateTime};

    use super::*;
    use crate::wire::parse::{Job, ObservationItem, ObservationResult, SyncConfig};
    use crate::wire::{
        AvailabilityRequest, Heartbeat, RoomInventoryRequest, RoomTypeCatalogRequest,
        StayClassification,
    };

    #[derive(Default)]
    struct FakeHub {
        posts: Mutex<Vec<usize>>,
        timeout_above: Option<usize>,
        reject: Option<i64>,
    }

    impl HubApi for FakeHub {
        fn heartbeat(&self, _: &Heartbeat) -> Result<bool, HubError> {
            Ok(false)
        }
        fn sync_config(&self) -> Result<SyncConfig, HubError> {
            Ok(SyncConfig::default())
        }
        fn pull_jobs(&self, _: u32) -> Result<Vec<Job>, HubError> {
            Ok(Vec::new())
        }
        fn post_availability(&self, _: &AvailabilityRequest) -> Result<(), HubError> {
            Ok(())
        }
        fn post_room_inventory(&self, _: &RoomInventoryRequest) -> Result<(), HubError> {
            Ok(())
        }
        fn post_room_types(&self, _: &RoomTypeCatalogRequest) -> Result<(), HubError> {
            Ok(())
        }
        fn post_folio_observations(
            &self,
            obs: &[FolioObservation],
            _: &str,
            _: Option<&str>,
        ) -> Result<ObservationResult, HubError> {
            if self.timeout_above.is_some_and(|n| obs.len() > n) {
                return Err(HubError::Timeout("folio-observations"));
            }
            self.posts.lock().unwrap().push(obs.len());
            Ok(ObservationResult {
                accepted: obs.len() as i64,
                items: obs
                    .iter()
                    .map(|o| ObservationItem {
                        folio_num: o.folio_num,
                        folio_sub_num: o.folio_sub_num,
                        rejected: self.reject == o.folio_num.to_i64(),
                    })
                    .collect(),
                ..ObservationResult::default()
            })
        }
    }

    fn obs(n: i64) -> FolioObservation {
        FolioObservation {
            folio_num: Decimal::from(n),
            folio_sub_num: Decimal::ZERO,
            pms_folio_id: Some(n),
            folio_status: 1,
            checkout_epoch: 0,
            checked_out_at: None,
            checked_in_at: None,
            observed_at: OffsetDateTime::UNIX_EPOCH,
            arrival_date: Date::new(2026, 1, 1).unwrap(),
            departure_date: Date::new(2026, 1, 2).unwrap(),
            confirm_num: None,
            external_confirm_number: None,
            is_cancelled: false,
            cancelled_at: None,
            is_no_show: false,
            room_code: None,
            room_item_id_hint: None,
            guest_phone: None,
            guest_name: None,
            qualifying_revenue: Decimal::ZERO,
            revenue_fingerprint: String::new(),
            booked_room_type_code: None,
            actual_room_type_code: None,
            num_adult: None,
            num_child: None,
            classification: StayClassification::default(),
            num_enf: None,
            extra_bed: None,
            extra_bed_special_qty: None,
        }
    }

    #[test]
    fn acknowledged_unchanged_observations_are_skipped_but_rejected_ones_resent() {
        let hub = FakeHub {
            reject: Some(2),
            ..FakeHub::default()
        };
        let mut p = Poster::new(100);
        let all: Vec<_> = (1..=3).map(obs).collect();
        let first = p.post(&hub, &all, "v", None).unwrap();
        assert_eq!((first.sent, first.skipped, first.accepted), (3, 0, 3));
        let second = p.post(&hub, &all, "v", None).unwrap();
        assert_eq!(
            (second.sent, second.skipped),
            (1, 2),
            "folio 2 was rejected"
        );
        let mut changed = obs(1);
        changed.qualifying_revenue = Decimal::from(5);
        assert_eq!(p.post(&hub, &[changed], "v", None).unwrap().sent, 1);
    }

    #[test]
    fn timeouts_halve_the_chunk_and_success_grows_it_back() {
        let hub = FakeHub {
            timeout_above: Some(60),
            ..FakeHub::default()
        };
        let mut p = Poster::new(200);
        let all: Vec<_> = (1..=150).map(obs).collect();
        assert!(p.post(&hub, &all, "v", None).unwrap_err().is_timeout());
        assert_eq!(p.chunk(), 75);
        assert!(p.post(&hub, &all, "v", None).is_err());
        assert_eq!(p.chunk(), 37);
        let ok = p.post(&hub, &all, "v", None).unwrap();
        assert_eq!(ok.sent, 150);
        assert_eq!(p.chunk(), 74, "doubles after a clean post");
        assert!(hub.posts.lock().unwrap().iter().all(|n| *n <= 37));
    }
}
