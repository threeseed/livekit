//! The compile-time description of the config tree.
//!
//! `#[derive(ConfigSchema)]` emits one [`StructSchema`] per config struct. Two
//! consumers walk it: [`crate::strict`], which rejects unknown YAML keys the
//! way Go's `yaml.Decoder.KnownFields(true)` does, and [`crate::cli`], which
//! generates one flag per scalar field the way `config.GenerateCLIFlags` does.

/// What a field looks like on the YAML wire.
///
/// The variants are exactly the `reflect.Kind` cases the Go flag generator
/// switches on, including its rule that sequences, maps and nested structs
/// produce no flag of their own.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    /// A YAML boolean.
    Bool,
    /// A signed integer.
    Int,
    /// An unsigned integer.
    Uint,
    /// A floating point number.
    Float,
    /// A string.
    Str,
    /// A Go duration string such as `500ms`.
    Duration,
    /// A sequence, carrying its element schema when the elements are structs.
    Seq(Option<fn() -> &'static StructSchema>),
    /// A mapping, carrying its value schema when the values are structs.
    Map(Option<fn() -> &'static StructSchema>),
    /// A nested struct.
    Nested(fn() -> &'static StructSchema),
    /// A leaf whose contents are not walked, such as a protobuf message.
    Opaque,
}

/// Two kinds are equal when they are the same variant. The schema functions
/// a `Seq`, `Map` or `Nested` carries are not compared: function pointers have
/// no meaningful identity across codegen units, and callers only ever ask which
/// shape a field has.
impl PartialEq for Kind {
    fn eq(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

impl Eq for Kind {}

impl Kind {
    /// Whether the Go flag generator emits a CLI flag for this kind.
    #[must_use]
    pub const fn is_flaggable(self) -> bool {
        matches!(
            self,
            Self::Bool | Self::Int | Self::Uint | Self::Float | Self::Str | Self::Duration
        )
    }
}

/// One YAML field of a config struct.
#[derive(Clone, Copy, Debug)]
pub struct FieldSchema {
    /// The YAML key.
    pub name: &'static str,
    /// Go's `yaml:",inline"`: the nested struct's keys live in the parent.
    pub inline: bool,
    /// The field's wire shape.
    pub kind: Kind,
}

/// One config struct.
#[derive(Clone, Copy, Debug)]
pub struct StructSchema {
    /// The Rust type name, used in error messages.
    pub name: &'static str,
    /// The struct's YAML fields, in declaration order.
    pub fields: &'static [FieldSchema],
}

impl StructSchema {
    /// Looks up a field by its YAML key, following inlined structs, and
    /// returns the field together with the schema that declared it.
    #[must_use]
    pub fn lookup(&'static self, key: &str) -> Option<(&'static FieldSchema, &'static Self)> {
        for field in self.fields {
            if field.inline {
                if let Kind::Nested(inner) = field.kind
                    && let Some(found) = inner().lookup(key)
                {
                    return Some(found);
                }
            } else if field.name == key {
                return Some((field, self));
            }
        }
        None
    }

    /// Every scalar field in the tree, as dotted YAML paths, in the order the
    /// Go generator would reach them (breadth-first over nested structs).
    ///
    /// Inlined structs contribute their fields under the parent's prefix, which
    /// is what `ToCLIFlagNames` does with `yaml:",inline"`.
    #[must_use]
    pub fn scalar_paths(&'static self) -> Vec<(String, Kind)> {
        let mut out = Vec::new();
        let mut queue = vec![(self, String::new())];
        while !queue.is_empty() {
            let (node, prefix) = queue.remove(0);
            for field in node.fields {
                let path = if field.inline {
                    prefix.clone()
                } else if prefix.is_empty() {
                    field.name.to_owned()
                } else {
                    format!("{prefix}.{}", field.name)
                };
                match field.kind {
                    Kind::Nested(inner) => queue.push((inner(), path)),
                    kind if kind.is_flaggable() => out.push((path, kind)),
                    _ => {}
                }
            }
        }
        out
    }
}

/// Implemented by every config struct through `#[derive(ConfigSchema)]`.
pub trait HasSchema {
    /// The struct's YAML schema.
    fn schema() -> &'static StructSchema;
}
