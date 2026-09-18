//! Go `time.Duration` on the YAML wire.
//!
//! Every duration in `config-sample.yaml` is written the way Go's
//! `time.Duration` marshals: `500ms`, `7.5s`, `2m`, `1h30m`. A Rust
//! `std::time::Duration` serialises as a `{secs, nanos}` map, which would break
//! every existing config file, so this newtype carries the Go format on the
//! wire and a plain `Duration` in memory.
//!
//! Integers are also accepted on input and read as nanoseconds, because the Go
//! CLI flag for a duration field is an `Int64Flag` carrying nanoseconds.

use std::fmt;
use std::time::Duration;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A [`Duration`] that reads and writes Go's `time.Duration` string format.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub struct GoDuration(pub Duration);

impl GoDuration {
    /// A zero duration, the value Go's `omitempty` leaves out of the YAML.
    pub const ZERO: Self = Self(Duration::ZERO);

    /// Builds a duration from whole milliseconds.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(Duration::from_millis(millis))
    }

    /// Builds a duration from whole seconds.
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(Duration::from_secs(secs))
    }

    /// The wrapped [`Duration`].
    #[must_use]
    pub const fn get(self) -> Duration {
        self.0
    }

    /// True when the duration is zero, i.e. "unset" in Go's `omitempty` sense.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    /// Parses Go's `time.ParseDuration` syntax: a sequence of decimal numbers,
    /// each with an optional fraction and a unit suffix, such as `300ms`,
    /// `1.5h` or `2h45m`.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending input when the string is empty,
    /// carries an unknown unit, or is not a number.
    pub fn parse(input: &str) -> Result<Self, String> {
        let s = input.trim();
        if s.is_empty() {
            return Err("empty duration".to_owned());
        }
        if s == "0" {
            return Ok(Self::ZERO);
        }

        let mut total = 0f64;
        let mut rest = s;
        let mut saw_unit = false;
        while !rest.is_empty() {
            let digits_end = rest
                .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-' && c != '+')
                .unwrap_or(rest.len());
            let (number, tail) = rest.split_at(digits_end);
            if number.is_empty() {
                return Err(format!("invalid duration {input:?}"));
            }
            let unit_end = tail
                .find(|c: char| c.is_ascii_digit())
                .unwrap_or(tail.len());
            let (unit, next) = tail.split_at(unit_end);
            let scale = match unit {
                "ns" => 1e-9,
                "us" | "\u{b5}s" => 1e-6,
                "ms" => 1e-3,
                "s" => 1.0,
                "m" => 60.0,
                "h" => 3600.0,
                _ => return Err(format!("unknown unit {unit:?} in duration {input:?}")),
            };
            let value: f64 = number
                .parse()
                .map_err(|_| format!("invalid duration {input:?}"))?;
            total += value * scale;
            saw_unit = true;
            rest = next;
        }
        if !saw_unit {
            return Err(format!("missing unit in duration {input:?}"));
        }
        if total < 0.0 {
            return Err(format!("negative duration {input:?}"));
        }
        Ok(Self(Duration::from_secs_f64(total)))
    }

    /// Formats the duration the way Go's `time.Duration.String` does, so a
    /// config written by this server is byte-identical to one written by the Go
    /// server for the same value.
    #[must_use]
    pub fn to_go_string(self) -> String {
        let nanos = self.0.as_nanos();
        if nanos == 0 {
            return "0s".to_owned();
        }
        if nanos < 1_000 {
            return format!("{nanos}ns");
        }
        if nanos < 1_000_000 {
            return format!("{}\u{b5}s", trim_float(nanos as f64 / 1e3));
        }
        if nanos < 1_000_000_000 {
            return format!("{}ms", trim_float(nanos as f64 / 1e6));
        }

        let total_secs = self.0.as_secs();
        let frac_nanos = self.0.subsec_nanos();
        let hours = total_secs / 3600;
        let minutes = (total_secs % 3600) / 60;
        let seconds = (total_secs % 60) as f64 + f64::from(frac_nanos) / 1e9;

        let mut out = String::new();
        if hours > 0 {
            out.push_str(&format!("{hours}h"));
        }
        if hours > 0 || minutes > 0 {
            out.push_str(&format!("{minutes}m"));
        }
        out.push_str(&format!("{}s", trim_float(seconds)));
        out
    }
}

fn trim_float(value: f64) -> String {
    let mut s = format!("{value:.9}");
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

impl fmt::Debug for GoDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_go_string())
    }
}

impl fmt::Display for GoDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_go_string())
    }
}

impl From<Duration> for GoDuration {
    fn from(value: Duration) -> Self {
        Self(value)
    }
}

impl From<GoDuration> for Duration {
    fn from(value: GoDuration) -> Self {
        value.0
    }
}

impl Serialize for GoDuration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_go_string())
    }
}

impl<'de> Deserialize<'de> for GoDuration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(GoDurationVisitor)
    }
}

struct GoDurationVisitor;

impl Visitor<'_> for GoDurationVisitor {
    type Value = GoDuration;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Go duration string such as \"500ms\", or nanoseconds as an integer")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        GoDuration::parse(v).map_err(E::custom)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        Ok(GoDuration(Duration::from_nanos(v)))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        if v < 0 {
            return Err(E::custom(format!("negative duration {v}")));
        }
        Ok(GoDuration(Duration::from_nanos(v as u64)))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        if v < 0.0 {
            return Err(E::custom(format!("negative duration {v}")));
        }
        Ok(GoDuration(Duration::from_nanos(v as u64)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parses_go_duration_syntax() {
        assert_eq!(GoDuration::parse("0").unwrap(), GoDuration::ZERO);
        assert_eq!(GoDuration::parse("500ms").unwrap().get().as_millis(), 500);
        assert_eq!(GoDuration::parse("7.5s").unwrap().get().as_millis(), 7500);
        assert_eq!(GoDuration::parse("2m").unwrap().get().as_secs(), 120);
        assert_eq!(GoDuration::parse("1h30m").unwrap().get().as_secs(), 5400);
        assert!(GoDuration::parse("5 parsecs").is_err());
        assert!(GoDuration::parse("5").is_err());
    }

    #[test]
    fn formats_like_go() {
        assert_eq!(GoDuration::ZERO.to_go_string(), "0s");
        assert_eq!(GoDuration::from_millis(500).to_go_string(), "500ms");
        assert_eq!(GoDuration::from_secs(1).to_go_string(), "1s");
        assert_eq!(GoDuration::from_secs(120).to_go_string(), "2m0s");
        assert_eq!(GoDuration::from_secs(5400).to_go_string(), "1h30m0s");
        assert_eq!(GoDuration::from_millis(7500).to_go_string(), "7.5s");
    }

    #[test]
    fn round_trips_through_yaml() {
        for text in ["500ms", "7.5s", "2m0s", "1h30m0s", "0s"] {
            let parsed = GoDuration::parse(text).unwrap();
            assert_eq!(parsed.to_go_string(), text);
            let yaml = serde_yaml::to_string(&parsed).unwrap();
            let back: GoDuration = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(back, parsed);
        }
    }
}
