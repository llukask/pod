//! The UPnP (SOAP over HTTP) actions pod-server sends to one Sonos speaker
//! (spec 13).
//!
//! Requests are built and responses parsed by pure functions, so the parsing
//! is tested against responses captured from real speakers
//! (`tests/fixtures/sonos/`). [`Speaker`] only adds the HTTP round trip.

use std::collections::HashMap;
use std::time::Duration;

use quick_xml::Reader;
use quick_xml::escape::{escape, resolve_predefined_entity};
use quick_xml::events::Event;
use url::Url;

use super::tracker::TransportState;

/// Speakers sit on the LAN and answer within milliseconds, so a speaker
/// that takes longer than this is treated as gone.
const TIMEOUT: Duration = Duration::from_secs(5);

// ==============================================================================
// Services
// ==============================================================================

#[derive(Debug, Clone, Copy)]
enum Service {
    AvTransport,
    GroupRenderingControl,
    ZoneGroupTopology,
}

impl Service {
    fn control_path(self) -> &'static str {
        match self {
            Self::AvTransport => "/MediaRenderer/AVTransport/Control",
            Self::GroupRenderingControl => "/MediaRenderer/GroupRenderingControl/Control",
            Self::ZoneGroupTopology => "/ZoneGroupTopology/Control",
        }
    }

    fn urn(self) -> &'static str {
        match self {
            Self::AvTransport => "urn:schemas-upnp-org:service:AVTransport:1",
            Self::GroupRenderingControl => "urn:schemas-upnp-org:service:GroupRenderingControl:1",
            Self::ZoneGroupTopology => "urn:schemas-upnp-org:service:ZoneGroupTopology:1",
        }
    }
}

// ==============================================================================
// Errors
// ==============================================================================

/// Why an action failed. Every variant is a `502 upstream_unavailable` for
/// the client.
#[derive(Debug, thiserror::Error)]
pub enum UpnpError {
    #[error("the speaker didn't answer {action}")]
    Unreachable {
        action: &'static str,
        #[source]
        source: reqwest::Error,
    },
    /// A SOAP fault. Sonos answers HTTP 500 with the UPnP error code, for
    /// example 717 for a `Play` speed other than 1.
    #[error("the speaker refused {action} with UPnP error {code:?}")]
    Fault {
        action: &'static str,
        code: Option<u32>,
    },
    #[error("the speaker answered {action} with HTTP {status}")]
    Status { action: &'static str, status: u16 },
    #[error("the speaker's answer to {action} is unusable: {detail}")]
    Body {
        action: &'static str,
        detail: String,
    },
}

impl UpnpError {
    fn body(action: &'static str, detail: impl Into<String>) -> Self {
        Self::Body {
            action,
            detail: detail.into(),
        }
    }
}

// ==============================================================================
// Requests
// ==============================================================================

/// The SOAP envelope for one action. Argument values are XML-escaped here,
/// so a value that is itself an XML document (the DIDL-Lite metadata) ends
/// up escaped twice, as the speaker expects.
fn envelope(service: Service, action: &str, args: &[(&str, &str)]) -> String {
    let args: String = args
        .iter()
        .map(|(name, value)| format!("<{name}>{}</{name}>", escape(*value)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:{action} xmlns:u="{urn}">{args}</u:{action}></s:Body></s:Envelope>"#,
        urn = service.urn(),
    )
}

/// What the Sonos app shows for an episode.
#[derive(Debug, Clone, Copy)]
pub struct TrackMetadata<'a> {
    pub audio_url: &'a str,
    /// The enclosure's MIME type; `audio/mpeg` when the feed names none.
    pub mime_type: Option<&'a str>,
    pub title: &'a str,
    /// The podcast's title, shown as artist and album.
    pub show: Option<&'a str>,
    pub artwork_url: Option<&'a str>,
}

/// The DIDL-Lite document for `CurrentURIMetaData`. Every value is
/// escaped once here; [`envelope`] escapes the whole document again.
pub fn didl_lite(track: &TrackMetadata<'_>) -> String {
    let mime_type = track.mime_type.unwrap_or("audio/mpeg");
    let mut item = format!(
        r#"<res protocolInfo="http-get:*:{}:*">{}</res><dc:title>{}</dc:title>"#,
        escape(mime_type),
        escape(track.audio_url),
        escape(track.title),
    );
    if let Some(show) = track.show {
        let show = escape(show);
        item.push_str(&format!(
            "<dc:creator>{show}</dc:creator><upnp:album>{show}</upnp:album>"
        ));
    }
    if let Some(artwork_url) = track.artwork_url {
        item.push_str(&format!(
            "<upnp:albumArtURI>{}</upnp:albumArtURI>",
            escape(artwork_url)
        ));
    }
    format!(
        r#"<DIDL-Lite xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" xmlns:r="urn:schemas-rinconnetworks-com:metadata-1-0/" xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"><item id="-1" parentID="-1" restricted="true">{item}<upnp:class>object.item.audioItem.musicTrack</upnp:class></item></DIDL-Lite>"#
    )
}

/// `H:MM:SS` for `Seek`'s `REL_TIME` target. Speakers seek in whole
/// seconds, so the milliseconds are dropped.
pub fn format_rel_time(position_ms: i64) -> String {
    let seconds = position_ms.max(0) / 1000;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

// ==============================================================================
// Responses
// ==============================================================================

/// The text of every element without child elements, by local name, with
/// entities resolved. The first occurrence of a name wins. Every response
/// pod-server reads keeps its values in uniquely named leaves
/// (`CurrentTransportState`, `RelTime`, `errorCode`, ...), so this one
/// reader serves them all.
pub fn leaves(xml: &str) -> Result<HashMap<String, String>, quick_xml::Error> {
    struct Open {
        name: String,
        text: String,
        has_children: bool,
    }
    let mut reader = Reader::from_str(xml);
    // `<NextURI/>` then arrives as a start and an end, and counts as an
    // empty leaf.
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<Open> = Vec::new();
    let mut found = HashMap::new();
    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                if let Some(parent) = stack.last_mut() {
                    parent.has_children = true;
                }
                stack.push(Open {
                    name: element.local_name().as_ref().to_owned(),
                    text: String::new(),
                    has_children: false,
                });
            }
            Event::End(_) => {
                if let Some(open) = stack.pop()
                    && !open.has_children
                {
                    found.entry(open.name).or_insert(open.text);
                }
            }
            Event::Text(chunk) => {
                if let Some(open) = stack.last_mut() {
                    open.text.push_str(&chunk.xml10_content());
                }
            }
            Event::CData(chunk) => {
                if let Some(open) = stack.last_mut() {
                    open.text.push_str(&chunk.xml10_content());
                }
            }
            // Text arrives split around entity references, which is where
            // the escaped XML inside `ZoneGroupState` and `TrackMetaData`
            // gets unescaped.
            Event::GeneralRef(reference) => {
                if let Some(open) = stack.last_mut() {
                    if let Ok(Some(ch)) = reference.resolve_char_ref() {
                        open.text.push(ch);
                    } else if let Some(resolved) = resolve_predefined_entity(&reference) {
                        open.text.push_str(resolved);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(found)
}

/// Reads an action's response from its HTTP status and body: the leaf
/// values on success, the UPnP error code on a SOAP fault.
pub fn interpret(
    action: &'static str,
    status: u16,
    body: &str,
) -> Result<HashMap<String, String>, UpnpError> {
    let parsed = leaves(body);
    if (200..300).contains(&status) {
        return parsed.map_err(|error| UpnpError::body(action, error.to_string()));
    }
    match parsed {
        Ok(fields) if fields.contains_key("faultcode") => Err(UpnpError::Fault {
            action,
            code: fields
                .get("errorCode")
                .and_then(|code| code.trim().parse().ok()),
        }),
        _ => Err(UpnpError::Status { action, status }),
    }
}

fn field<'a>(
    fields: &'a HashMap<String, String>,
    action: &'static str,
    name: &str,
) -> Result<&'a str, UpnpError> {
    fields
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| UpnpError::body(action, format!("no {name}")))
}

/// Milliseconds from `H:MM:SS`, as `RelTime` reports them. `None` for
/// anything else, such as `NOT_IMPLEMENTED`.
pub fn parse_rel_time(text: &str) -> Option<i64> {
    let mut parts = text.trim().split(':');
    let (hours, minutes, seconds) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let number = |part: &str| {
        (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| part.parse::<i64>().ok())
            .flatten()
    };
    let (hours, minutes, seconds) = (number(hours)?, number(minutes)?, number(seconds)?);
    if minutes >= 60 || seconds >= 60 {
        return None;
    }
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000)
}

// ==============================================================================
// Speaker
// ==============================================================================

/// One speaker's UPnP endpoint, addressed by its base URL
/// (`http://192.168.0.20:1400/`). Cheap to clone.
#[derive(Debug, Clone)]
pub struct Speaker {
    http: reqwest::Client,
    base: Url,
}

/// The client every [`Speaker`] shares.
pub fn http_client() -> reqwest::Result<reqwest::Client> {
    // Speakers are on the LAN, so a proxy from the environment would only
    // get in the way.
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .no_proxy()
        .build()
}

impl Speaker {
    pub fn new(http: reqwest::Client, base: Url) -> Self {
        Self { http, base }
    }

    async fn call(
        &self,
        service: Service,
        action: &'static str,
        args: &[(&str, &str)],
    ) -> Result<HashMap<String, String>, UpnpError> {
        let unreachable = |source| UpnpError::Unreachable { action, source };
        let mut url = self.base.clone();
        url.set_path(service.control_path());
        let response = self
            .http
            .post(url)
            .header("Content-Type", r#"text/xml; charset="utf-8""#)
            .header("SOAPACTION", format!("\"{}#{action}\"", service.urn()))
            .body(envelope(service, action, args))
            .send()
            .await
            .map_err(unreachable)?;
        let status = response.status().as_u16();
        let body = response.text().await.map_err(unreachable)?;
        interpret(action, status, &body)
    }

    pub async fn set_av_transport_uri(&self, uri: &str, metadata: &str) -> Result<(), UpnpError> {
        self.call(
            Service::AvTransport,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", uri),
                ("CurrentURIMetaData", metadata),
            ],
        )
        .await
        .map(drop)
    }

    pub async fn seek(&self, position_ms: i64) -> Result<(), UpnpError> {
        let target = format_rel_time(position_ms);
        self.call(
            Service::AvTransport,
            "Seek",
            &[
                ("InstanceID", "0"),
                ("Unit", "REL_TIME"),
                ("Target", &target),
            ],
        )
        .await
        .map(drop)
    }

    /// Plays at speed 1, the only speed Sonos accepts (UPnP error 717
    /// otherwise).
    pub async fn play(&self) -> Result<(), UpnpError> {
        self.call(
            Service::AvTransport,
            "Play",
            &[("InstanceID", "0"), ("Speed", "1")],
        )
        .await
        .map(drop)
    }

    pub async fn pause(&self) -> Result<(), UpnpError> {
        self.call(Service::AvTransport, "Pause", &[("InstanceID", "0")])
            .await
            .map(drop)
    }

    pub async fn transport_state(&self) -> Result<TransportState, UpnpError> {
        const ACTION: &str = "GetTransportInfo";
        let fields = self
            .call(Service::AvTransport, ACTION, &[("InstanceID", "0")])
            .await?;
        Ok(TransportState::parse(field(
            &fields,
            ACTION,
            "CurrentTransportState",
        )?))
    }

    /// The position within the current track, in whole seconds.
    pub async fn position_ms(&self) -> Result<i64, UpnpError> {
        const ACTION: &str = "GetPositionInfo";
        let fields = self
            .call(Service::AvTransport, ACTION, &[("InstanceID", "0")])
            .await?;
        let rel_time = field(&fields, ACTION, "RelTime")?;
        parse_rel_time(rel_time)
            .ok_or_else(|| UpnpError::body(ACTION, format!("RelTime {rel_time:?}")))
    }

    /// The URI the group holds, which stays the episode's `audio_url`
    /// while the group plays it.
    pub async fn current_uri(&self) -> Result<String, UpnpError> {
        const ACTION: &str = "GetMediaInfo";
        let fields = self
            .call(Service::AvTransport, ACTION, &[("InstanceID", "0")])
            .await?;
        field(&fields, ACTION, "CurrentURI").map(str::to_owned)
    }

    pub async fn group_volume(&self) -> Result<u8, UpnpError> {
        const ACTION: &str = "GetGroupVolume";
        let fields = self
            .call(
                Service::GroupRenderingControl,
                ACTION,
                &[("InstanceID", "0")],
            )
            .await?;
        let volume = field(&fields, ACTION, "CurrentVolume")?;
        volume
            .trim()
            .parse::<u8>()
            .ok()
            .filter(|volume| *volume <= 100)
            .ok_or_else(|| UpnpError::body(ACTION, format!("CurrentVolume {volume:?}")))
    }

    pub async fn set_group_volume(&self, volume: u8) -> Result<(), UpnpError> {
        self.call(
            Service::GroupRenderingControl,
            "SetGroupVolume",
            &[("InstanceID", "0"), ("DesiredVolume", &volume.to_string())],
        )
        .await
        .map(drop)
    }

    /// The household topology as the speaker reports it, an XML document
    /// for [`super::topology::parse_zone_groups`].
    pub async fn zone_group_state(&self) -> Result<String, UpnpError> {
        const ACTION: &str = "GetZoneGroupState";
        let mut fields = self.call(Service::ZoneGroupTopology, ACTION, &[]).await?;
        fields
            .remove("ZoneGroupState")
            .ok_or_else(|| UpnpError::body(ACTION, "no ZoneGroupState"))
    }
}

/// The action name in a `SOAPACTION` header (`"urn:...:AVTransport:1#Play"`),
/// for test stubs that answer like a speaker.
pub fn soap_action(header: &str) -> Option<&str> {
    let (_, action) = header.trim_matches('"').rsplit_once('#')?;
    Some(action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/sonos/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    #[test]
    fn reads_the_captured_responses() {
        let transport = interpret(
            "GetTransportInfo",
            200,
            &fixture("transport_info_stopped.xml"),
        )
        .expect("a successful response");
        assert_eq!(
            TransportState::parse(&transport["CurrentTransportState"]),
            TransportState::Stopped
        );

        let position = interpret("GetPositionInfo", 200, &fixture("position_info_queue.xml"))
            .expect("a successful response");
        assert_eq!(parse_rel_time(&position["RelTime"]), Some(0));
        assert_eq!(
            position["TrackURI"],
            "x-sonos-spotify:spotify%3atrack%3a0000000000000000000000?sid=9&flags=8232&sn=2"
        );
        // The track metadata is a document of its own, unescaped once.
        let track = leaves(&position["TrackMetaData"]).expect("the metadata is XML");
        assert_eq!(track["title"], "Breaking the Habit");

        let media = interpret("GetMediaInfo", 200, &fixture("media_info_queue.xml"))
            .expect("a successful response");
        assert_eq!(
            media["CurrentURI"],
            "x-rincon-queue:RINCON_000E5800000201400#0"
        );
        assert_eq!(media["NextURI"], "");

        let volume = interpret("GetGroupVolume", 200, &fixture("group_volume.xml"))
            .expect("a successful response");
        assert_eq!(volume["CurrentVolume"], "10");
    }

    #[test]
    fn a_soap_fault_carries_its_upnp_error_code() {
        let error = interpret("Play", 500, &fixture("error_play_speed.xml"))
            .expect_err("a fault is an error");
        assert!(
            matches!(
                error,
                UpnpError::Fault {
                    action: "Play",
                    code: Some(717)
                }
            ),
            "{error:?}"
        );

        let error = interpret("Play", 503, "<html>busy</html>").expect_err("not a fault");
        assert!(
            matches!(
                error,
                UpnpError::Status {
                    action: "Play",
                    status: 503
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn rel_times_parse_and_format() {
        assert_eq!(parse_rel_time("0:11:23"), Some(683_000));
        assert_eq!(parse_rel_time("12:00:01"), Some(43_201_000));
        for invalid in [
            "NOT_IMPLEMENTED",
            "",
            "1:2",
            "0:60:00",
            "0:00:-1",
            "1:00:00:00",
        ] {
            assert_eq!(parse_rel_time(invalid), None, "{invalid:?}");
        }
        assert_eq!(format_rel_time(683_999), "0:11:23");
        assert_eq!(format_rel_time(43_201_000), "12:00:01");
        assert_eq!(format_rel_time(0), "0:00:00");
    }

    /// The metadata is escaped twice on the way out, as a document and as
    /// a SOAP argument, so reading the request back takes two passes, and
    /// every character survives them.
    #[test]
    fn metadata_survives_both_levels_of_escaping() {
        let title = r#"Fish & Chips <live> "Grüße" from Köln's Café"#;
        let metadata = didl_lite(&TrackMetadata {
            audio_url: "https://media.example.org/ep.mp3?a=1&b=2",
            mime_type: None,
            title,
            show: Some("Ärger & Ö"),
            artwork_url: Some("https://img.example.org/a.jpg?s=1&t=2"),
        });
        let request = envelope(
            Service::AvTransport,
            "SetAVTransportURI",
            &[
                ("InstanceID", "0"),
                ("CurrentURI", "https://media.example.org/ep.mp3?a=1&b=2"),
                ("CurrentURIMetaData", &metadata),
            ],
        );
        assert!(!request.contains("<DIDL-Lite"), "{request}");

        let arguments = leaves(&request).expect("the envelope is XML");
        assert_eq!(arguments["CurrentURIMetaData"], metadata);
        assert_eq!(
            arguments["CurrentURI"],
            "https://media.example.org/ep.mp3?a=1&b=2"
        );
        let didl = leaves(&arguments["CurrentURIMetaData"]).expect("the metadata is XML");
        assert_eq!(didl["title"], title);
        assert_eq!(didl["creator"], "Ärger & Ö");
        assert_eq!(didl["album"], "Ärger & Ö");
        assert_eq!(didl["albumArtURI"], "https://img.example.org/a.jpg?s=1&t=2");
        assert_eq!(didl["res"], "https://media.example.org/ep.mp3?a=1&b=2");
        assert_eq!(didl["class"], "object.item.audioItem.musicTrack");
        assert!(
            arguments["CurrentURIMetaData"].contains(r#"protocolInfo="http-get:*:audio/mpeg:*""#)
        );
    }

    #[test]
    fn soap_actions_name_the_action() {
        assert_eq!(
            soap_action(r#""urn:schemas-upnp-org:service:AVTransport:1#Play""#),
            Some("Play")
        );
        assert_eq!(soap_action("Play"), None);
    }
}
