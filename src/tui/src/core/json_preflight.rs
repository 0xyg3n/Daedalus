//! Strict JSON preflight with a caller-owned cumulative work/deadline policy.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use std::{collections::HashSet, fmt};
pub(crate) const INVALID: &str = "Invalid or ambiguous JSON";
type Result<T> = std::result::Result<T, String>;
struct State<'a> {
    consume: &'a mut dyn FnMut() -> Result<()>,
    failure: Option<String>,
}
impl State<'_> {
    fn step<E: de::Error>(&mut self) -> std::result::Result<(), E> {
        (self.consume)().map_err(|error| {
            self.failure = Some(error);
            E::custom(INVALID)
        })
    }
}
pub(crate) fn check(bytes: &[u8], mut consume: impl FnMut() -> Result<()>) -> Result<()> {
    let mut state = State {
        consume: &mut consume,
        failure: None,
    };
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    if Seed(&mut state)
        .deserialize(&mut decoder)
        .and_then(|()| decoder.end())
        .is_err()
    {
        return Err(state.failure.unwrap_or_else(|| INVALID.into()));
    }
    Ok(())
}
struct Seed<'a, 'b>(&'a mut State<'b>);
impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(
        self,
        decoder: D,
    ) -> std::result::Result<(), D::Error> {
        self.0.step()?;
        decoder.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("bounded JSON")
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> std::result::Result<(), E> {
        Ok(())
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> std::result::Result<(), A::Error> {
        while sequence.next_element_seed(Seed(&mut *self.0))?.is_some() {}
        Ok(())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        let mut keys = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            self.0.step()?;
            if !keys.insert(key) {
                self.0.failure = Some(INVALID.into());
                return Err(de::Error::custom(INVALID));
            }
            map.next_value_seed(Seed(&mut *self.0))?;
        }
        Ok(())
    }
}
