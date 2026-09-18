//! Strict-mode parsing: the Rust half of `yaml.Decoder.KnownFields(true)`.
//!
//! The Go server rejects unknown YAML keys unless `--disable-strict-config` is
//! passed. `serde`'s `deny_unknown_fields` is a compile-time attribute and
//! cannot be switched off at run time, so the check runs as a separate pass
//! over the parsed document against the [`StructSchema`] tree. The upside over
//! the serde attribute is the error message: a full dotted path per offending
//! key, all of them at once rather than the first.

use serde_yaml::Value;

use crate::schema::{Kind, StructSchema};

/// Collects the dotted paths of every key in `value` that the schema does not
/// declare. An empty result means the document is strictly valid.
#[must_use]
pub fn unknown_fields(value: &Value, schema: &'static StructSchema) -> Vec<String> {
    let mut out = Vec::new();
    walk(value, schema, "", &mut out);
    out
}

fn walk(value: &Value, schema: &'static StructSchema, prefix: &str, out: &mut Vec<String>) {
    let Some(mapping) = value.as_mapping() else {
        return;
    };
    for (key, child) in mapping {
        let Some(key) = key.as_str() else {
            continue;
        };
        let path = if prefix.is_empty() {
            key.to_owned()
        } else {
            format!("{prefix}.{key}")
        };
        let Some((field, _)) = schema.lookup(key) else {
            out.push(path);
            continue;
        };
        match field.kind {
            Kind::Nested(inner) => walk(child, inner(), &path, out),
            Kind::Seq(Some(inner)) => {
                if let Some(items) = child.as_sequence() {
                    for (index, item) in items.iter().enumerate() {
                        walk(item, inner(), &format!("{path}[{index}]"), out);
                    }
                }
            }
            Kind::Map(Some(inner)) => {
                if let Some(entries) = child.as_mapping() {
                    for (name, item) in entries {
                        let name = name.as_str().unwrap_or("?");
                        walk(item, inner(), &format!("{path}.{name}"), out);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::schema::HasSchema;

    #[test]
    fn reports_unknown_keys_with_paths() {
        let yaml = r"
port: 7880
rtc:
  tcp_port: 7881
  tcp_prot: 7881
rooom:
  auto_create: true
";
        let value: Value = serde_yaml::from_str(yaml).unwrap();
        let unknown = unknown_fields(&value, Config::schema());
        assert_eq!(unknown, vec!["rtc.tcp_prot".to_owned(), "rooom".to_owned()]);
    }

    #[test]
    fn walks_into_sequences_and_maps() {
        let yaml = r"
rtc:
  turn_servers:
    - host: turn.example.com
      protocal: tls
node_selector:
  regions:
    - name: us-west
      latitude: 37.0
";
        let value: Value = serde_yaml::from_str(yaml).unwrap();
        let unknown = unknown_fields(&value, Config::schema());
        assert_eq!(
            unknown,
            vec![
                "rtc.turn_servers[0].protocal".to_owned(),
                "node_selector.regions[0].latitude".to_owned(),
            ]
        );
    }

    #[test]
    fn accepts_inlined_fields_in_the_parent() {
        // `rtc.tcp_port` lives on the inlined rtcconfig struct, and
        // `audio.active_level` on the inlined audio-level struct.
        let yaml = r"
rtc:
  tcp_port: 7881
audio:
  active_level: 30
";
        let value: Value = serde_yaml::from_str(yaml).unwrap();
        assert!(unknown_fields(&value, Config::schema()).is_empty());
    }
}
