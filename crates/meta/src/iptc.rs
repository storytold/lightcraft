use crate::Metadata;
use crate::convert::from_craft;

pub fn parse_iptc(bytes: &[u8]) -> Metadata {
    from_craft(craft_metadata::parse_iptc(bytes))
}
