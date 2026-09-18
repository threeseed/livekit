//! Redis channel names, byte-exact with `livekit/psrpc`.
//!
//! Ported from `psrpc/pkg/info/channels.go`. This is a cluster-facing
//! compatibility contract: a Rust media node joining a live Go cluster
//! subscribes to the channels the Go nodes publish on, so a single byte of
//! difference here is a node that silently receives nothing.
//!
//! psrpc publishes each message on up to three names at once:
//!
//! - **Legacy**, pipe-delimited, which older nodes still subscribe to.
//! - **Server**, the `SRV.`/`CLI.`-prefixed dotted form current nodes use.
//! - **Local**, used only by the in-process bus, where there is no service or
//!   client id to disambiguate.
//!
//! Every user-supplied part (a room name, a participant identity) is sanitised
//! the same way Go sanitises it: characters outside `[0-9A-Za-z_]` are escaped
//! as `u+xxxx` or `U+xxxxxxxx`. Room names are user input, so this escaping is
//! also what keeps a room called `a.b` from colliding with the delimiter.

use std::fmt::Write as _;

/// The set of names one logical channel is addressed by.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Channel {
    /// Pipe-delimited legacy name.
    pub legacy: String,
    /// Dotted `SRV.`/`CLI.` name.
    pub server: String,
    /// In-process name, empty where psrpc does not build one.
    pub local: String,
}

/// Everything needed to name the channels of one request.
#[derive(Debug, Clone, Copy)]
pub struct ChannelNames<'a> {
    /// The psrpc service name, as the generated `SERVICE_NAME` constant gives
    /// it.
    pub service: &'a str,
    /// The method name, PascalCase, as the proto declares it.
    pub method: &'a str,
    /// The topic parts, in the order the method's `topic_params.names`
    /// declares them.
    pub topic: &'a [String],
    /// Whether the method is queue-routed, which appends `.Q` to the server
    /// name so that queue and non-queue subscribers do not share it.
    pub queue: bool,
}

impl ChannelNames<'_> {
    /// The channel a request is published on.
    #[must_use]
    pub fn rpc(&self) -> Channel {
        Channel {
            legacy: legacy(&[self.service, self.method], self.topic, Some("REQ")),
            server: server(self.service, self.topic, self.queue),
            local: local(self.method, "REQ"),
        }
    }

    /// The channel a claim response is published on.
    ///
    /// Never queue-routed: the claim protocol needs every server to see the
    /// response, which is why `queue` is ignored here.
    #[must_use]
    pub fn claim_response(&self) -> Channel {
        Channel {
            legacy: legacy(&[self.service, self.method], self.topic, Some("RCLAIM")),
            server: server(self.service, self.topic, false),
            local: local(self.method, "RCLAIM"),
        }
    }

    /// The channel a stream's server side is published on.
    #[must_use]
    pub fn stream_server(&self) -> Channel {
        Channel {
            legacy: legacy(&[self.service, self.method], self.topic, Some("STR")),
            server: server(self.service, self.topic, false),
            local: local(self.method, "STR"),
        }
    }

    /// The key a handler is registered under, `Method.topic.parts`.
    #[must_use]
    pub fn handler_key(&self) -> String {
        join_sanitised(
            '.',
            std::iter::once(self.method).chain(self.topic.iter().map(String::as_str)),
        )
    }
}

/// The channel a client listens on for claim requests.
#[must_use]
pub fn claim_request_channel(service: &str, client_id: &str) -> Channel {
    Channel {
        legacy: join_sanitised('|', [service, client_id, "CLAIM"]),
        server: client(service, client_id, "CLAIM"),
        local: String::new(),
    }
}

/// The channel a node listens on for stream messages.
#[must_use]
pub fn stream_channel(service: &str, node_id: &str) -> Channel {
    Channel {
        legacy: join_sanitised('|', [service, node_id, "STR"]),
        server: client(service, node_id, "STR"),
        local: String::new(),
    }
}

/// The channel a client listens on for responses.
#[must_use]
pub fn response_channel(service: &str, client_id: &str) -> Channel {
    Channel {
        legacy: join_sanitised('|', [service, client_id, "RES"]),
        server: client(service, client_id, "RES"),
        local: String::new(),
    }
}

fn legacy(head: &[&str], topic: &[String], tail: Option<&str>) -> String {
    let parts = head
        .iter()
        .copied()
        .chain(topic.iter().map(String::as_str))
        .chain(tail);
    join_sanitised('|', parts)
}

/// `CLI.<service>.<client id>.<channel>`.
///
/// Go does not sanitise these three: a service name is a compile-time
/// constant and a client id is generated, so neither can carry a delimiter.
fn client(service: &str, client_id: &str, channel: &str) -> String {
    let mut out = String::with_capacity(4 + service.len() + client_id.len() + channel.len() + 2);
    out.push_str("CLI.");
    out.push_str(service);
    out.push('.');
    out.push_str(client_id);
    out.push('.');
    out.push_str(channel);
    out
}

/// `SRV.<service>[.<topic part>]*[.Q]`.
///
/// Empty topic parts are skipped rather than producing an empty segment, which
/// is what makes a one-parameter topic with an empty value name the same
/// channel as no topic at all. That is Go's behaviour and the wire depends on
/// it.
fn server(service: &str, topic: &[String], queue: bool) -> String {
    let mut out = String::with_capacity(4 + service.len() + topic.len() * 16 + 2);
    out.push_str("SRV.");
    out.push_str(service);
    for part in topic {
        if part.is_empty() {
            continue;
        }
        out.push('.');
        push_sanitised(&mut out, part);
    }
    if queue {
        out.push_str(".Q");
    }
    out
}

/// `<method>.<channel>`, unsanitised for the same reason as [`client`].
fn local(method: &str, channel: &str) -> String {
    let mut out = String::with_capacity(method.len() + channel.len() + 1);
    out.push_str(method);
    out.push('.');
    out.push_str(channel);
    out
}

/// Join `parts` with `delim`, sanitising each.
///
/// A part that sanitises to nothing contributes no delimiter either, which
/// matters because Go's `appendChannelParts` only writes a delimiter when the
/// previous part actually produced bytes. A topic with an empty element must
/// not name a different channel than one without it.
fn join_sanitised<'a>(delim: char, parts: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    let mut prefix = false;
    for part in parts {
        if prefix {
            out.push(delim);
        }
        let before = out.len();
        push_sanitised(&mut out, part);
        prefix = out.len() > before;
    }
    out
}

/// Append `input` with everything outside `[0-9A-Za-z_]` escaped.
///
/// `u+` and four lowercase hex digits for anything below U+10000, `U+` and
/// eight for the rest. The case of the prefix is the only thing distinguishing
/// the two forms, so it is not cosmetic.
fn push_sanitised(out: &mut String, input: &str) {
    for ch in input.chars() {
        let code = ch as u32;
        let plain = ch.is_ascii_digit() || ch.is_ascii_alphabetic() || ch == '_';
        if plain {
            out.push(ch);
        } else if code < 0x10000 {
            let _ = write!(out, "u+{code:04x}");
        } else {
            let _ = write!(out, "U+{code:08x}");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn topic(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|p| (*p).to_owned()).collect()
    }

    #[test]
    fn rpc_channel_matches_the_go_format() {
        let topic = topic(&["room1"]);
        let names = ChannelNames {
            service: "Room",
            method: "DeleteRoom",
            topic: &topic,
            queue: true,
        };
        let channel = names.rpc();
        assert_eq!(channel.legacy, "Room|DeleteRoom|room1|REQ");
        assert_eq!(channel.server, "SRV.Room.room1.Q");
        assert_eq!(channel.local, "DeleteRoom.REQ");
    }

    #[test]
    fn claim_response_is_never_queue_routed() {
        let topic = topic(&["room1"]);
        let names = ChannelNames {
            service: "Room",
            method: "DeleteRoom",
            topic: &topic,
            queue: true,
        };
        // The request is queue-routed, so it carries `.Q`; the claim response
        // must not, or only one server would ever see a claim.
        assert!(names.rpc().server.ends_with(".Q"));
        assert_eq!(names.claim_response().server, "SRV.Room.room1");
        assert_eq!(
            names.claim_response().legacy,
            "Room|DeleteRoom|room1|RCLAIM"
        );
    }

    #[test]
    fn stream_server_channel_matches_the_go_format() {
        let topic = topic(&["node1"]);
        let names = ChannelNames {
            service: "Signal",
            method: "RelaySignal",
            topic: &topic,
            queue: false,
        };
        assert_eq!(names.stream_server().legacy, "Signal|RelaySignal|node1|STR");
        assert_eq!(names.stream_server().server, "SRV.Signal.node1");
        assert_eq!(names.stream_server().local, "RelaySignal.STR");
    }

    #[test]
    fn client_channels_match_the_go_format() {
        assert_eq!(
            claim_request_channel("Room", "CLI_abc"),
            Channel {
                legacy: "Room|CLI_abc|CLAIM".to_owned(),
                server: "CLI.Room.CLI_abc.CLAIM".to_owned(),
                local: String::new(),
            }
        );
        assert_eq!(
            response_channel("Room", "CLI_abc").server,
            "CLI.Room.CLI_abc.RES"
        );
        assert_eq!(
            stream_channel("Signal", "ND_xyz").server,
            "CLI.Signal.ND_xyz.STR"
        );
    }

    #[test]
    fn a_multi_part_topic_keeps_its_order() {
        let topic = topic(&["room1", "PA_participant"]);
        let names = ChannelNames {
            service: "Participant",
            method: "UpdateParticipant",
            topic: &topic,
            queue: false,
        };
        assert_eq!(names.rpc().server, "SRV.Participant.room1.PA_participant");
        assert_eq!(
            names.rpc().legacy,
            "Participant|UpdateParticipant|room1|PA_participant|REQ"
        );
        assert_eq!(
            names.handler_key(),
            "UpdateParticipant.room1.PA_participant"
        );
    }

    #[test]
    fn a_room_name_with_a_delimiter_is_escaped_not_split() {
        // A room called `a.b|c` must not be able to name the channel of a room
        // called `a` with topic `b`, so every non-word character is escaped.
        let topic = topic(&["a.b|c"]);
        let names = ChannelNames {
            service: "Room",
            method: "SendData",
            topic: &topic,
            queue: false,
        };
        assert_eq!(names.rpc().server, "SRV.Room.au+002ebu+007cc");
        assert_eq!(names.rpc().legacy, "Room|SendData|au+002ebu+007cc|REQ");
    }

    #[test]
    fn non_ascii_uses_the_four_digit_form_and_astral_the_eight() {
        let mut out = String::new();
        push_sanitised(&mut out, "é");
        assert_eq!(out, "u+00e9");

        let mut out = String::new();
        push_sanitised(&mut out, "\u{1F600}");
        assert_eq!(out, "U+0001f600");
    }

    #[test]
    fn word_characters_pass_through_untouched() {
        let mut out = String::new();
        push_sanitised(&mut out, "Room_1AZaz09");
        assert_eq!(out, "Room_1AZaz09");
    }

    #[test]
    fn an_empty_topic_part_contributes_no_delimiter() {
        // Go writes a delimiter only when the previous part produced bytes, so
        // an empty room name must not shift the rest of the name along.
        let topic = topic(&["", "room1"]);
        let names = ChannelNames {
            service: "Room",
            method: "SendData",
            topic: &topic,
            queue: false,
        };
        assert_eq!(names.rpc().legacy, "Room|SendData|room1|REQ");
        assert_eq!(names.rpc().server, "SRV.Room.room1");
    }

    #[test]
    fn an_empty_topic_names_the_service_channel() {
        let names = ChannelNames {
            service: "Room",
            method: "ListRooms",
            topic: &[],
            queue: true,
        };
        assert_eq!(names.rpc().legacy, "Room|ListRooms|REQ");
        assert_eq!(names.rpc().server, "SRV.Room.Q");
        assert_eq!(names.handler_key(), "ListRooms");
    }
}
