//! The household's groups from `ZoneGroupTopology#GetZoneGroupState`
//! (spec 13.1).
//!
//! Any speaker reports every group: its coordinator's UUID, and per member
//! a room name and the address of its device description. A player is a
//! group. Home theatre satellites (surrounds, a sub) appear as `Satellite`
//! children of their main speaker and never become players.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use url::Url;

/// One group, addressed through its coordinator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The coordinator's UUID (`RINCON_…`), which is the player id.
    pub id: String,
    /// "Couch", or "Couch + Küche" for a grouped pair, as the Sonos app
    /// names it.
    pub name: String,
    /// The coordinator's base URL (`http://192.168.0.20:1400/`), where every
    /// command for the group goes.
    pub base: Url,
}

#[derive(Debug)]
struct Member {
    uuid: String,
    zone_name: String,
    location: Option<String>,
    invisible: bool,
}

#[derive(Debug)]
struct RawGroup {
    coordinator: String,
    members: Vec<Member>,
}

/// The groups in document order. A group whose coordinator isn't among its
/// members or has no usable `Location` is left out, since nothing could be
/// sent to it.
pub fn parse_zone_groups(state: &str) -> Result<Vec<Group>, quick_xml::Error> {
    let mut reader = Reader::from_str(state);
    reader.config_mut().expand_empty_elements = true;
    let mut groups: Vec<RawGroup> = Vec::new();
    // Members only count inside `ZoneGroups`; `VanishedDevices` lists
    // speakers that dropped off the network.
    let mut in_zone_groups = false;
    loop {
        match reader.read_event()? {
            Event::Start(element) => match element.local_name().as_ref() {
                "ZoneGroups" => in_zone_groups = true,
                "ZoneGroup" if in_zone_groups => groups.push(RawGroup {
                    coordinator: attribute(&element, "Coordinator").unwrap_or_default(),
                    members: Vec::new(),
                }),
                "ZoneGroupMember" if in_zone_groups => {
                    if let Some(group) = groups.last_mut() {
                        group.members.push(Member {
                            uuid: attribute(&element, "UUID").unwrap_or_default(),
                            zone_name: attribute(&element, "ZoneName").unwrap_or_default(),
                            location: attribute(&element, "Location"),
                            invisible: attribute(&element, "Invisible").as_deref() == Some("1"),
                        });
                    }
                }
                _ => {}
            },
            Event::End(element) if element.local_name().as_ref() == "ZoneGroups" => {
                in_zone_groups = false;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(groups
        .into_iter()
        .filter_map(RawGroup::into_group)
        .collect())
}

impl RawGroup {
    fn into_group(self) -> Option<Group> {
        let coordinator = self
            .members
            .iter()
            .find(|member| member.uuid == self.coordinator)?;
        let base = base_url(coordinator.location.as_deref()?)?;
        let others = self
            .members
            .iter()
            .filter(|member| member.uuid != self.coordinator && !member.invisible)
            .map(|member| member.zone_name.as_str());
        let name = std::iter::once(coordinator.zone_name.as_str())
            .chain(others)
            .collect::<Vec<_>>()
            .join(" + ");
        Some(Group {
            id: self.coordinator,
            name,
            base,
        })
    }
}

/// The speaker's origin from its device description URL
/// (`http://192.168.0.20:1400/xml/device_description.xml`); the control
/// URLs are absolute paths on it.
fn base_url(location: &str) -> Option<Url> {
    let mut url = Url::parse(location).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
        return None;
    }
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Some(url)
}

fn attribute(element: &BytesStart<'_>, name: &str) -> Option<String> {
    let attribute = element.try_get_attribute(name).ok().flatten()?;
    let value = attribute
        .normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .ok()?;
    Some(value.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sonos::upnp;

    fn group(id: &str, name: &str, base: &str) -> Group {
        Group {
            id: id.into(),
            name: name.into(),
            base: Url::parse(base).expect("valid URL literal"),
        }
    }

    #[test]
    fn reads_the_captured_household() {
        let body = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sonos/zone_group_state.xml"
        ))
        .expect("the fixture exists");
        let response =
            upnp::interpret("GetZoneGroupState", 200, &body).expect("a successful response");
        let groups = parse_zone_groups(&response["ZoneGroupState"]).expect("the state is XML");
        // The Beam's two satellites share its room name and never show up,
        // neither as players nor in the name.
        assert_eq!(
            groups,
            [
                group(
                    "RINCON_000E5800000101400",
                    "Küche",
                    "http://192.0.2.13:1400/"
                ),
                group(
                    "RINCON_000E5800000201400",
                    "Couch",
                    "http://192.0.2.45:1400/"
                ),
            ]
        );
    }

    #[test]
    fn grouped_rooms_are_named_coordinator_first() {
        let state = r#"<ZoneGroupState><ZoneGroups>
            <ZoneGroup Coordinator="RINCON_B" ID="RINCON_B:1">
              <ZoneGroupMember UUID="RINCON_K" Location="http://192.0.2.13:1400/xml/device_description.xml" ZoneName="Küche"/>
              <ZoneGroupMember UUID="RINCON_SUB" Location="http://192.0.2.99:1400/xml/device_description.xml" ZoneName="Sub" Invisible="1"/>
              <ZoneGroupMember UUID="RINCON_B" Location="http://192.0.2.45:1400/xml/device_description.xml" ZoneName="Couch">
                <Satellite UUID="RINCON_S1" Location="http://192.0.2.31:1400/xml/device_description.xml" ZoneName="Couch" Invisible="1"/>
              </ZoneGroupMember>
            </ZoneGroup>
            <ZoneGroup Coordinator="RINCON_GONE" ID="RINCON_GONE:2">
              <ZoneGroupMember UUID="RINCON_OTHER" Location="http://192.0.2.60:1400/x.xml" ZoneName="Elsewhere"/>
            </ZoneGroup>
          </ZoneGroups>
          <VanishedDevices><ZoneGroupMember UUID="RINCON_V" Location="http://192.0.2.70:1400/x.xml" ZoneName="Vanished"/></VanishedDevices>
        </ZoneGroupState>"#;
        assert_eq!(
            parse_zone_groups(state).expect("well-formed XML"),
            [group(
                "RINCON_B",
                "Couch + Küche",
                "http://192.0.2.45:1400/"
            )]
        );
    }
}
