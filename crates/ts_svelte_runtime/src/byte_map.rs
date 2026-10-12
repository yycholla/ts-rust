//! Byte maps in the JSON protocol. Each value is either an array of byte
//! numbers or a standard (padded) base64 string. Large snapshots should use
//! base64: a number array costs about 3.6 JSON bytes per byte.
use base64::Engine as _;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(untagged)]
enum Bytes {
    Array(Vec<u8>),
    Base64(String),
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<u8>>, D::Error> {
    BTreeMap::<String, Bytes>::deserialize(deserializer)?
        .into_iter()
        .map(|(file, bytes)| {
            let bytes = match bytes {
                Bytes::Array(bytes) => bytes,
                Bytes::Base64(text) => base64::engine::general_purpose::STANDARD
                    .decode(text)
                    .map_err(|e| {
                        serde::de::Error::custom(format!("{file}: invalid base64: {e}"))
                    })?,
            };
            Ok((file, bytes))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[derive(serde::Deserialize)]
    struct Holder {
        #[serde(deserialize_with = "super::deserialize")]
        files: std::collections::BTreeMap<String, Vec<u8>>,
    }
    #[test]
    fn arrays_and_base64_decode_to_the_same_bytes() {
        let holder: Holder =
            serde_json::from_str(r#"{"files":{"a":[0,255,1],"b":"AP8B"}}"#).unwrap();
        assert_eq!(holder.files["a"], holder.files["b"]);
        assert!(serde_json::from_str::<Holder>(r#"{"files":{"c":"not base64!"}}"#).is_err());
        assert!(serde_json::from_str::<Holder>(r#"{"files":{"d":[256]}}"#).is_err());
    }
}
