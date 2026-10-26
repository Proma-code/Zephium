//! Bounded, duplicate-rejecting JSON. Errors deliberately contain no input.

use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 16_384;

pub(crate) fn parse(input: &[u8]) -> Result<Value, ()> {
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let mut remaining = MAX_NODES;
    let value = Seed {
        depth: 0,
        remaining: &mut remaining,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| ())?;
    deserializer.end().map_err(|_| ())?;
    Ok(value)
}

struct Seed<'a> {
    depth: usize,
    remaining: &'a mut usize,
}

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.depth > MAX_DEPTH || *self.remaining == 0 {
            return Err(D::Error::custom("JSON structure limit"));
        }
        *self.remaining -= 1;
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Seed<'_> {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("bounded JSON with unique object keys")
    }
    fn visit_bool<E: Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E: Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E: Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid number"))
    }
    fn visit_str<E: Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }
    fn visit_string<E: Error>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_none<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut result = Vec::new();
        while let Some(value) = sequence.next_element_seed(Seed {
            depth: self.depth + 1,
            remaining: self.remaining,
        })? {
            result.push(value);
        }
        Ok(Value::Array(result))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut result = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if result.contains_key(&key) {
                return Err(A::Error::custom("duplicate JSON key"));
            }
            let value = map.next_value_seed(Seed {
                depth: self.depth + 1,
                remaining: self.remaining,
            })?;
            result.insert(key, value);
        }
        Ok(Value::Object(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_rejected_even_in_ignored_nested_fields() {
        for input in [
            r#"{"a":1,"a":2}"#,
            r#"{"a":[{"b":1,"b":2}]}"#,
            r#"{"a":1,"\u0061":2}"#,
        ] {
            assert!(parse(input.as_bytes()).is_err());
        }
        assert!(parse(br#"{"a":1,"b":{"a":2}}"#).is_ok());
    }

    #[test]
    fn structure_and_trailing_input_are_bounded() {
        let nested = format!(
            "{}0{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert!(parse(nested.as_bytes()).is_err());
        let flat = format!(
            "[{}]",
            std::iter::repeat_n("0", MAX_NODES)
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(parse(flat.as_bytes()).is_err());
        for input in [b"{} {}".as_slice(), b"NaN", b"1e9999", b"\xff"] {
            assert!(parse(input).is_err());
        }
    }
}
