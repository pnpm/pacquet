//! Edge-case unit tests for defensive fallbacks in [`pacquet_modules_yaml`].
//!
//! These tests cover branches that exist purely to keep the crate
//! tolerant of unknown shapes. Examples include a non-object
//! `hoistedAliases`, mixed-type `skipped` entries, and a non-string
//! `hoistPattern`. pnpm itself never emits these shapes; they are guard
//! rails for downstream code that might deserialize a corrupt manifest.
//! The tests exist to lock in the fallback behavior and to close
//! coverage holes that pnpm-sourced fixtures cannot reach.
//!
//! These tests are lower priority than the ports in
//! `crates/modules-yaml/tests/index.rs`. A regression in any of them means
//! the crate became *less* tolerant of garbage, not that it broke a real
//! pnpm flow.

use super::{
    HoistKind, derive_hoisted_dependencies, drop_empty_hoist_fields, is_empty_or_null, sort_skipped,
};
use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};

/// `derive_hoisted_dependencies` returns an empty `Object` when the input
/// is not an object at all. The fallback guards against a corrupt manifest
/// where `hoistedAliases` is somehow `null`, an array, or another shape.
#[test]
fn derive_hoisted_dependencies_returns_empty_object_for_non_object_input() {
    let result = derive_hoisted_dependencies(&Value::Null, HoistKind::Public);
    assert_eq!(result, Value::Object(Map::new()));

    let result = derive_hoisted_dependencies(&json!(["unexpected"]), HoistKind::Private);
    assert_eq!(result, Value::Object(Map::new()));
}

/// When an entry under `hoistedAliases` is not an array, the whole entry
/// is preserved with an empty alias map. Pnpm only writes `string[]` for
/// each value, but the fallback keeps the dep-path key so downstream code
/// has *something* to look up.
#[test]
fn derive_hoisted_dependencies_inserts_empty_entry_for_non_array_alias_list() {
    let aliases = json!({ "/foo/1.0.0": "not-an-array", "/bar/2.0.0": ["bar"] });
    let result = derive_hoisted_dependencies(&aliases, HoistKind::Public);
    assert_eq!(result, json!({ "/foo/1.0.0": {}, "/bar/2.0.0": { "bar": "public" } }));
}

/// `sort_skipped`'s comparator returns `Ordering::Equal` when either
/// element isn't a string. Stable sort then preserves input order for
/// any pair where at least one side is non-string, so a mixed-type
/// `skipped` array is left effectively as-is rather than panicking.
///
/// Limit of this test: a wrong fallback value is only partially detectable
/// because Rust's stable sort short-circuits when adjacent comparisons
/// agree with the existing layout. For `[3, 2, 1]`:
///   - `Equal` (correct)   → `[3, 2, 1]` (no swaps).
///   - `Less`              → `[1, 2, 3]` (sort reverses; this test catches it).
///   - `Greater`           → `[3, 2, 1]` (sort sees descending order as
///     already-sorted under that comparator; **this test misses it**).
///
/// Catching the `Greater` case would require building a full custom
/// comparator harness. The trade-off is not worth it for a defensive
/// fallback that pnpm never triggers.
#[test]
fn sort_skipped_returns_equal_for_non_string_pairs() {
    let mut fields = Map::new();
    fields.insert("skipped".to_string(), json!([3, 2, 1]));
    sort_skipped(&mut fields);
    assert_eq!(fields.get("skipped").unwrap(), &json!([3, 2, 1]));
}

/// `drop_empty_hoist_fields` keeps `hoistedAliases` when *both*
/// `hoistPattern` and `publicHoistPattern` are present. This covers the
/// `_ => !contains && !contains` arm of the fallthrough match.
#[test]
fn drop_empty_hoist_fields_keeps_hoisted_aliases_when_patterns_present() {
    let mut fields = Map::new();
    fields.insert("hoistPattern".to_string(), json!(["*"]));
    fields.insert("publicHoistPattern".to_string(), json!(["*"]));
    fields.insert("hoistedAliases".to_string(), json!({ "/foo/1.0.0": ["foo"] }));

    drop_empty_hoist_fields(&mut fields);

    assert!(fields.contains_key("hoistPattern"));
    assert!(fields.contains_key("publicHoistPattern"));
    assert!(fields.contains_key("hoistedAliases"));
}

/// `is_empty_or_null` treats present-non-empty-strings as non-empty and
/// every other concrete `Value` shape as non-empty. The fallthrough arms
/// are locked in so a future refactor cannot quietly broaden the "empty"
/// predicate.
#[test]
fn is_empty_or_null_returns_false_for_non_empty_string_and_other_shapes() {
    assert!(!is_empty_or_null(Some(&Value::String("non-empty".to_string()))));
    assert!(!is_empty_or_null(Some(&json!(42))));
    assert!(!is_empty_or_null(Some(&json!(true))));
    assert!(!is_empty_or_null(Some(&json!([]))));

    // Sanity: the documented "empty" cases still return true.
    assert!(is_empty_or_null(None));
    assert!(is_empty_or_null(Some(&Value::Null)));
    assert!(is_empty_or_null(Some(&Value::String(String::new()))));
}
