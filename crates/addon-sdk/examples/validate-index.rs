//! Validate the central repository registry without building the editor.
use std::io::{self, Read};
fn main() {
    let mut input = String::new();
    io::stdin()
        .take((focus_addon_sdk::MAX_MESSAGE + 1) as u64)
        .read_to_string(&mut input)
        .expect("read addon index");
    assert!(
        input.len() <= focus_addon_sdk::MAX_MESSAGE,
        "Addon index exceeds size limit"
    );
    let index: focus_addon_sdk::AddonIndex =
        serde_json::from_str(&input).expect("parse addon index");
    index.validate().expect("validate addon index");
}
