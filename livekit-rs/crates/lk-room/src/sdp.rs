//! SDP helpers.
//!
//! Ports `protocol/sdp`. These are the reads the server does against an offer
//! or answer it did not generate: the DTLS role, the fingerprint, the ICE
//! credentials, the bundle mid, the codecs a media section offers, and the
//! trickle-ICE fragments RFC 9725 defines for WHIP.

use rtc::peer_connection::transport::RTCDtlsRole;
use rtc_sdp::description::common::Attribute;
use rtc_sdp::description::media::MediaDescription;
use rtc_sdp::description::session::{
    ATTR_KEY_CANDIDATE, ATTR_KEY_CONNECTION_SETUP, ATTR_KEY_END_OF_CANDIDATES, ATTR_KEY_GROUP,
    ATTR_KEY_ICELITE, ATTR_KEY_MID, ATTR_KEY_MSID, ATTR_KEY_SSRC, SessionDescription,
};
use rtc_sdp::util::Codec;

use crate::error::{Error, Result};

/// The `a=mid` of a media section, or an empty string.
#[must_use]
pub fn mid_value(media: &MediaDescription) -> &str {
    media.attribute(ATTR_KEY_MID).flatten().unwrap_or("")
}

/// The DTLS fingerprint and its hash algorithm, as `(fingerprint, algorithm)`.
///
/// Every fingerprint in the description must agree: a description offering two
/// different ones is not something to pick from, it is an attacker splicing two
/// descriptions together.
///
/// # Errors
///
/// Returns [`Error::NoFingerprint`], [`Error::ConflictingFingerprints`] or
/// [`Error::InvalidFingerprint`].
pub fn extract_fingerprint(description: &SessionDescription) -> Result<(String, String)> {
    let mut fingerprints = Vec::new();
    if let Some(fingerprint) = description.attribute("fingerprint") {
        fingerprints.push(fingerprint.clone());
    }
    for media in &description.media_descriptions {
        if let Some(Some(fingerprint)) = media.attribute("fingerprint") {
            fingerprints.push(fingerprint.to_owned());
        }
    }

    let Some(first) = fingerprints.first() else {
        return Err(Error::NoFingerprint);
    };
    if fingerprints.iter().any(|f| f != first) {
        return Err(Error::ConflictingFingerprints);
    }

    let mut parts = first.split(' ');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(algorithm), Some(fingerprint), None) => {
            Ok((fingerprint.to_owned(), algorithm.to_owned()))
        }
        _ => Err(Error::InvalidFingerprint),
    }
}

/// The DTLS role the remote description asks this end to take.
///
/// A description with no `a=setup` yields the client role, which is what an
/// answerer defaults to. That default is the reason this helper exists rather
/// than the transport's own: pion and libwebrtc disagree about the role to take
/// against an ICE-lite peer, and LiveKit follows the browsers.
#[must_use]
pub fn extract_dtls_role(description: &SessionDescription) -> RTCDtlsRole {
    for media in &description.media_descriptions {
        let Some(Some(setup)) = media.attribute(ATTR_KEY_CONNECTION_SETUP) else {
            continue;
        };
        if setup == "active" {
            return RTCDtlsRole::Client;
        }
        if setup == "passive" {
            return RTCDtlsRole::Server;
        }
    }
    RTCDtlsRole::Client
}

/// The ICE ufrag and password, as `(ufrag, pwd)`.
///
/// As with the fingerprint, every occurrence must agree.
///
/// # Errors
///
/// Returns [`Error::MissingIceUfrag`], [`Error::MissingIcePwd`],
/// [`Error::ConflictingIceUfrag`] or [`Error::ConflictingIcePwd`].
pub fn extract_ice_credential(description: &SessionDescription) -> Result<(String, String)> {
    let mut ufrags = Vec::new();
    let mut pwds = Vec::new();

    if let Some(ufrag) = description.attribute("ice-ufrag") {
        ufrags.push(ufrag.clone());
    }
    if let Some(pwd) = description.attribute("ice-pwd") {
        pwds.push(pwd.clone());
    }
    for media in &description.media_descriptions {
        if let Some(Some(ufrag)) = media.attribute("ice-ufrag") {
            ufrags.push(ufrag.to_owned());
        }
        if let Some(Some(pwd)) = media.attribute("ice-pwd") {
            pwds.push(pwd.to_owned());
        }
    }

    agree(&ufrags, Error::MissingIceUfrag, Error::ConflictingIceUfrag).and_then(|ufrag| {
        agree(&pwds, Error::MissingIcePwd, Error::ConflictingIcePwd).map(|pwd| (ufrag, pwd))
    })
}

fn agree(values: &[String], missing: Error, conflicting: Error) -> Result<String> {
    let Some(first) = values.first() else {
        return Err(missing);
    };
    if values.iter().any(|v| v != first) {
        return Err(conflicting);
    }
    Ok(first.clone())
}

/// The stream id from a media section's `a=msid`, if it has one.
///
/// An `msid` with only one part is taken whole, as the Go helper does: some
/// clients send `a=msid:<stream>` without the track id.
#[must_use]
pub fn extract_stream_id(media: &MediaDescription) -> Option<String> {
    let msid = media.attribute(ATTR_KEY_MSID).flatten()?;
    let parts: Vec<&str> = msid.split(' ').collect();
    match parts.as_slice() {
        [_, stream, ..] => Some((*stream).to_owned()),
        _ => Some(msid.to_owned()),
    }
}

/// The connection address, from the session or the first media section that
/// carries one.
#[must_use]
pub fn ip(description: &SessionDescription) -> Option<String> {
    if let Some(info) = &description.connection_information
        && info.network_type == "IN"
        && let Some(address) = &info.address
    {
        return Some(address.address.clone());
    }
    for media in &description.media_descriptions {
        if let Some(info) = &media.connection_information
            && info.network_type == "IN"
            && let Some(address) = &info.address
        {
            return Some(address.address.clone());
        }
    }
    None
}

/// The media stream track id, from `a=msid` or from the `a=ssrc ... msid:`
/// attribute older clients send instead.
#[must_use]
pub fn media_stream_track(media: &MediaDescription) -> String {
    if let Some(msid) = media.attribute(ATTR_KEY_MSID).flatten() {
        let parts: Vec<&str> = msid.split(' ').collect();
        if let [_, track] = parts.as_slice() {
            return (*track).to_owned();
        }
    }

    if let Some(ssrc) = media.attribute(ATTR_KEY_SSRC).flatten() {
        let parts: Vec<&str> = ssrc.split(' ').collect();
        if let [_, key, value] = parts.as_slice()
            && key.to_lowercase().starts_with("msid:")
        {
            return (*value).to_owned();
        }
    }

    String::new()
}

/// The simulcast RIDs a media section sends, from `a=simulcast:send`.
#[must_use]
pub fn simulcast_rids(media: &MediaDescription) -> Option<Vec<String>> {
    let value = media.attribute("simulcast").flatten()?;
    let parts: Vec<&str> = value.split(' ').collect();
    match parts.as_slice() {
        ["send", rids] => Some(rids.split(';').map(str::to_owned).collect()),
        _ => None,
    }
}

/// The codecs a media section offers, in the order its format list names them.
///
/// Payload type 0 is skipped when it has no `a=rtpmap`, because that is PCMU's
/// static payload type and its absence from the rtpmap is legal.
///
/// # Errors
///
/// Returns [`Error::InvalidPayloadType`] when a format is not an 8-bit number,
/// and [`Error::UnknownPayloadType`] when a non-zero payload type has no codec.
pub fn codecs_from_media_description(media: &MediaDescription) -> Result<Vec<Codec>> {
    let codecs = media.codecs();
    let mut out = Vec::new();
    for format in &media.media_name.formats {
        let payload_type: u8 = format
            .parse()
            .map_err(|_| Error::InvalidPayloadType(format.clone()))?;
        match codecs.get(&payload_type) {
            Some(codec) => out.push(codec.clone()),
            None if payload_type == 0 => {}
            None => return Err(Error::UnknownPayloadType(payload_type)),
        }
    }
    Ok(out)
}

/// The mid the BUNDLE group names first, which is the transport every other
/// media section rides.
#[must_use]
pub fn bundle_mid(description: &SessionDescription) -> Option<String> {
    let group = description.attribute(ATTR_KEY_GROUP)?;
    let ids: Vec<&str> = group.split(' ').collect();
    match ids.as_slice() {
        [semantics, first, ..] if semantics.eq_ignore_ascii_case("BUNDLE") => {
            Some((*first).to_owned())
        }
        _ => None,
    }
}

/// The ICE parameters of a session or media section within a fragment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FragmentIce {
    /// `a=ice-ufrag`.
    pub ufrag: String,
    /// `a=ice-pwd`.
    pub pwd: String,
    /// `a=ice-lite`, present as a flag.
    pub lite: Option<bool>,
    /// `a=ice-options`.
    pub options: String,
}

impl FragmentIce {
    fn from_attributes(attributes: &[Attribute]) -> Self {
        let get = |key: &str| {
            attributes
                .iter()
                .find(|a| a.key == key)
                .and_then(|a| a.value.clone())
        };
        Self {
            ufrag: get("ice-ufrag").unwrap_or_default(),
            pwd: get("ice-pwd").unwrap_or_default(),
            lite: attributes
                .iter()
                .any(|a| a.key == ATTR_KEY_ICELITE)
                .then_some(true),
            options: get("ice-options").unwrap_or_default(),
        }
    }

    fn marshal_into(&self, out: &mut String) {
        if !self.ufrag.is_empty() {
            out.push_str(&format!("a=ice-ufrag:{}\r\n", self.ufrag));
        }
        if !self.pwd.is_empty() {
            out.push_str(&format!("a=ice-pwd:{}\r\n", self.pwd));
        }
        if self.lite == Some(true) {
            out.push_str("a=ice-lite\r\n");
        }
        if !self.options.is_empty() {
            out.push_str(&format!("a=ice-options:{}\r\n", self.options));
        }
    }
}

/// The media section of a fragment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FragmentMedia {
    /// The `m=` line without its `m=` prefix.
    pub info: String,
    /// `a=mid`.
    pub mid: String,
    /// The section's ICE parameters.
    pub ice: FragmentIce,
    /// `a=candidate` values, without the `a=candidate:` prefix.
    pub candidates: Vec<String>,
    /// `a=end-of-candidates`, present as a flag.
    pub end_of_candidates: Option<bool>,
}

/// An SDP fragment, as RFC 9725 uses for WHIP trickle ICE and ICE restart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SdpFragment {
    /// `a=group`.
    pub group: String,
    /// Session-level ICE parameters.
    pub ice: FragmentIce,
    /// The single media section a fragment carries.
    pub media: Option<FragmentMedia>,
}

impl SdpFragment {
    /// Parses a fragment.
    ///
    /// A fragment is not a session description: it has no `v=`, `o=` or `s=`,
    /// so it is read line by line rather than through the SDP parser.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFragment`] for a malformed line, more than one
    /// media section, no media section, or a BUNDLE group naming a mid the
    /// media section does not carry.
    pub fn parse(fragment: &str) -> Result<Self> {
        let mut out = Self::default();

        for line in fragment.split('\n') {
            let line = line.trim_end_matches([' ', '\r']);
            if line.is_empty() {
                continue;
            }

            let mut chars = line.chars();
            let kind = chars.next().unwrap_or(' ');
            if kind == 'm' {
                if line.len() < 3 || !line.starts_with("m=") {
                    return Err(Error::InvalidFragment("invalid media section"));
                }
                if out.media.is_some() {
                    return Err(Error::InvalidFragment("too many media sections"));
                }
                out.media = Some(FragmentMedia {
                    info: line[2..].to_owned(),
                    ..FragmentMedia::default()
                });
                continue;
            }
            if kind != 'a' {
                continue;
            }
            if line.len() < 2 || !line.starts_with("a=") {
                return Err(Error::InvalidFragment("invalid attribute"));
            }

            let attribute = &line[2..];
            let Some((key, value)) = attribute.split_once(':') else {
                if attribute == ATTR_KEY_ICELITE {
                    match out.media.as_mut() {
                        Some(media) => media.ice.lite = Some(true),
                        None => out.ice.lite = Some(true),
                    }
                }
                continue;
            };

            match key {
                ATTR_KEY_GROUP => out.group = value.to_owned(),
                "ice-ufrag" => match out.media.as_mut() {
                    Some(media) => media.ice.ufrag = value.to_owned(),
                    None => out.ice.ufrag = value.to_owned(),
                },
                "ice-pwd" => match out.media.as_mut() {
                    Some(media) => media.ice.pwd = value.to_owned(),
                    None => out.ice.pwd = value.to_owned(),
                },
                "ice-options" => match out.media.as_mut() {
                    Some(media) => media.ice.options = value.to_owned(),
                    None => out.ice.options = value.to_owned(),
                },
                ATTR_KEY_MID => {
                    if let Some(media) = out.media.as_mut() {
                        media.mid = value.to_owned();
                    }
                }
                ATTR_KEY_CANDIDATE => {
                    if let Some(media) = out.media.as_mut() {
                        media.candidates.push(value.to_owned());
                    }
                }
                ATTR_KEY_END_OF_CANDIDATES => {
                    if let Some(media) = out.media.as_mut() {
                        media.end_of_candidates = Some(true);
                    }
                }
                _ => {}
            }
        }

        let Some(media) = &out.media else {
            return Err(Error::InvalidFragment("missing media section"));
        };
        if !out.group.is_empty() {
            let ids: Vec<&str> = out.group.split(' ').collect();
            if let [semantics, first, ..] = ids.as_slice()
                && semantics.eq_ignore_ascii_case("BUNDLE")
                && media.mid != *first
            {
                return Err(Error::InvalidFragment("bundle media mismatch"));
            }
        }

        Ok(out)
    }

    /// Serialises the fragment.
    #[must_use]
    pub fn marshal(&self) -> String {
        let mut out = String::new();
        if !self.group.is_empty() {
            out.push_str(&format!("a=group:{}\r\n", self.group));
        }
        self.ice.marshal_into(&mut out);
        if let Some(media) = &self.media {
            if !media.info.is_empty() {
                out.push_str(&format!("m={}\r\n", media.info));
            }
            if !media.mid.is_empty() {
                out.push_str(&format!("a=mid:{}\r\n", media.mid));
            }
            media.ice.marshal_into(&mut out);
            for candidate in &media.candidates {
                out.push_str(&format!("a=candidate:{candidate}\r\n"));
            }
            if media.end_of_candidates == Some(true) {
                out.push_str("a=end-of-candidates\r\n");
            }
        }
        out
    }

    /// The mid of the fragment's media section.
    #[must_use]
    pub fn mid(&self) -> &str {
        self.media.as_ref().map_or("", |m| m.mid.as_str())
    }

    /// The candidates the fragment carries.
    #[must_use]
    pub fn candidates(&self) -> &[String] {
        self.media.as_ref().map_or(&[], |m| m.candidates.as_slice())
    }

    /// The fragment's ICE credentials, as `(ufrag, pwd)`.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`extract_ice_credential`].
    pub fn extract_ice_credential(&self) -> Result<(String, String)> {
        let mut ufrags = Vec::new();
        let mut pwds = Vec::new();
        for ice in std::iter::once(&self.ice).chain(self.media.as_ref().map(|m| &m.ice)) {
            if !ice.ufrag.is_empty() {
                ufrags.push(ice.ufrag.clone());
            }
            if !ice.pwd.is_empty() {
                pwds.push(ice.pwd.clone());
            }
        }

        agree(&ufrags, Error::MissingIceUfrag, Error::ConflictingIceUfrag).and_then(|ufrag| {
            agree(&pwds, Error::MissingIcePwd, Error::ConflictingIcePwd).map(|pwd| (ufrag, pwd))
        })
    }

    /// Patches the fragment's ICE credentials and candidates into a session
    /// description, as an ICE restart does.
    ///
    /// The existing candidates are removed rather than added to: a restart
    /// replaces the candidate set, and keeping the old ones would leave the
    /// peer trying addresses the restart abandoned.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidFragment`] when `a=ice-lite` or `a=ice-options`
    /// disagree with the description, or when the description has no media
    /// section with the fragment's mid.
    pub fn patch_into(&self, description: &mut SessionDescription) -> Result<()> {
        if self.ice.lite.is_some() || !self.ice.options.is_empty() {
            for attribute in &description.attributes {
                check_ice_agreement(&self.ice, attribute)?;
            }
        }

        let mid = self.mid();
        let found = !mid.is_empty()
            && description
                .media_descriptions
                .iter()
                .any(|md| mid_value(md) == mid);
        if !found {
            return Err(Error::InvalidFragment("could not find media mid"));
        }

        if let Some(media) = &self.media
            && (media.ice.lite.is_some() || !media.ice.options.is_empty())
        {
            for md in &description.media_descriptions {
                for attribute in &md.attributes {
                    check_ice_agreement(&media.ice, attribute)?;
                }
            }
        }

        if !self.ice.ufrag.is_empty() && !self.ice.pwd.is_empty() {
            for attribute in &mut description.attributes {
                if attribute.key == "ice-ufrag" {
                    attribute.value = Some(self.ice.ufrag.clone());
                } else if attribute.key == "ice-pwd" {
                    attribute.value = Some(self.ice.pwd.clone());
                }
            }
        }

        let Some(media) = &self.media else {
            return Ok(());
        };
        for md in &mut description.media_descriptions {
            for attribute in &mut md.attributes {
                if attribute.key == "ice-ufrag" && !media.ice.ufrag.is_empty() {
                    attribute.value = Some(media.ice.ufrag.clone());
                } else if attribute.key == "ice-pwd" && !media.ice.pwd.is_empty() {
                    attribute.value = Some(media.ice.pwd.clone());
                }
            }

            md.attributes
                .retain(|a| !a.is_ice_candidate() && a.key != ATTR_KEY_END_OF_CANDIDATES);
            for candidate in &media.candidates {
                md.attributes.push(Attribute::new(
                    ATTR_KEY_CANDIDATE.to_owned(),
                    Some(candidate.clone()),
                ));
            }
            if media.end_of_candidates == Some(true) {
                md.attributes
                    .push(Attribute::new(ATTR_KEY_END_OF_CANDIDATES.to_owned(), None));
            }
        }

        Ok(())
    }
}

fn check_ice_agreement(ice: &FragmentIce, attribute: &Attribute) -> Result<()> {
    match attribute.key.as_str() {
        "ice-lite" if ice.lite != Some(true) => Err(Error::InvalidFragment("ice lite mismatch")),
        "ice-options"
            if !ice.options.is_empty() && attribute.value.as_deref() != Some(&ice.options) =>
        {
            Err(Error::InvalidFragment("ice options mismatch"))
        }
        _ => Ok(()),
    }
}

/// Builds a fragment from a session description's bundle media section.
///
/// # Errors
///
/// Returns [`Error::InvalidFragment`] when the description has no BUNDLE group
/// or no media section with the bundle mid.
pub fn extract_sdp_fragment(description: &SessionDescription) -> Result<SdpFragment> {
    let Some(bundle_mid) = bundle_mid(description) else {
        return Err(Error::InvalidFragment("could not get bundle mid"));
    };

    let mut fragment = SdpFragment {
        group: description
            .attribute(ATTR_KEY_GROUP)
            .cloned()
            .unwrap_or_default(),
        ice: FragmentIce::from_attributes(&description.attributes),
        media: None,
    };

    for md in &description.media_descriptions {
        if mid_value(md) != bundle_mid {
            continue;
        }
        fragment.media = Some(FragmentMedia {
            info: media_name(md),
            mid: bundle_mid.clone(),
            ice: FragmentIce::from_attributes(&md.attributes),
            candidates: md
                .attributes
                .iter()
                .filter(|a| a.is_ice_candidate())
                .filter_map(|a| a.value.clone())
                .collect(),
            end_of_candidates: md
                .attributes
                .iter()
                .any(|a| a.key == ATTR_KEY_END_OF_CANDIDATES)
                .then_some(true),
        });
        break;
    }

    if fragment.media.is_none() {
        return Err(Error::InvalidFragment("could not find bundle media"));
    }
    Ok(fragment)
}

/// The `m=` line's value, rebuilt from its parts the way the SDP writer does.
fn media_name(media: &MediaDescription) -> String {
    let name = &media.media_name;
    let mut port = name.port.value.to_string();
    if let Some(range) = name.port.range {
        port.push('/');
        port.push_str(&range.to_string());
    }
    format!(
        "{} {} {} {}",
        name.media,
        port,
        name.protos.join("/"),
        name.formats.join(" ")
    )
}
