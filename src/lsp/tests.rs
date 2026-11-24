use super::protocol::*;
use serde_json::{Value, json};

#[test]
fn utf16_offsets_handle_surrogates_crlf_and_invalid_positions() {
    let text = "é😀\r\nfn test() {}\n";
    assert_eq!(
        position(text, 2),
        Position {
            line: 0,
            character: 3
        }
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 0,
                character: 3
            }
        ),
        Some(2)
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 0,
                character: 2
            }
        ),
        None
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 1,
                character: 0
            }
        ),
        Some(4)
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 3,
                character: 0
            }
        ),
        None
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 0,
                character: 4
            }
        ),
        None
    );
    assert_eq!(
        offset(
            text,
            Position {
                line: 2,
                character: 0
            }
        ),
        Some(text.chars().count())
    );
}
#[test]
fn incremental_replacement_round_trips_edits() {
    for (old, new) in [
        ("é😀 one\r\nend", "é😀 two\r\nend"),
        ("", "😀"),
        ("hello", ""),
        ("abc", "a\nβc"),
        ("unchanged", "unchanged"),
        ("a\r\nb", "a\nb"),
        ("a\nb", "a\r\nb"),
        ("a\r\nb", "a\r\nβ"),
        ("a\r\nb", "a\rX\nb"),
    ] {
        let edit = change(old, new);
        let range: Range = serde_json::from_value(edit["range"].clone()).unwrap();
        let start = offset(old, range.start).unwrap();
        let end = offset(old, range.end).unwrap();
        let result = old.chars().take(start).collect::<String>()
            + edit["text"].as_str().unwrap()
            + &old.chars().skip(end).collect::<String>();
        assert_eq!(result, new);
    }
}
fn range(start: (u32, u32), end: (u32, u32)) -> Value {
    json!({"start":{"line":start.0,"character":start.1},"end":{"line":end.0,"character":end.1}})
}
#[test]
fn hierarchical_and_flat_symbols_have_safe_targets() {
    let text = "class C:\n    def f(self):\n        pass\n";
    let symbols_json = json!([{"name":"C","kind":5,"range":range((0,0),(2,12)),"selectionRange":range((0,6),(0,7)),"children":[{"name":"f","kind":6,"range":range((1,4),(2,12)),"selectionRange":range((1,8),(1,9))}]}]);
    let list = symbols(&symbols_json, text, "file:///a.py").unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].parent, Some(0));
    assert_eq!(list[1].target, 17);
    let flat = json!([{"name":"C","kind":5,"location":{"uri":"file:///a.py","range":range((0,0),(2,12))}},{"name":"f","kind":6,"location":{"uri":"file:///a.py","range":range((1,8),(1,9))}},{"name":"other","location":{"uri":"file:///else.py","range":range((0,0),(0,1))}}]);
    let list = symbols(&flat, text, "file:///a.py").unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].parent, Some(0));
    assert!(!list[1].enclosing);
    assert!(
        symbols(&json!(null), text, "file:///a.py")
            .unwrap()
            .is_empty()
    );
    let invalid = json!([{"name":"bad","range":range((99,0),(99,3))}]);
    assert!(symbols(&invalid, text, "file:///a.py").unwrap().is_empty());
}
