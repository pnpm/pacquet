use crate::HashEncoding;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Mirrors `hashObject` from
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/crypto/object-hasher/src/index.ts#L41>
/// (sorted keys, sha256, base64).
///
/// The bytestream the library writes before hashing is described in
/// [`serialize`] below — it must match pnpm's byte-for-byte because
/// the result is persisted on disk and shared with pnpm.
///
/// `undefined` in JS maps to no Rust value here; the upstream
/// short-circuit `hashUnknown(undefined)` returns 44 zero characters
/// regardless of options. Callers who need that semantic should
/// branch on the optional before calling.
pub fn hash_object(value: &Value) -> String {
    hash_object_with_encoding(value, HashEncoding::Base64, /* sort */ true)
}

/// Mirrors `hashObjectWithoutSorting` at
/// <https://github.com/pnpm/pnpm/blob/b4f8f47ac2/crypto/object-hasher/src/index.ts#L37>.
pub fn hash_object_without_sorting(value: &Value, encoding: HashEncoding) -> String {
    hash_object_with_encoding(value, encoding, /* sort */ false)
}

/// General form. `sort = true` sorts object keys before serialization
/// (the `unorderedObjects` option upstream); `sort = false` preserves
/// insertion order.
pub fn hash_object_with_encoding(value: &Value, encoding: HashEncoding, sort: bool) -> String {
    let mut bytes = Vec::new();
    serialize(&mut bytes, value, sort);
    let digest = Sha256::digest(&bytes);
    match encoding {
        HashEncoding::Base64 => BASE64.encode(digest),
        HashEncoding::Hex => format!("{digest:x}"),
    }
}

/// Recursive bytestream serializer mirroring the `typeHasher` dispatch
/// in object-hash@3.0.0 with `respectType: false`. Only the type
/// arms pacquet actually feeds in are implemented here — `Value` is
/// the union of String / Number / Bool / Null / Array / Object,
/// which covers every input pacquet's graph-hasher and the upstream
/// `hashObject`-using callers ever pass. Anything else would either
/// be unreachable for pacquet or require porting upstream's Date /
/// Set / Map / Buffer arms — explicitly not in scope here.
fn serialize(out: &mut Vec<u8>, value: &Value, sort: bool) {
    match value {
        Value::Null => {
            // object-hash writes the literal string `Null` (uppercase
            // `N`) for null values at index.js:334.
            out.extend_from_slice(b"Null");
        }
        Value::Bool(b) => {
            // index.js:301-303.
            out.extend_from_slice(b"bool:");
            out.extend_from_slice(if *b { b"true" } else { b"false" });
        }
        Value::Number(n) => {
            // index.js:327-329 — `number:<n.toString()>`. JS
            // `Number.prototype.toString()` for integers gives the
            // plain decimal repr; for floats it produces the
            // shortest round-trippable form. Pacquet's inputs only
            // ever hit the integer path in practice (the upstream
            // tests use `1`, `2`, `3`); `n.to_string()` from
            // `serde_json::Number` matches for that case.
            out.extend_from_slice(b"number:");
            out.extend_from_slice(n.to_string().as_bytes());
        }
        Value::String(s) => {
            // index.js:304-307. `string:<length>:<value>` — length
            // is UTF-16 code units (JS `.length`), not bytes and
            // not Unicode codepoints. For ASCII strings, all three
            // agree.
            let utf16_len: usize = s.encode_utf16().count();
            out.extend_from_slice(b"string:");
            out.extend_from_slice(utf16_len.to_string().as_bytes());
            out.push(b':');
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(arr) => {
            // index.js:257-291 — `array:<N>:` then each entry.
            // pnpm's `hashObject` *does* sort arrays in the
            // unordered case, but pacquet does not currently feed
            // arrays through this path, so the simpler "ordered"
            // variant is what we model. Adding the unordered
            // permutation handling can land alongside a real
            // caller that needs it.
            out.extend_from_slice(b"array:");
            out.extend_from_slice(arr.len().to_string().as_bytes());
            out.push(b':');
            for entry in arr {
                serialize(out, entry, sort);
            }
        }
        Value::Object(map) => {
            // index.js:225-255 — `object:<N>:<key>:<value>,...`.
            // Sorted iff `unorderedObjects` (i.e. `sort = true`).
            // Each key is dispatched through `_string` and each
            // value through the normal dispatcher.
            out.extend_from_slice(b"object:");
            out.extend_from_slice(map.len().to_string().as_bytes());
            out.push(b':');
            if sort {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    write_pair(out, key, &map[key], sort);
                }
            } else {
                for (key, val) in map {
                    write_pair(out, key, val, sort);
                }
            }
        }
    }
}

fn write_pair(out: &mut Vec<u8>, key: &str, value: &Value, sort: bool) {
    serialize(out, &Value::String(key.to_string()), sort);
    out.push(b':');
    serialize(out, value, sort);
    out.push(b',');
}
