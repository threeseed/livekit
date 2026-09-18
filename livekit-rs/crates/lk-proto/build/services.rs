//! Service-trait generation from the descriptor set.
//!
//! Two generators, from two different worlds:
//!
//! - **twirp**, for the five HTTP services the client SDKs and the CLI call
//!   (`RoomService`, `AgentDispatchService`, `Egress`, `Ingress`, `SIP`). These
//!   get a trait per service plus the routing table `lk-service` mounts.
//! - **psrpc**, for the `rpc/*.proto` services the nodes use to talk to each
//!   other over Redis. No psrpc codegen exists in Rust anywhere, so this
//!   generates the method table and the typed topic helpers that `lk-bus` will
//!   drive in Phase 4.
//!
//! Neither generator emits transport code. A trait and a method table are what
//! Phase 0 can validate; the twirp router and the Redis bus are Phase 1 and
//! Phase 4, and generating their innards now would be generating against an
//! interface that does not exist yet.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::descriptor::{
    FileDescriptorProtoLite, FileDescriptorSetLite, Routing, ServiceDescriptorProtoLite,
    rust_path_of, to_snake_case,
};

/// The five twirp services, by proto service name. Everything else in the
/// `livekit` package is either a psrpc service or not a service at all.
const TWIRP_SERVICES: &[&str] = &[
    "RoomService",
    "AgentDispatchService",
    "Egress",
    "Ingress",
    "SIP",
];

/// Render the twirp traits and routing tables.
pub fn generate_twirp(set: &FileDescriptorSetLite) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str(TWIRP_PREAMBLE);

    let mut total_methods = 0usize;
    let mut service_count = 0usize;

    for file in &set.file {
        let package = file.package.as_deref().unwrap_or_default();
        for service in &file.service {
            let Some(name) = service.name.as_deref() else {
                continue;
            };
            if !TWIRP_SERVICES.contains(&name) {
                continue;
            }
            service_count += 1;
            total_methods += service.method.len();
            write_twirp_service(&mut out, package, name, service);
        }
    }

    let _ = writeln!(
        out,
        "/// Every twirp service generated from the vendored protos.\n\
         pub const SERVICE_COUNT: usize = {service_count};\n\
         /// Every twirp method across those services.\n\
         pub const METHOD_COUNT: usize = {total_methods};"
    );
    out
}

fn write_twirp_service(
    out: &mut String,
    package: &str,
    name: &str,
    service: &ServiceDescriptorProtoLite,
) {
    let module = to_snake_case(name);
    let full_name = if package.is_empty() {
        name.to_owned()
    } else {
        format!("{package}.{name}")
    };

    let _ = writeln!(
        out,
        "\n/// `{full_name}`, served over twirp at `/twirp/{full_name}/<Method>`.\n\
         pub mod {module} {{\n    \
         use super::{{TwirpError, TwirpMethod}};\n\n    \
         /// The fully qualified protobuf service name, which is also the\n    \
         /// second path segment of every route.\n    \
         pub const SERVICE_NAME: &str = \"{full_name}\";\n"
    );

    // The routing table. `lk-service` walks this to mount the router, so it is
    // a const slice rather than a match: adding a method to the proto must not
    // require touching hand-written code.
    let _ = writeln!(
        out,
        "    /// Every method on this service, in declaration order.\n    \
         pub const METHODS: &[TwirpMethod] = &["
    );
    for method in &service.method {
        let Some(method_name) = method.name.as_deref() else {
            continue;
        };
        let _ = writeln!(
            out,
            "        TwirpMethod {{ name: \"{method_name}\", path: \"/twirp/{full_name}/{method_name}\" }},"
        );
    }
    let _ = writeln!(out, "    ];\n");

    let _ = writeln!(
        out,
        "    /// Server side of `{full_name}`.\n    \
         ///\n    \
         /// Implemented by `lk-service`; the router that dispatches to it is\n    \
         /// hand-written there, because it needs the axum types this crate\n    \
         /// deliberately does not depend on.\n    \
         #[allow(async_fn_in_trait)]\n    \
         pub trait {name}: Send + Sync + 'static {{"
    );
    for method in &service.method {
        let Some(method_name) = method.name.as_deref() else {
            continue;
        };
        let request = rust_path_of(method.input_type.as_deref().unwrap_or_default());
        let response = rust_path_of(method.output_type.as_deref().unwrap_or_default());
        let fn_name = to_snake_case(method_name);
        let _ = writeln!(
            out,
            "        /// `{method_name}`\n        \
             fn {fn_name}(\n            \
             &self,\n            \
             request: {request},\n        \
             ) -> impl core::future::Future<Output = Result<{response}, TwirpError>> + Send;"
        );
    }
    let _ = writeln!(out, "    }}\n}}");
}

/// Render the psrpc method tables and topic helpers.
pub fn generate_psrpc(set: &FileDescriptorSetLite) -> String {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str(PSRPC_PREAMBLE);

    let mut service_count = 0usize;
    let mut method_count = 0usize;

    for file in &set.file {
        if file.package.as_deref() != Some("rpc") {
            continue;
        }
        for service in &file.service {
            let Some(name) = service.name.as_deref() else {
                continue;
            };
            service_count += 1;
            method_count += service.method.len();
            write_psrpc_service(&mut out, file, name, service);
        }
    }

    let _ = writeln!(
        out,
        "/// Every psrpc service generated from `rpc/*.proto`.\n\
         pub const SERVICE_COUNT: usize = {service_count};\n\
         /// Every psrpc method across those services.\n\
         pub const METHOD_COUNT: usize = {method_count};"
    );
    out
}

fn write_psrpc_service(
    out: &mut String,
    file: &FileDescriptorProtoLite,
    name: &str,
    service: &ServiceDescriptorProtoLite,
) {
    let module = to_snake_case(name);
    let source = file.name.as_deref().unwrap_or("<unknown>");

    let _ = writeln!(
        out,
        "\n/// `rpc.{name}`, from `{source}`.\n\
         pub mod {module} {{\n    \
         use super::{{PsrpcMethod, Routing, TopicParams}};\n\n    \
         /// The service name psrpc puts in every channel it builds. This is a\n    \
         /// wire value: a Rust node and a Go node must agree on it or they\n    \
         /// subscribe to different Redis channels.\n    \
         pub const SERVICE_NAME: &str = \"{name}\";\n"
    );

    let _ = writeln!(
        out,
        "    /// Every method on this service, in declaration order.\n    \
         pub const METHODS: &[PsrpcMethod] = &["
    );
    for method in &service.method {
        let Some(method_name) = method.name.as_deref() else {
            continue;
        };
        let opts = method.options.as_ref().and_then(|o| o.psrpc.as_ref());
        let routing = match opts.map(super::descriptor::PsrpcOptions::effective_routing) {
            Some(Routing::Affinity) => "Routing::Affinity",
            Some(Routing::Multi) => "Routing::Multi",
            _ => "Routing::Queue",
        };
        let topics = opts.is_some_and(|o| o.topics);
        let stream = opts.is_some_and(|o| o.stream);
        let subscription = opts.is_some_and(|o| o.subscription);
        let params = opts.and_then(|o| o.topic_params.as_ref());
        let group = params.map(|p| p.group.as_str()).unwrap_or_default();
        let typed = params.is_some_and(|p| p.typed);
        let single_server = params.is_some_and(|p| p.single_server);
        let names = params
            .map(|p| {
                p.names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();

        let _ = writeln!(
            out,
            "        PsrpcMethod {{\n            \
             name: \"{method_name}\",\n            \
             routing: {routing},\n            \
             topics: {topics},\n            \
             stream: {stream},\n            \
             subscription: {subscription},\n            \
             topic_params: TopicParams {{ group: \"{group}\", names: &[{names}], typed: {typed}, single_server: {single_server} }},\n        \
             }},"
        );
    }
    let _ = writeln!(out, "    ];\n");

    // Topic helpers, one per distinct parameter shape rather than one per
    // method: half a dozen methods on a service usually share `["room"]`, and
    // emitting six identical functions would only make the call sites
    // ambiguous.
    let mut shapes: BTreeSet<Vec<String>> = BTreeSet::new();
    for method in &service.method {
        if let Some(params) = method
            .options
            .as_ref()
            .and_then(|o| o.psrpc.as_ref())
            .and_then(|o| o.topic_params.as_ref())
            && !params.names.is_empty()
        {
            shapes.insert(params.names.clone());
        }
    }
    for shape in shapes {
        let fn_name = format!(
            "{}_topic",
            shape
                .iter()
                .map(|n| to_snake_case(n))
                .collect::<Vec<_>>()
                .join("_")
        );
        let args = shape
            .iter()
            .map(|n| format!("{}: &str", to_snake_case(n)))
            .collect::<Vec<_>>()
            .join(", ");
        let pushes = shape
            .iter()
            .map(|n| format!("        parts.push({});", to_snake_case(n)))
            .collect::<Vec<_>>()
            .join("\n");
        let doc = shape.join("`, `");
        let _ = writeln!(
            out,
            "    /// The topic addressed by `{doc}` for the methods on this\n    \
             /// service that take it.\n    \
             #[must_use]\n    \
             pub fn {fn_name}({args}) -> Vec<String> {{\n        \
             let mut parts: Vec<&str> = Vec::with_capacity({});\n{pushes}\n        \
             parts.into_iter().map(str::to_owned).collect()\n    \
             }}\n",
            shape.len(),
        );
    }

    let _ = writeln!(out, "}}");
}

const HEADER: &str = "// @generated by lk-proto's build script from the vendored livekit/protocol\n\
// descriptors. Do not edit; run `cargo xtask proto-sync` and rebuild.\n\n";

const TWIRP_PREAMBLE: &str = r#"
/// One twirp method: its protobuf name and the path it is served at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwirpMethod {
    /// The method name as the proto declares it, PascalCase.
    pub name: &'static str,
    /// `/twirp/<package>.<Service>/<Method>`, the only route twirp uses.
    pub path: &'static str,
}

/// A twirp error, in the shape twirp puts on the wire.
///
/// The `code` is a twirp error code string (`not_found`, `permission_denied`,
/// ...), not an HTTP status; twirp derives the status from it. Go's
/// `livekit-server` returns these from every RoomService handler and the client
/// SDKs match on the code, so the strings are a compatibility surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwirpError {
    /// The twirp error code.
    pub code: &'static str,
    /// A human-readable message.
    pub msg: String,
}

impl TwirpError {
    /// Build an error with `code` and `msg`.
    pub fn new(code: &'static str, msg: impl Into<String>) -> Self {
        Self { code, msg: msg.into() }
    }

    /// `not_found`.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new("not_found", msg)
    }

    /// `permission_denied`.
    pub fn permission_denied(msg: impl Into<String>) -> Self {
        Self::new("permission_denied", msg)
    }

    /// `invalid_argument`.
    pub fn invalid_argument(msg: impl Into<String>) -> Self {
        Self::new("invalid_argument", msg)
    }

    /// `internal`.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new("internal", msg)
    }
}

impl core::fmt::Display for TwirpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {}", self.code, self.msg)
    }
}

impl std::error::Error for TwirpError {}
"#;

const PSRPC_PREAMBLE: &str = r#"
/// How psrpc routes a request to servers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// Servers join a queue and exactly one receives each request.
    Queue,
    /// The service supplies an affinity function for handler selection.
    Affinity,
    /// Every server responds to every request.
    Multi,
}

/// How a method's topic is composed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TopicParams {
    /// Methods sharing a group are registered and deregistered atomically.
    pub group: &'static str,
    /// The named parameters the topic is built from, in order.
    pub names: &'static [&'static str],
    /// Whether the parameters have associated string-like type parameters.
    pub typed: bool,
    /// Whether at most one server may register for each topic.
    pub single_server: bool,
}

/// One psrpc method, as declared by its `psrpc.options`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsrpcMethod {
    /// The method name as the proto declares it, PascalCase. This is a wire
    /// value: it appears in the legacy channel name and the handler key.
    pub name: &'static str,
    /// How requests are routed.
    pub routing: Routing,
    /// Whether the method is addressed by topic.
    pub topics: bool,
    /// Whether the method is bidirectionally streaming.
    pub stream: bool,
    /// Whether the method is a pub/sub subscription rather than an RPC.
    pub subscription: bool,
    /// How the topic is composed.
    pub topic_params: TopicParams,
}

impl PsrpcMethod {
    /// Look a method up by its proto name.
    #[must_use]
    pub fn find(methods: &'static [PsrpcMethod], name: &str) -> Option<&'static PsrpcMethod> {
        methods.iter().find(|m| m.name == name)
    }
}
"#;
