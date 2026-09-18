//! NAT models.
//!
//! Only the two behaviours the port plan's scenarios need: full cone, which is
//! what a consumer router usually does and what makes host candidates work, and
//! symmetric, which is what breaks them and forces a TURN relay. Restricted
//! cone sits between the two and is not modelled, because no scenario in the
//! plan distinguishes it from full cone.

use std::net::{IpAddr, SocketAddr};

use ahash::AHashMap;

/// Which mapping behaviour a NAT uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatKind {
    /// One external port per internal address, reused for every destination,
    /// and open to any peer once created. Host candidates work through this.
    FullCone,
    /// A fresh external port per `(internal address, destination)` pair, and
    /// inbound traffic accepted only from that destination. A peer told about
    /// one mapping cannot use it to reach another, which is what makes
    /// symmetric NAT a TURN case.
    Symmetric,
}

/// A NAT sitting in front of one or more endpoints.
#[derive(Debug)]
pub struct Nat {
    kind: NatKind,
    external_ip: IpAddr,
    next_port: u16,
    /// Outbound: what external address an internal sender is seen as.
    outbound: AHashMap<(SocketAddr, Option<SocketAddr>), u16>,
    /// Inbound: which internal address an external port maps back to, and for
    /// a symmetric NAT, the only peer allowed to use it.
    inbound: AHashMap<u16, (SocketAddr, Option<SocketAddr>)>,
}

/// Why a NAT dropped an inbound datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatDrop {
    /// Nothing inside ever sent out through that external port.
    NoMapping,
    /// The mapping exists but belongs to a different peer. This is the whole
    /// point of a symmetric NAT and the reason `force_relay` scenarios need it.
    WrongPeer,
}

impl Nat {
    /// A NAT of `kind` handing out ports on `external_ip`.
    ///
    /// Ports are allocated from 40000 upward, deterministically, so a seeded
    /// run reproduces the same external addresses.
    #[must_use]
    pub fn new(kind: NatKind, external_ip: IpAddr) -> Self {
        Self {
            kind,
            external_ip,
            next_port: 40000,
            outbound: AHashMap::new(),
            inbound: AHashMap::new(),
        }
    }

    /// Which behaviour this NAT implements.
    #[must_use]
    pub fn kind(&self) -> NatKind {
        self.kind
    }

    /// Translate an outbound datagram's source address, creating the mapping
    /// if this is the first packet of the flow.
    pub fn translate_outbound(&mut self, internal: SocketAddr, dest: SocketAddr) -> SocketAddr {
        let key = match self.kind {
            NatKind::FullCone => (internal, None),
            NatKind::Symmetric => (internal, Some(dest)),
        };
        let port = *self.outbound.entry(key).or_insert_with(|| {
            let port = self.next_port;
            self.next_port = self.next_port.wrapping_add(1).max(40000);
            port
        });
        self.inbound.entry(port).or_insert(match self.kind {
            NatKind::FullCone => (internal, None),
            NatKind::Symmetric => (internal, Some(dest)),
        });
        SocketAddr::new(self.external_ip, port)
    }

    /// Translate an inbound datagram's destination address back to an internal
    /// one, or say why it was dropped.
    ///
    /// # Errors
    ///
    /// [`NatDrop`] when no mapping covers the datagram.
    pub fn translate_inbound(
        &self,
        external: SocketAddr,
        from: SocketAddr,
    ) -> Result<SocketAddr, NatDrop> {
        let (internal, allowed) = self
            .inbound
            .get(&external.port())
            .ok_or(NatDrop::NoMapping)?;
        match allowed {
            Some(peer) if *peer != from => Err(NatDrop::WrongPeer),
            _ => Ok(*internal),
        }
    }

    /// Whether `addr` is one of this NAT's external addresses.
    #[must_use]
    pub fn owns(&self, addr: SocketAddr) -> bool {
        addr.ip() == self.external_ip && self.inbound.contains_key(&addr.port())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([198, 51, 100, last])
    }

    fn sock(a: u8, port: u16) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, a], port))
    }

    #[test]
    fn full_cone_reuses_one_port_for_every_destination() {
        let mut nat = Nat::new(NatKind::FullCone, ip(1));
        let peer_a = SocketAddr::from(([203, 0, 113, 1], 3478));
        let peer_b = SocketAddr::from(([203, 0, 113, 2], 3478));

        let via_a = nat.translate_outbound(sock(5, 1234), peer_a);
        let via_b = nat.translate_outbound(sock(5, 1234), peer_b);
        assert_eq!(via_a, via_b);

        // And a third party that was merely told the address can use it.
        let stranger = SocketAddr::from(([203, 0, 113, 9], 9));
        assert_eq!(nat.translate_inbound(via_a, stranger), Ok(sock(5, 1234)));
    }

    #[test]
    fn symmetric_allocates_a_port_per_destination_and_rejects_strangers() {
        let mut nat = Nat::new(NatKind::Symmetric, ip(1));
        let peer_a = SocketAddr::from(([203, 0, 113, 1], 3478));
        let peer_b = SocketAddr::from(([203, 0, 113, 2], 3478));

        let via_a = nat.translate_outbound(sock(5, 1234), peer_a);
        let via_b = nat.translate_outbound(sock(5, 1234), peer_b);
        assert_ne!(via_a, via_b);

        assert_eq!(nat.translate_inbound(via_a, peer_a), Ok(sock(5, 1234)));
        // The candidate learned by peer A is useless to peer B: this is what
        // forces a relay.
        assert_eq!(
            nat.translate_inbound(via_a, peer_b),
            Err(NatDrop::WrongPeer)
        );
    }

    #[test]
    fn an_unmapped_port_is_dropped() {
        let nat = Nat::new(NatKind::FullCone, ip(1));
        let unmapped = SocketAddr::new(ip(1), 40000);
        let from = SocketAddr::from(([203, 0, 113, 1], 3478));
        assert_eq!(
            nat.translate_inbound(unmapped, from),
            Err(NatDrop::NoMapping)
        );
    }

    #[test]
    fn two_internal_endpoints_get_distinct_external_ports() {
        let mut nat = Nat::new(NatKind::FullCone, ip(1));
        let peer = SocketAddr::from(([203, 0, 113, 1], 3478));
        let a = nat.translate_outbound(sock(5, 1234), peer);
        let b = nat.translate_outbound(sock(6, 1234), peer);
        assert_ne!(a, b);
        assert_eq!(nat.translate_inbound(a, peer), Ok(sock(5, 1234)));
        assert_eq!(nat.translate_inbound(b, peer), Ok(sock(6, 1234)));
    }
}
