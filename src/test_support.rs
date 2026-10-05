//! Helpers shared by several modules' tests.

use std::collections::BTreeSet;

use serde_json::Value;

/// Asserts a dial's feedback and its shipped layout agree both ways: every
/// feedback key has a layout item to land in, and every layout item is fed
/// (an unfed item would show the layout's placeholder forever) - except
/// `static_keys`, fixed labels whose text the layout itself supplies.
pub fn assert_feedback_matches_layout(layout_json: &str, feedback: &Value, static_keys: &[&str]) {
    let layout: Value = serde_json::from_str(layout_json).unwrap();
    let items = layout["items"].as_array().unwrap();
    let feedback_keys: BTreeSet<&str> = feedback
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for k in static_keys {
        let item = items
            .iter()
            .find(|i| i["key"] == *k)
            .unwrap_or_else(|| panic!("layout has no static item keyed {k}"));
        assert!(item["value"].is_string(), "static item {k} has no value");
        assert!(!feedback_keys.contains(k), "feedback overwrites static {k}");
    }
    let fed_keys: BTreeSet<&str> = items
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .filter(|k| !static_keys.contains(k))
        .collect();
    let unplaced: Vec<_> = feedback_keys.difference(&fed_keys).collect();
    assert!(unplaced.is_empty(), "layout has no item keyed {unplaced:?}");
    let unfed: Vec<_> = fed_keys.difference(&feedback_keys).collect();
    assert!(
        unfed.is_empty(),
        "feedback never sets layout items {unfed:?}"
    );
}

/// The shipped manifest.
pub fn manifest() -> Value {
    serde_json::from_str(include_str!("../assets/manifest.json")).unwrap()
}

/// The manifest entry for action `uuid`, found by UUID (not by position,
/// so reordering the manifest's action list breaks nothing) and asserted
/// to be the only one.
pub fn manifest_entry(uuid: &str) -> Value {
    let manifest = manifest();
    let matches: Vec<&Value> = manifest["Actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["UUID"] == uuid)
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "manifest has {} entries for {uuid}",
        matches.len()
    );
    matches[0].clone()
}
