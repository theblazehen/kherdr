use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use std::fmt;
// Deserialize recursively, rejecting duplicate keys at EVERY depth before typed
// validation. serde_json additionally enforces JSON syntax, UTF-8 and depth bounds.
pub(crate) struct Unique(pub(crate) Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Strict;
        impl<'de> Visitor<'de> for Strict {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result { f.write_str("strict JSON") }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> { Ok(Unique(Value::Bool(v))) }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> { Ok(Unique(Value::Number(v.into()))) }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> { Ok(Unique(Value::Number(v.into()))) }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Number::from_f64(v).map(|n| Unique(Value::Number(n))).ok_or_else(|| E::custom("Nonfinite number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> { self.visit_string(v.into()) }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Unique, E> {
                if v.contains('\0') { return Err(E::custom("NUL in JSON string")); }
                Ok(Unique(Value::String(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> { Ok(Unique(Value::Null)) }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element::<Unique>()? { values.push(v.0); }
                Ok(Unique(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Unique, A::Error> {
                let mut values = Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if key.contains('\0') || values.contains_key(&key) { return Err(de::Error::custom("Duplicate or NUL object member")); }
                    values.insert(key, a.next_value::<Unique>()?.0);
                }
                Ok(Unique(Value::Object(values)))
            }
        }
        d.deserialize_any(Strict)
    }
}
