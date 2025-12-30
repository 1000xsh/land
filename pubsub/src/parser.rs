use crate::types::{SlotInfo, SubscriptionId};

// parse subscription response: {"jsonrpc":"2.0","result":5308752,"id":1}
// returns the subscription id from the "result" field
pub fn parse_subscription_response(json: &str) -> Option<SubscriptionId> {
    extract_u64_field(json, "result").map(SubscriptionId::new)
}

// parse slot notification: {"jsonrpc":"2.0","method":"slotNotification","params":{"result":{"slot":12345,"parent":12344,"root":12340},"subscription":5308752}}
// returns slotinfo with slot, parent, root
pub fn parse_slot_notification(json: &str) -> Option<SlotInfo> {
    // find the "result" object which contains slot, parent, root
    let result_start = json.find(r#""result":"#)?;
    let result_json = &json[result_start + 9..]; // skip `"result":`

    // extract slot, parent, root from the result object
    let slot = extract_u64_field(result_json, "slot")?;
    let parent = extract_u64_field(result_json, "parent")?;
    let root = extract_u64_field(result_json, "root")?;

    Some(SlotInfo {
        slot,
        parent,
        root,
    })
}

// extract u64 field from json string
// example: extract_u64_field(r#"{"slot":12345,"parent":12344}"#, "slot") -> Some(12345)
#[inline]
fn extract_u64_field(json: &str, field: &str) -> Option<u64> {
    // construct pattern: "field":
    let pattern = format!(r#""{}":"#, field);
    let pos = json.find(&pattern)?;
    let start = pos + pattern.len();
    let rest = &json[start..];

    // find end of number (first non-digit character)
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());

    // parse the number
    rest[..end].parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_subscription_response() {
        let json = r#"{"jsonrpc":"2.0","result":5308752,"id":1}"#;
        let sub_id = parse_subscription_response(json).unwrap();
        assert_eq!(sub_id.get(), 5308752);
    }

    #[test]
    fn test_parse_slot_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"slotNotification","params":{"result":{"slot":12345,"parent":12344,"root":12340},"subscription":5308752}}"#;
        let info = parse_slot_notification(json).unwrap();
        assert_eq!(info.slot, 12345);
        assert_eq!(info.parent, 12344);
        assert_eq!(info.root, 12340);
    }

    #[test]
    fn test_extract_u64_field() {
        let json = r#"{"slot":12345,"parent":12344,"root":12340}"#;
        assert_eq!(extract_u64_field(json, "slot"), Some(12345));
        assert_eq!(extract_u64_field(json, "parent"), Some(12344));
        assert_eq!(extract_u64_field(json, "root"), Some(12340));
        assert_eq!(extract_u64_field(json, "missing"), None);
    }
}
