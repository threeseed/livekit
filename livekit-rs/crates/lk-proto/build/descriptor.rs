//! A deliberately partial view of `FileDescriptorSet`.
//!
//! The psrpc service options live in extension field 2198 of
//! `google.protobuf.MethodOptions`. `prost-types`' own `MethodOptions` has no
//! storage for extensions, so decoding a descriptor with it silently throws the
//! psrpc options away, and without them there is nothing to generate topic
//! helpers from.
//!
//! So the descriptor bytes are decoded twice: once by `prost-build`, with the
//! full `prost-types` model, to emit the message types; and once here, with a
//! model that declares only the handful of fields the service generators need
//! plus the psrpc extension. Protobuf is field-number addressed, so every field
//! not declared here is skipped as an unknown field. That is the whole trick,
//! and it is why this file is short.

use prost::Message;

/// A `FileDescriptorSet`, cut down to services.
#[derive(Clone, PartialEq, Message)]
pub struct FileDescriptorSetLite {
    /// `FileDescriptorProto file = 1`
    #[prost(message, repeated, tag = "1")]
    pub file: Vec<FileDescriptorProtoLite>,
}

/// A `FileDescriptorProto`, cut down to services.
#[derive(Clone, PartialEq, Message)]
pub struct FileDescriptorProtoLite {
    /// `optional string name = 1`
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    /// `optional string package = 2`
    #[prost(string, optional, tag = "2")]
    pub package: Option<String>,
    /// `repeated ServiceDescriptorProto service = 6`
    #[prost(message, repeated, tag = "6")]
    pub service: Vec<ServiceDescriptorProtoLite>,
}

/// A `ServiceDescriptorProto`.
#[derive(Clone, PartialEq, Message)]
pub struct ServiceDescriptorProtoLite {
    /// `optional string name = 1`
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    /// `repeated MethodDescriptorProto method = 2`
    #[prost(message, repeated, tag = "2")]
    pub method: Vec<MethodDescriptorProtoLite>,
}

/// A `MethodDescriptorProto`.
#[derive(Clone, PartialEq, Message)]
pub struct MethodDescriptorProtoLite {
    /// `optional string name = 1`
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    /// `optional string input_type = 2`, fully qualified and leading-dotted.
    #[prost(string, optional, tag = "2")]
    pub input_type: Option<String>,
    /// `optional string output_type = 3`, fully qualified and leading-dotted.
    #[prost(string, optional, tag = "3")]
    pub output_type: Option<String>,
    /// `optional MethodOptions options = 4`
    #[prost(message, optional, tag = "4")]
    pub options: Option<MethodOptionsLite>,
}

/// `google.protobuf.MethodOptions`, declaring only the psrpc extension.
#[derive(Clone, PartialEq, Message)]
pub struct MethodOptionsLite {
    /// `extend google.protobuf.MethodOptions { optional psrpc.Options options = 2198; }`
    #[prost(message, optional, tag = "2198")]
    pub psrpc: Option<PsrpcOptions>,
}

/// `psrpc.Options`.
#[derive(Clone, PartialEq, Message)]
pub struct PsrpcOptions {
    /// This method is a pub/sub.
    #[prost(bool, tag = "1")]
    pub subscription: bool,
    /// This method uses topics.
    #[prost(bool, tag = "2")]
    pub topics: bool,
    /// How the topic is composed.
    #[prost(message, optional, tag = "3")]
    pub topic_params: Option<TopicParamOptions>,
    /// The method uses bidirectional streaming.
    #[prost(bool, tag = "4")]
    pub stream: bool,
    /// `psrpc.Routing`: 0 queue, 1 affinity, 2 multi.
    #[prost(int32, tag = "8")]
    pub routing: i32,
    /// Deprecated routing oneof, still set by some older methods.
    #[prost(bool, tag = "5")]
    pub multi: bool,
    /// Deprecated routing oneof.
    #[prost(bool, tag = "6")]
    pub affinity_func: bool,
    /// Deprecated routing oneof.
    #[prost(bool, tag = "7")]
    pub queue: bool,
}

/// `psrpc.TopicParamOptions`.
#[derive(Clone, PartialEq, Message)]
pub struct TopicParamOptions {
    /// The rpc can be registered and deregistered atomically with other group
    /// members.
    #[prost(string, tag = "1")]
    pub group: String,
    /// The topic is composed of these string-like parameters.
    #[prost(string, repeated, tag = "2")]
    pub names: Vec<String>,
    /// The topic parameters have associated string-like type parameters.
    #[prost(bool, tag = "3")]
    pub typed: bool,
    /// At most one server is registered for each topic.
    #[prost(bool, tag = "4")]
    pub single_server: bool,
}

/// How a psrpc method is routed, resolved from both the current `type` field
/// and the deprecated oneof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Routing {
    /// Servers join a queue and exactly one receives each request.
    Queue,
    /// The service supplies an affinity function for handler selection.
    Affinity,
    /// Every server responds to every request.
    Multi,
}

impl PsrpcOptions {
    /// The effective routing for this method.
    ///
    /// The `routing` oneof is deprecated in favour of the `type` enum, but the
    /// vendored protos still use both, and a generator that reads only one of
    /// them mis-routes whichever methods use the other.
    pub fn effective_routing(&self) -> Routing {
        match self.routing {
            1 => return Routing::Affinity,
            2 => return Routing::Multi,
            _ => {}
        }
        if self.multi {
            Routing::Multi
        } else if self.affinity_func {
            Routing::Affinity
        } else {
            Routing::Queue
        }
    }
}

impl FileDescriptorSetLite {
    /// Decode a `FileDescriptorSet` from its encoded form.
    pub fn decode_set(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(Self::decode(bytes)?)
    }
}

/// Strip the leading dot protobuf puts on fully qualified type names and map
/// the result to the Rust path `prost-build` generates for it.
///
/// `.livekit.Room` becomes `crate::livekit::Room`; `.livekit.SIPInboundTrunkInfo`
/// becomes `crate::livekit::SipInboundTrunkInfo`, because prost normalises type
/// names through `heck`'s `UpperCamelCase` and a generator that echoes the
/// proto spelling names types that do not exist. Nested messages keep their
/// nesting, which prost renders as a snake_case module per enclosing message.
pub fn rust_path_of(proto_type: &str) -> String {
    let trimmed = proto_type.trim_start_matches('.');
    let segments: Vec<&str> = trimmed.split('.').collect();
    let Some((last, prefix)) = segments.split_last() else {
        return "()".to_owned();
    };
    let mut parts: Vec<String> = Vec::with_capacity(segments.len());
    for segment in prefix {
        // A package segment is already lowercase and passes through; an
        // enclosing message name is PascalCase and becomes a snake_case module.
        parts.push(to_snake_case(segment));
    }
    parts.push(to_upper_camel_case(last));
    format!("crate::{}", parts.join("::"))
}

/// prost's field and module naming: `heck`'s snake_case, then keyword
/// sanitisation.
pub fn to_snake_case(input: &str) -> String {
    use heck::ToSnakeCase as _;
    sanitize_identifier(&input.to_snake_case())
}

/// prost's type naming: `heck`'s UpperCamelCase, then keyword sanitisation.
pub fn to_upper_camel_case(input: &str) -> String {
    use heck::ToUpperCamelCase as _;
    sanitize_identifier(&input.to_upper_camel_case())
}

/// prost's `sanitize_identifier`, reproduced so that generated service code
/// names the same identifiers prost generated.
fn sanitize_identifier(ident: &str) -> String {
    match ident {
        "as" | "break" | "const" | "continue" | "else" | "enum" | "false" | "fn" | "for" | "if"
        | "impl" | "in" | "let" | "loop" | "match" | "mod" | "move" | "mut" | "pub" | "ref"
        | "return" | "static" | "struct" | "trait" | "true" | "type" | "unsafe" | "use"
        | "where" | "while" | "dyn" | "abstract" | "become" | "box" | "do" | "final" | "macro"
        | "override" | "priv" | "typeof" | "unsized" | "virtual" | "yield" | "async" | "await"
        | "try" | "gen" => format!("r#{ident}"),
        "_" | "super" | "self" | "Self" | "extern" | "crate" => format!("{ident}_"),
        s if s.starts_with(|c: char| c.is_numeric()) => format!("_{ident}"),
        _ => ident.to_owned(),
    }
}
