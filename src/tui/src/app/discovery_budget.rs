//! Shared cooperative deadline and metadata admission for one discovery pass.
//! Counts accepted wire/normalized JSON, including nested values and map keys.
use super::runtime_settings::Settings;
use serde::{
    Serialize,
    de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use std::{
    fmt, io,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const CAPACITY: &str = "Discovery metadata limit reached; review Terminals and discovery settings or narrow discovery scope";
const TIME: &str = "Discovery refresh deadline reached; review Terminals and discovery settings or narrow discovery scope";
#[derive(Debug)]
struct Remaining {
    bytes: usize,
    values: usize,
}
#[derive(Debug)]
pub struct Budget {
    pub deadline: Instant,
    pub byte_limit: usize,
    remaining: Mutex<Remaining>,
    capacity: AtomicBool,
    timed_out: AtomicBool,
}
impl Budget {
    pub fn new(settings: &Settings) -> Result<Self, String> {
        settings.validate()?;
        Ok(Self {
            deadline: Instant::now() + Duration::from_millis(settings.discovery_timeout_ms),
            byte_limit: settings.discovery_metadata_limit_bytes as usize,
            // Reserve the fixed root and at most two final incompleteness records.
            remaining: Mutex::new(Remaining {
                bytes: (settings.discovery_metadata_limit_bytes as usize).saturating_sub(1024),
                values: (settings.discovery_metadata_value_limit as usize).saturating_sub(128),
            }),
            capacity: AtomicBool::new(false),
            timed_out: AtomicBool::new(false),
        })
    }
    pub fn time_left(&self) -> Result<Duration, String> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            self.timed_out.store(true, Ordering::Relaxed);
            Err(TIME.into())
        } else {
            Ok(left)
        }
    }
    pub fn remaining_bytes(&self) -> usize {
        self.remaining.lock().expect("discovery budget lock").bytes
    }
    pub fn capacity_error(&self) -> String {
        self.capacity.store(true, Ordering::Relaxed);
        CAPACITY.into()
    }
    fn consume(&self, bytes: usize, values: usize) -> Result<(), String> {
        self.time_left()?;
        let mut remaining = self.remaining.lock().expect("discovery budget lock");
        if bytes > remaining.bytes || values > remaining.values {
            return Err(self.capacity_error());
        }
        remaining.bytes -= bytes;
        remaining.values -= values;
        Ok(())
    }
    /// Charge plain tmux text before allocating its normalized records.
    pub fn admit_text(&self, bytes: usize, values: usize) -> Result<(), String> {
        self.consume(bytes, values)
    }
    /// Preflight before serde_json::Value allocation. Strings are discarded by
    /// the visitor; escaped string scratch space remains bounded by wire bytes.
    pub fn admit_json(&self, bytes: &[u8]) -> Result<(), String> {
        self.time_left()?;
        let limit = self.remaining.lock().expect("discovery budget lock").values;
        let mut count = Count {
            budget: self,
            count: 0,
            limit,
        };
        let mut decoder = serde_json::Deserializer::from_slice(bytes);
        Seed(&mut count)
            .deserialize(&mut decoder)
            .and_then(|()| decoder.end())
            .map_err(|_| -> String {
                if self.time_left().is_err() {
                    TIME.into()
                } else if count.count > count.limit {
                    CAPACITY.into()
                } else {
                    "Malformed terminal discovery JSON".into()
                }
            })?;
        self.consume(bytes.len(), count.count)
    }
    pub fn admit_value(&self, value: &Value) -> Result<(), String> {
        let bytes = json_size(value, self.remaining_bytes()).map_err(|_| self.capacity_error())?;
        // The same no-allocation visitor counts in-memory Values, including keys.
        let limit = self.remaining.lock().expect("discovery budget lock").values;
        let mut count = Count {
            budget: self,
            count: 0,
            limit,
        };
        Seed(&mut count).deserialize(value).map_err(|_| {
            if self.time_left().is_err() {
                TIME.into()
            } else {
                self.capacity_error()
            }
        })?;
        // Account for an array separator when a value loses its old wrapper.
        self.consume(bytes + 1, count.count)
    }
    pub fn error(&self, snapshot: &mut Value, error: Value) {
        if self.admit_value(&error).is_ok() {
            snapshot["errors"]
                .as_array_mut()
                .expect("discovery errors")
                .push(error);
        }
    }
    pub fn finish(&self, snapshot: &mut Value) {
        let _ = self.time_left();
        if self.capacity.load(Ordering::Relaxed)
            && !snapshot["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["backend"] == "discovery" && row["error"] == CAPACITY)
        {
            snapshot["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!({"backend":"discovery","error":CAPACITY}));
        }
        if self.timed_out.load(Ordering::Relaxed)
            && !snapshot["errors"]
                .as_array()
                .unwrap()
                .iter()
                .any(|row| row["backend"] == "discovery" && row["error"] == TIME)
        {
            snapshot["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!({"backend":"discovery","error":TIME}));
        }
    }
}

struct Count<'a> {
    budget: &'a Budget,
    count: usize,
    limit: usize,
}
impl Count<'_> {
    fn next<E: de::Error>(&mut self) -> Result<(), E> {
        self.budget.time_left().map_err(E::custom)?;
        self.count += 1;
        if self.count > self.limit {
            return Err(E::custom(self.budget.capacity_error()));
        }
        Ok(())
    }
}
struct Seed<'a, 'b>(&'a mut Count<'b>);
impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<(), D::Error> {
        self.0.next()?;
        decoder.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded discovery metadata")
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<(), A::Error> {
        while sequence.next_element_seed(Seed(&mut *self.0))?.is_some() {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while map.next_key::<IgnoredAny>()?.is_some() {
            self.0.next()?;
            map.next_value_seed(Seed(&mut *self.0))?;
        }
        Ok(())
    }
}
pub(crate) fn json_size(value: &impl Serialize, limit: usize) -> Result<usize, String> {
    struct Counter {
        bytes: usize,
        limit: usize,
    }
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes) {
                return Err(io::Error::other("Discovery serialization exceeds limit"));
            }
            self.bytes += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter { bytes: 0, limit };
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| "Discovery serialization exceeds limit")?;
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_and_normalized_values_share_a_nested_budget_without_echoing_content() {
        let settings = Settings::default();
        let budget = Budget::new(&settings).unwrap();
        let value = json!({"a":[null,true,1,-2,1.5,"å\n"],"b":{}});
        let bytes = serde_json::to_vec(&value).unwrap();
        budget.admit_json(&bytes).unwrap();
        budget.admit_value(&value).unwrap();
        let remaining = budget.remaining.lock().unwrap();
        assert_eq!(
            remaining.bytes,
            settings.discovery_metadata_limit_bytes as usize - 1024 - 2 * bytes.len() - 1
        );
        assert_eq!(
            remaining.values,
            settings.discovery_metadata_value_limit as usize - 128 - 22
        );
        drop(remaining);
        let settings = settings
            .set("discovery_metadata_value_limit", "256")
            .unwrap();
        let budget = Budget::new(&settings).unwrap();
        let tiny = serde_json::to_vec(&vec![0; 200]).unwrap();
        assert!(
            budget
                .admit_json(&tiny)
                .unwrap_err()
                .contains("metadata limit")
        );
        let error = budget
            .admit_json(br#"{"NEVER_EXPORT_CONTENT":invalid}"#)
            .unwrap_err();
        assert_eq!(error, "Malformed terminal discovery JSON");
        assert!(!error.contains("NEVER_EXPORT_CONTENT"));
        let mut snapshot = json!({"errors":[]});
        budget.finish(&mut snapshot);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn concurrent_admission_cannot_overcommit_bytes_and_small_later_records_fit() {
        let settings = Settings::default()
            .set("discovery_metadata_limit_bytes", "4096")
            .unwrap();
        let budget = Budget::new(&settings).unwrap();
        let packet = serde_json::to_vec(&json!({"text":"x".repeat(500)})).unwrap();
        let admitted = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                scope.spawn(|| {
                    if budget.admit_json(&packet).is_ok() {
                        admitted.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        let count = admitted.load(Ordering::SeqCst);
        assert_eq!(count, (4096 - 1024) / packet.len());
        assert_eq!(budget.remaining_bytes(), 4096 - 1024 - count * packet.len());
        budget.admit_json(b"null").unwrap();
        let mut snapshot = json!({"errors":[]});
        budget.finish(&mut snapshot);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
        assert!(json_size(&snapshot, 4096).is_ok());
    }
    #[test]
    fn expired_budget_refuses_metadata_without_charging_or_marking_capacity() {
        let settings = Settings::default();
        let mut budget = Budget::new(&settings).unwrap();
        budget.deadline = Instant::now() - Duration::from_millis(1);
        assert!(budget.admit_json(b"{}").unwrap_err().contains("deadline"));
        assert!(
            budget
                .admit_value(&json!({}))
                .unwrap_err()
                .contains("deadline")
        );
        assert_eq!(
            budget.remaining_bytes(),
            settings.discovery_metadata_limit_bytes as usize - 1024
        );
        let mut snapshot = json!({"errors":[]});
        budget.finish(&mut snapshot);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 1);
        assert!(!budget.capacity.load(Ordering::Relaxed));
        let invalid = Settings {
            discovery_timeout_ms: u64::MAX,
            ..Settings::default()
        };
        assert!(Budget::new(&invalid).is_err());
    }
    #[test]
    fn minimum_budget_reserves_space_for_root_and_both_final_warnings() {
        let settings = Settings::default()
            .set("discovery_metadata_limit_bytes", "4096")
            .unwrap()
            .set("discovery_metadata_value_limit", "256")
            .unwrap();
        let mut budget = Budget::new(&settings).unwrap();
        let row = json!({"text":"x".repeat(3000)});
        budget.admit_value(&row).unwrap();
        assert!(budget.admit_value(&json!("x".repeat(100))).is_err());
        budget.deadline = Instant::now() - Duration::from_millis(1);
        let mut snapshot = json!({"panes":[row],"clients":[],"workspaces":[],"tabs":[],"layouts":[],"herdr_sessions":[],"tmux_sessions":[],"herdr_focus":[],"errors":[],"unavailable":[],"stale_sockets":[],"external_terminals":[],"external_errors":[],"observed_at":1234567890.123456});
        budget.finish(&mut snapshot);
        assert_eq!(snapshot["errors"].as_array().unwrap().len(), 2);
        assert!(json_size(&snapshot, 4096).is_ok());
        // Count the final actual snapshot, including root and warning keys.
        let fresh = Budget::new(&settings).unwrap();
        let mut count = Count {
            budget: &fresh,
            count: 0,
            limit: 256,
        };
        Seed(&mut count).deserialize(&snapshot).unwrap();
    }
}
