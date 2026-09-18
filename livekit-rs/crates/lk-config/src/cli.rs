//! One CLI flag per config field, generated from the schema.
//!
//! The Go server generates a flag for every YAML field by walking the config
//! struct with `reflect` (`config.GenerateCLIFlags`), so an operator can
//! override any setting without a config file. A hand-maintained subset would
//! drift, so the same walk runs here over [`StructSchema::scalar_paths`].
//!
//! Flag names are the dotted YAML path (`--rtc.tcp_port`) and each flag also
//! reads `LIVEKIT_` + the path upper-cased with dots as underscores
//! (`LIVEKIT_RTC_TCP_PORT`), matching the Go env var names exactly.

use clap::{Arg, ArgAction, ArgMatches, Command, parser::ValueSource};
use serde_yaml::{Mapping, Value};

use crate::duration::GoDuration;
use crate::error::{Error, Result};
use crate::schema::{Kind, StructSchema};

/// The usage string the Go server gives every generated flag.
pub const GENERATED_FLAG_USAGE: &str = "generated";

/// A generated flag: its dotted YAML path, its wire kind, and its env var.
#[derive(Clone, Debug)]
pub struct FlagSpec {
    /// Dotted YAML path, used verbatim as the flag's long name.
    pub path: String,
    /// The kind the raw string value is parsed as.
    pub kind: Kind,
    /// The `LIVEKIT_`-prefixed environment variable.
    pub env: String,
}

/// Every flag the config tree generates, minus the names already taken by
/// hand-written flags. Mirrors `GenerateCLIFlags(existingFlags, hidden)`.
#[must_use]
pub fn generated_flags(schema: &'static StructSchema, existing: &[&str]) -> Vec<FlagSpec> {
    schema
        .scalar_paths()
        .into_iter()
        .filter(|(path, _)| !existing.contains(&path.as_str()))
        .map(|(path, kind)| {
            let env = format!("LIVEKIT_{}", path.replace('.', "_").to_uppercase());
            FlagSpec { path, kind, env }
        })
        .collect()
}

/// Adds the generated flags to a [`Command`].
///
/// `hidden` keeps them out of `--help` while leaving them usable, which is what
/// the Go server does outside `help-verbose`.
#[must_use]
pub fn augment_command(mut command: Command, flags: &[FlagSpec], hidden: bool) -> Command {
    for flag in flags {
        let mut arg = Arg::new(flag.path.clone())
            .long(flag.path.clone())
            .env(flag.env.clone())
            .help(GENERATED_FLAG_USAGE)
            .hide(hidden);
        arg = if flag.kind == Kind::Bool {
            arg.action(ArgAction::Set)
                .num_args(0..=1)
                .default_missing_value("true")
        } else {
            arg.action(ArgAction::Set).num_args(1)
        };
        command = command.arg(arg);
    }
    command
}

/// The overrides the operator actually supplied, as raw strings.
///
/// Values that came from a flag's default are skipped, so a config file value
/// is not silently replaced by a flag the operator never passed. This is the
/// `c.IsSet(flagName)` guard in `updateFromCLI`.
#[must_use]
pub fn overrides_from_matches(matches: &ArgMatches, flags: &[FlagSpec]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for flag in flags {
        let source = matches.value_source(&flag.path);
        if source.is_none() || source == Some(ValueSource::DefaultValue) {
            continue;
        }
        if let Some(raw) = matches.get_one::<String>(&flag.path) {
            out.push((flag.path.clone(), raw.clone()));
        }
    }
    out
}

/// Applies dotted-path overrides onto a parsed YAML document, creating the
/// intermediate mappings a path needs.
///
/// Overrides are applied to the document rather than to the deserialised
/// struct, so one code path (serde) owns every conversion and a flag cannot
/// bypass a field's validation.
///
/// # Errors
///
/// Returns [`Error::Cli`] when a value does not parse as the field's kind, and
/// when a path runs through a key that is not a mapping.
pub fn apply_overrides(
    document: &mut Value,
    schema: &'static StructSchema,
    overrides: &[(String, String)],
) -> Result<()> {
    for (path, raw) in overrides {
        let kind = kind_at(schema, path)
            .ok_or_else(|| Error::Cli(format!("unknown config path {path:?}")))?;
        let parsed = parse_scalar(kind, raw)
            .map_err(|message| Error::Cli(format!("flag --{path}: {message}")))?;
        insert_at(document, path, parsed)?;
    }
    Ok(())
}

/// Resolves a dotted path to the kind of the field it names.
#[must_use]
pub fn kind_at(schema: &'static StructSchema, path: &str) -> Option<Kind> {
    let mut current = schema;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        let (field, _) = current.lookup(segment)?;
        if segments.peek().is_none() {
            return Some(field.kind);
        }
        match field.kind {
            Kind::Nested(inner) => current = inner(),
            _ => return None,
        }
    }
    None
}

fn parse_scalar(kind: Kind, raw: &str) -> std::result::Result<Value, String> {
    match kind {
        Kind::Bool => raw
            .parse::<bool>()
            .map(Value::from)
            .map_err(|_| format!("{raw:?} is not a boolean")),
        Kind::Int => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("{raw:?} is not an integer")),
        Kind::Uint => raw
            .parse::<u64>()
            .map(Value::from)
            .map_err(|_| format!("{raw:?} is not an unsigned integer")),
        Kind::Float => raw
            .parse::<f64>()
            .map(Value::from)
            .map_err(|_| format!("{raw:?} is not a number")),
        Kind::Str => Ok(Value::from(raw)),
        Kind::Duration => {
            // Go's generated flag for a duration is an Int64Flag carrying
            // nanoseconds; a bare integer is accepted for that reason, and the
            // duration string for everyone else.
            if let Ok(nanos) = raw.parse::<u64>() {
                Ok(Value::from(
                    GoDuration(std::time::Duration::from_nanos(nanos)).to_go_string(),
                ))
            } else {
                GoDuration::parse(raw).map(|d| Value::from(d.to_go_string()))
            }
        }
        Kind::Seq(_) | Kind::Map(_) | Kind::Nested(_) | Kind::Opaque => {
            Err("only scalar fields can be set from the command line".to_owned())
        }
    }
}

fn insert_at(document: &mut Value, path: &str, leaf: Value) -> Result<()> {
    if !document.is_mapping() {
        *document = Value::Mapping(Mapping::new());
    }
    let mut current = document;
    let mut segments = path.split('.').peekable();
    while let Some(segment) = segments.next() {
        let key = Value::from(segment);
        let mapping = current
            .as_mapping_mut()
            .ok_or_else(|| Error::Cli(format!("{path:?} runs through a non-mapping value")))?;
        if segments.peek().is_none() {
            mapping.insert(key, leaf);
            return Ok(());
        }
        let entry = mapping
            .entry(key)
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !entry.is_mapping() {
            *entry = Value::Mapping(Mapping::new());
        }
        current = entry;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Config;
    use crate::schema::HasSchema;

    #[test]
    fn generates_a_flag_per_scalar_field() {
        let flags = generated_flags(Config::schema(), &[]);
        let names: Vec<&str> = flags.iter().map(|f| f.path.as_str()).collect();
        assert!(names.contains(&"port"));
        assert!(names.contains(&"rtc.tcp_port"));
        assert!(names.contains(&"rtc.congestion_control.enabled"));
        assert!(names.contains(&"room.empty_timeout"));
        assert!(names.contains(&"limit.max_metadata_size"));
        // sequences, maps and structs get no flag of their own, as in Go
        assert!(!names.contains(&"keys"));
        assert!(!names.contains(&"rtc.turn_servers"));
        assert!(!names.contains(&"rtc"));
    }

    #[test]
    fn env_var_names_match_go() {
        let flags = generated_flags(Config::schema(), &[]);
        let flag = flags.iter().find(|f| f.path == "rtc.tcp_port").unwrap();
        assert_eq!(flag.env, "LIVEKIT_RTC_TCP_PORT");
    }

    #[test]
    fn existing_flag_names_are_not_regenerated() {
        let flags = generated_flags(Config::schema(), &["region"]);
        assert!(!flags.iter().any(|f| f.path == "region"));
    }

    #[test]
    fn overrides_create_intermediate_mappings() {
        let mut document: Value = serde_yaml::from_str("port: 7880").unwrap();
        apply_overrides(
            &mut document,
            Config::schema(),
            &[
                ("rtc.tcp_port".to_owned(), "7000".to_owned()),
                ("room.create_room_timeout".to_owned(), "15s".to_owned()),
            ],
        )
        .unwrap();
        let config: Config = serde_yaml::from_value(document).unwrap();
        assert_eq!(config.rtc.base.tcp_port, 7000);
        assert_eq!(config.room.create_room_timeout, GoDuration::from_secs(15));
    }

    #[test]
    fn duration_flags_accept_nanoseconds_like_go() {
        let mut document = Value::Mapping(Mapping::new());
        apply_overrides(
            &mut document,
            Config::schema(),
            &[(
                "room.create_room_timeout".to_owned(),
                "1500000000".to_owned(),
            )],
        )
        .unwrap();
        let config: Config = serde_yaml::from_value(document).unwrap();
        assert_eq!(
            config.room.create_room_timeout,
            GoDuration::from_millis(1500)
        );
    }
}
