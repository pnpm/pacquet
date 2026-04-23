//! Decoder for the narrow subset of [msgpackr](https://github.com/kriszyp/msgpackr)'s
//! wire format that pnpm v11 uses to write `index.db` rows — standard
//! MessagePack extended with msgpackr's **records** extension.
//!
//! ## Why this exists
//!
//! pnpm packs every `PackageFilesIndex` with `new Packr({ useRecords: true,
//! moreTypes: true })` (see
//! [`store/index/src/index.ts`](https://github.com/pnpm/pnpm/blob/main/store/index/src/index.ts)
//! line 12). `useRecords` replaces repeated string keys in same-shape
//! structs with a compact slot reference — roughly, Protobuf field numbers
//! inline. Standard `rmp_serde` has no idea what those bytes mean, so a
//! row pnpm wrote round-trips as "decode error → cache miss → re-download"
//! through pacquet's SQLite lookup. That defeats the whole point of a
//! shared store.
//!
//! ## Wire format (the parts pnpm actually emits)
//!
//! **Record definition** — a struct-shape declaration:
//! ```text
//! d4 72 <slot>    fixext1, ext type 0x72 ('r'), 1-byte payload = slot id
//! <array>         msgpack array of N field-name strings
//! <value 0>       raw msgpack value for field 0       ──┐
//! <value 1>       raw msgpack value for field 1         │ first instance,
//! …                                                     │ inlined
//! <value N-1>     raw msgpack value for field N-1     ──┘
//! ```
//! The slot byte is from `0x40..=0x7f`. (These bytes are where MessagePack
//! would normally encode positive fixints 64–127; inside a records stream
//! those values are instead hoisted into `uint 8`, so the range is free.)
//!
//! **Record reference** — every subsequent instance of a slot:
//! ```text
//! <slot>          single byte in 0x40..=0x7f
//! <value 0> … <value N-1>
//! ```
//!
//! Everything else (maps, arrays, strings, ints, bools, nil, floats) is
//! vanilla MessagePack. Despite `moreTypes: true`, pnpm's payloads encode
//! JS `Map` objects as standard msgpack `fixmap`/`map16`/`map32` — no
//! ext-type wrapping. `checkedAt` timestamps are written as `float 64`
//! because JS numbers are doubles.
//!
//! ## Strategy
//!
//! Rather than deserialize `PackageFilesIndex` directly from msgpackr
//! bytes, we **transcode** to vanilla MessagePack (expanding each record
//! instance into a string-keyed map) and hand the result to `rmp_serde`.
//! Reusing the existing `Deserialize` derive keeps the decoder focused on
//! the wire-format transformation and nothing else.

use derive_more::{Display, Error};
use miette::Diagnostic;
use std::collections::HashMap;

/// Extension type code msgpackr assigns to record-definition markers.
/// ASCII 'r'. See msgpackr's README under "Records Extension".
///
/// Exposed so callers can cheaply sniff whether a byte buffer was written
/// with `useRecords: true` — the fixext1 header `d4 72` is a reliable
/// opener for pnpm-written rows because the top-level struct is always
/// a record.
pub const RECORD_DEF_EXT_TYPE: u8 = 0x72;

/// Byte range that encodes a record-slot reference.
const SLOT_LO: u8 = 0x40;
const SLOT_HI: u8 = 0x7f;

/// Error type of [`transcode_to_plain_msgpack`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum DecodeError {
    #[display("Unexpected end of msgpackr buffer at offset {offset}")]
    #[diagnostic(code(pacquet_store_dir::msgpackr::unexpected_eof))]
    UnexpectedEof { offset: usize },

    #[display(
        "Reference to unknown record slot 0x{slot:02x} at offset {offset} — \
         the definition was missing or appeared later than its use"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr::unknown_slot))]
    UnknownSlot { slot: u8, offset: usize },

    #[display(
        "Record definition at offset {offset} has slot 0x{slot:02x}, which \
         is outside the valid reference range 0x40..=0x7f — any reference \
         written for this slot would be unreachable"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr::slot_out_of_range))]
    SlotOutOfRange { slot: u8, offset: usize },

    #[display(
        "Expected a msgpack array header (fixarray, array16, or array32) \
         for a record-definition field-name list at offset {offset}, got \
         byte 0x{byte:02x}"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr::expected_array_header))]
    ExpectedArrayHeader { byte: u8, offset: usize },

    #[display(
        "Expected a msgpack string header (fixstr, str8, str16, or str32) \
         for a record-definition field name at offset {offset}, got byte \
         0x{byte:02x}"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr::expected_string_header))]
    ExpectedStringHeader { byte: u8, offset: usize },

    #[display(
        "Field name in a record definition at offset {offset} contains \
         invalid UTF-8"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr::invalid_field_name_utf8))]
    InvalidFieldNameUtf8 { offset: usize },

    #[display("Unsupported msgpack header byte 0x{byte:02x} at offset {offset}")]
    #[diagnostic(code(pacquet_store_dir::msgpackr::unsupported))]
    Unsupported { byte: u8, offset: usize },

    #[display("{count} bytes left over after decoding the top-level value")]
    #[diagnostic(code(pacquet_store_dir::msgpackr::trailing_bytes))]
    TrailingBytes { count: usize },
}

/// Expand msgpackr records into a pure-MessagePack byte stream that
/// `rmp_serde` can deserialize.
///
/// `bytes` may already be pure msgpack (e.g. pacquet-written rows). The
/// bytes `0x40..=0x7f` are ambiguous — in vanilla MessagePack they're
/// positive fixints 64–127; inside a msgpackr-records stream they're
/// record-slot references. We disambiguate by tracking whether a record
/// definition has been seen in the stream so far: until the first
/// `d4 72 <slot>` header, those bytes are treated as fixints and the
/// transcoder behaves as a pass-through (modulo float-to-int narrowing,
/// which is always applied so the output can be deserialized into
/// integer-typed Rust fields).
pub fn transcode_to_plain_msgpack(bytes: &[u8]) -> Result<Vec<u8>, DecodeError> {
    let mut state = TranscodeState::default();
    let mut reader = Reader::new(bytes);
    let mut writer = Vec::with_capacity(bytes.len() + bytes.len() / 4);
    transcode_value(&mut reader, &mut writer, &mut state)?;
    let leftover = reader.remaining();
    if leftover != 0 {
        return Err(DecodeError::TrailingBytes { count: leftover });
    }
    Ok(writer)
}

/// Parser context threaded through `transcode_value`. Records mode
/// starts off and flips on the first record definition — msgpackr
/// doesn't re-emit positive fixints in the slot-byte range once records
/// mode is on, so the flip is one-way for any real stream.
#[derive(Default)]
struct TranscodeState {
    slots: HashMap<u8, Vec<String>>,
    records_mode: bool,
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Reader { bytes, pos: 0 }
    }
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }
    fn peek(&self, offset: usize) -> Result<u8, DecodeError> {
        self.bytes
            .get(self.pos + offset)
            .copied()
            .ok_or(DecodeError::UnexpectedEof { offset: self.pos + offset })
    }
    fn read_u8(&mut self) -> Result<u8, DecodeError> {
        let b = self.peek(0)?;
        self.pos += 1;
        Ok(b)
    }
    fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::UnexpectedEof { offset: self.pos })?;
        if end > self.bytes.len() {
            return Err(DecodeError::UnexpectedEof { offset: end });
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
    }
    fn read_u16(&mut self) -> Result<u16, DecodeError> {
        let b = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    fn read_u32(&mut self) -> Result<u32, DecodeError> {
        let b = self.read_bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Transcode one logical value (which may be a record instance — i.e. a
/// compound thing spanning a def + N raw values).
fn transcode_value(
    r: &mut Reader<'_>,
    w: &mut Vec<u8>,
    state: &mut TranscodeState,
) -> Result<(), DecodeError> {
    let start = r.pos;
    let head = r.peek(0)?;

    // Record reference — only valid after records mode has been entered;
    // in plain MessagePack the same bytes are positive fixints 64–127.
    if state.records_mode && (SLOT_LO..=SLOT_HI).contains(&head) {
        r.read_u8()?;
        let fields = state
            .slots
            .get(&head)
            .cloned()
            .ok_or(DecodeError::UnknownSlot { slot: head, offset: start })?;
        write_map_header(w, fields.len());
        for name in &fields {
            write_str(w, name);
            transcode_value(r, w, state)?;
        }
        return Ok(());
    }

    // Record definition — fixext1 with ext type 0x72. Followed by the field-name
    // array, then the first instance inlined. Seeing this header flips the
    // stream into records mode from here on.
    if head == 0xd4 && r.peek(1)? == RECORD_DEF_EXT_TYPE {
        r.read_u8()?; // 0xd4
        r.read_u8()?; // 0x72
        let slot_offset = r.pos;
        let slot = r.read_u8()?;
        // msgpackr only ever emits slot bytes in 0x40..=0x7f — any value
        // outside that range is either malformed input or a payload we
        // don't understand. Reject rather than silently registering a
        // slot that nothing could ever reference.
        if !(SLOT_LO..=SLOT_HI).contains(&slot) {
            return Err(DecodeError::SlotOutOfRange { slot, offset: slot_offset });
        }
        let fields = read_string_array(r)?;
        state.slots.insert(slot, fields.clone());
        state.records_mode = true;
        write_map_header(w, fields.len());
        for name in &fields {
            write_str(w, name);
            transcode_value(r, w, state)?;
        }
        return Ok(());
    }

    // Everything else: vanilla MessagePack. For scalars we just copy the
    // header + payload bytes across; for containers we emit the header and
    // recurse so any records inside still get expanded.
    match head {
        // Positive fixint 0x00..=0x7f. When records mode is active the
        // 0x40..=0x7f slice is trapped above; when it isn't, those bytes
        // are legitimate fixints and pass through.
        0x00..=0x7f => copy_n(r, w, 1),

        // Fixmap 0x80..=0x8f
        0x80..=0x8f => {
            let n = (head & 0x0f) as usize;
            r.read_u8()?;
            w.push(head);
            transcode_pairs(r, w, state, n)
        }
        // Fixarray 0x90..=0x9f
        0x90..=0x9f => {
            let n = (head & 0x0f) as usize;
            r.read_u8()?;
            w.push(head);
            transcode_array(r, w, state, n)
        }
        // Fixstr 0xa0..=0xbf
        0xa0..=0xbf => {
            let n = (head & 0x1f) as usize;
            copy_n(r, w, 1 + n)
        }
        // Negative fixint 0xe0..=0xff
        0xe0..=0xff => copy_n(r, w, 1),

        0xc0 /* nil */ | 0xc2 /* false */ | 0xc3 /* true */ => copy_n(r, w, 1),

        0xc4 /* bin 8  */ => {
            let n = r.peek(1)? as usize;
            copy_n(r, w, 2 + n)
        }
        0xc5 /* bin 16 */ => {
            let n = u16::from_be_bytes([r.peek(1)?, r.peek(2)?]) as usize;
            copy_n(r, w, 3 + n)
        }
        0xc6 /* bin 32 */ => {
            let n = u32::from_be_bytes([r.peek(1)?, r.peek(2)?, r.peek(3)?, r.peek(4)?]) as usize;
            copy_n(r, w, 5 + n)
        }

        // ext 8/16/32 — we've handled records above via fixext1; any other ext
        // just passes through. If a future pnpm release sends something fancier
        // we'll see it here.
        0xc7 => {
            let n = r.peek(1)? as usize;
            copy_n(r, w, 3 + n)
        }
        0xc8 => {
            let n = u16::from_be_bytes([r.peek(1)?, r.peek(2)?]) as usize;
            copy_n(r, w, 4 + n)
        }
        0xc9 => {
            let n = u32::from_be_bytes([r.peek(1)?, r.peek(2)?, r.peek(3)?, r.peek(4)?]) as usize;
            copy_n(r, w, 6 + n)
        }

        // msgpackr emits JS Number as float 64 whenever the value exceeds
        // int32 range — so timestamps like `checkedAt = 1_700_000_000_000`
        // arrive as `cb` + 8 bytes, even though they're semantically
        // integers. `rmp_serde` rejects floats for our integer-typed
        // fields (`size: u64`, `checked_at: Option<u64>`), so narrow
        // the representation back to uint 64 whenever the float is a
        // finite, non-negative integer value that fits. Non-integer or
        // out-of-range floats pass through unchanged so legitimate
        // floats (none appear in `PackageFilesIndex` today, but future
        // fields might) still round-trip.
        0xca /* float 32 */ => {
            r.read_u8()?;
            let bits = r.read_bytes(4)?;
            let v = f32::from_be_bytes([bits[0], bits[1], bits[2], bits[3]]);
            maybe_narrow_float_to_uint(w, v as f64, 0xca, &[bits[0], bits[1], bits[2], bits[3]]);
            Ok(())
        }
        0xcb /* float 64 */ => {
            r.read_u8()?;
            let bits = r.read_bytes(8)?;
            let arr = [bits[0], bits[1], bits[2], bits[3], bits[4], bits[5], bits[6], bits[7]];
            let v = f64::from_be_bytes(arr);
            maybe_narrow_float_to_uint(w, v, 0xcb, &arr);
            Ok(())
        }
        0xcc /* uint 8 */   => copy_n(r, w, 2),
        0xcd /* uint 16 */  => copy_n(r, w, 3),
        0xce /* uint 32 */  => copy_n(r, w, 5),
        0xcf /* uint 64 */  => copy_n(r, w, 9),
        0xd0 /* int 8 */    => copy_n(r, w, 2),
        0xd1 /* int 16 */   => copy_n(r, w, 3),
        0xd2 /* int 32 */   => copy_n(r, w, 5),
        0xd3 /* int 64 */   => copy_n(r, w, 9),

        // fixext 1/2/4/8/16 — 1 ext-type byte + 2^k payload bytes. 0xd4 + type
        // 0x72 is already handled above as records.
        0xd4 => copy_n(r, w, 1 + 1 + 1),
        0xd5 => copy_n(r, w, 1 + 1 + 2),
        0xd6 => copy_n(r, w, 1 + 1 + 4),
        0xd7 => copy_n(r, w, 1 + 1 + 8),
        0xd8 => copy_n(r, w, 1 + 1 + 16),

        0xd9 /* str 8  */ => {
            let n = r.peek(1)? as usize;
            copy_n(r, w, 2 + n)
        }
        0xda /* str 16 */ => {
            let n = u16::from_be_bytes([r.peek(1)?, r.peek(2)?]) as usize;
            copy_n(r, w, 3 + n)
        }
        0xdb /* str 32 */ => {
            let n = u32::from_be_bytes([r.peek(1)?, r.peek(2)?, r.peek(3)?, r.peek(4)?]) as usize;
            copy_n(r, w, 5 + n)
        }

        // array 16 / 32 — emit header, recurse N times.
        0xdc => {
            let n = u16::from_be_bytes([r.peek(1)?, r.peek(2)?]) as usize;
            w.extend_from_slice(r.read_bytes(3)?);
            transcode_array(r, w, state, n)
        }
        0xdd => {
            let n = u32::from_be_bytes([r.peek(1)?, r.peek(2)?, r.peek(3)?, r.peek(4)?]) as usize;
            w.extend_from_slice(r.read_bytes(5)?);
            transcode_array(r, w, state, n)
        }
        // map 16 / 32
        0xde => {
            let n = u16::from_be_bytes([r.peek(1)?, r.peek(2)?]) as usize;
            w.extend_from_slice(r.read_bytes(3)?);
            transcode_pairs(r, w, state, n)
        }
        0xdf => {
            let n = u32::from_be_bytes([r.peek(1)?, r.peek(2)?, r.peek(3)?, r.peek(4)?]) as usize;
            w.extend_from_slice(r.read_bytes(5)?);
            transcode_pairs(r, w, state, n)
        }

        // 0xc1 is reserved in the spec — reject rather than silently drop.
        other => Err(DecodeError::Unsupported { byte: other, offset: start }),
    }
}

fn transcode_array(
    r: &mut Reader<'_>,
    w: &mut Vec<u8>,
    state: &mut TranscodeState,
    n: usize,
) -> Result<(), DecodeError> {
    for _ in 0..n {
        transcode_value(r, w, state)?;
    }
    Ok(())
}

fn transcode_pairs(
    r: &mut Reader<'_>,
    w: &mut Vec<u8>,
    state: &mut TranscodeState,
    n: usize,
) -> Result<(), DecodeError> {
    for _ in 0..n {
        transcode_value(r, w, state)?; // key
        transcode_value(r, w, state)?; // value
    }
    Ok(())
}

fn copy_n(r: &mut Reader<'_>, w: &mut Vec<u8>, n: usize) -> Result<(), DecodeError> {
    let bytes = r.read_bytes(n)?;
    w.extend_from_slice(bytes);
    Ok(())
}

/// Read a msgpack array of strings at the current reader position and
/// return its elements. Only fixarray + array16/32 are accepted — record
/// defs in the wild are always fixarray, but array16/32 costs nothing to
/// support and future-proofs against a pnpm release that widens schemas
/// past 15 fields.
fn read_string_array(r: &mut Reader<'_>) -> Result<Vec<String>, DecodeError> {
    let start = r.pos;
    let head = r.read_u8()?;
    let len = match head {
        0x90..=0x9f => (head & 0x0f) as usize,
        0xdc => r.read_u16()? as usize,
        0xdd => r.read_u32()? as usize,
        _ => return Err(DecodeError::ExpectedArrayHeader { byte: head, offset: start }),
    };
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        out.push(read_string(r)?);
    }
    Ok(out)
}

fn read_string(r: &mut Reader<'_>) -> Result<String, DecodeError> {
    let start = r.pos;
    let head = r.read_u8()?;
    let len = match head {
        0xa0..=0xbf => (head & 0x1f) as usize,
        0xd9 => r.read_u8()? as usize,
        0xda => r.read_u16()? as usize,
        0xdb => r.read_u32()? as usize,
        _ => return Err(DecodeError::ExpectedStringHeader { byte: head, offset: start }),
    };
    let bytes = r.read_bytes(len)?.to_vec();
    String::from_utf8(bytes).map_err(|_| DecodeError::InvalidFieldNameUtf8 { offset: start })
}

/// Exactly 2^64 as f64 — the smallest `f64` value that does **not** fit
/// in a `u64`. `u64::MAX as f64` rounds *up* to 2^64 (u64::MAX is
/// 2^64 − 1, which is not exactly representable in f64), so using it as
/// the inclusive upper bound would admit a literal 2^64 and silently
/// saturate to `u64::MAX` on cast.
const U64_MAX_EXCLUSIVE_AS_F64: f64 = 18_446_744_073_709_551_616.0;

/// If `v` is a finite non-negative integer value that strictly fits in
/// `u64`, emit it as msgpack `uint 64` (`cf` + 8 big-endian bytes).
/// Otherwise, pass through the original float header + payload
/// unchanged. The strict upper bound (`< 2^64`, not `<= u64::MAX as f64`)
/// prevents silent value corruption at the representable-but-overflowing
/// edge.
fn maybe_narrow_float_to_uint(w: &mut Vec<u8>, v: f64, original_head: u8, original_bytes: &[u8]) {
    if v.is_finite() && (0.0..U64_MAX_EXCLUSIVE_AS_F64).contains(&v) && v.fract() == 0.0 {
        w.push(0xcf);
        w.extend_from_slice(&(v as u64).to_be_bytes());
    } else {
        w.push(original_head);
        w.extend_from_slice(original_bytes);
    }
}

fn write_map_header(w: &mut Vec<u8>, n: usize) {
    if n < 16 {
        w.push(0x80 | (n as u8));
    } else if n <= u16::MAX as usize {
        w.push(0xde);
        w.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        w.push(0xdf);
        w.extend_from_slice(&(n as u32).to_be_bytes());
    }
}

fn write_str(w: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    let n = bytes.len();
    if n < 32 {
        w.push(0xa0 | (n as u8));
    } else if n <= u8::MAX as usize {
        w.push(0xd9);
        w.push(n as u8);
    } else if n <= u16::MAX as usize {
        w.push(0xda);
        w.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        w.push(0xdb);
        w.extend_from_slice(&(n as u32).to_be_bytes());
    }
    w.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CafsFileInfo, PackageFilesIndex};
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    /// Decoding fixture bytes produced by msgpackr yields the same
    /// `PackageFilesIndex` we'd get from a vanilla msgpack round-trip.
    fn decode(bytes: &[u8]) -> PackageFilesIndex {
        let plain = transcode_to_plain_msgpack(bytes).expect("transcode succeeds");
        rmp_serde::from_slice::<PackageFilesIndex>(&plain)
            .expect("transcoded output deserializes as PackageFilesIndex")
    }

    /// Fixture: `node /tmp/msgpackr_fixture.mjs`, "one-file index" case.
    /// Source object:
    /// ```js
    /// { algo: 'sha512', files: new Map([['package.json',
    ///   { digest: 'abc', mode: 0o644, size: 17, checkedAt: 1700000000000 }]]) }
    /// ```
    #[test]
    fn decodes_one_file_fixture_from_msgpackr() {
        let bytes: [u8; 84] = [
            0xd4, 0x72, 0x40, 0x92, 0xa4, 0x61, 0x6c, 0x67, 0x6f, 0xa5, 0x66, 0x69, 0x6c, 0x65,
            0x73, 0xa6, 0x73, 0x68, 0x61, 0x35, 0x31, 0x32, 0x81, 0xac, 0x70, 0x61, 0x63, 0x6b,
            0x61, 0x67, 0x65, 0x2e, 0x6a, 0x73, 0x6f, 0x6e, 0xd4, 0x72, 0x41, 0x94, 0xa6, 0x64,
            0x69, 0x67, 0x65, 0x73, 0x74, 0xa4, 0x6d, 0x6f, 0x64, 0x65, 0xa4, 0x73, 0x69, 0x7a,
            0x65, 0xa9, 0x63, 0x68, 0x65, 0x63, 0x6b, 0x65, 0x64, 0x41, 0x74, 0xa3, 0x61, 0x62,
            0x63, 0xcd, 0x01, 0xa4, 0x11, 0xcb, 0x42, 0x78, 0xbc, 0xfe, 0x56, 0x80, 0x00, 0x00,
        ];
        let decoded = decode(&bytes);

        let mut expected_files = HashMap::new();
        expected_files.insert(
            "package.json".to_string(),
            CafsFileInfo {
                digest: "abc".to_string(),
                mode: 0o644,
                size: 17,
                checked_at: Some(1_700_000_000_000),
            },
        );
        assert_eq!(decoded.algo, "sha512");
        assert_eq!(decoded.files, expected_files);
        assert_eq!(decoded.manifest, None);
        assert_eq!(decoded.requires_build, None);
    }

    /// Fixture: "two-file index" — exercises record **reuse** (the second
    /// `CafsFileInfo` starts with a bare slot byte 0x41).
    #[test]
    fn decodes_two_file_fixture_with_record_reuse() {
        let bytes: [u8; 103] = [
            0xd4, 0x72, 0x40, 0x92, 0xa4, 0x61, 0x6c, 0x67, 0x6f, 0xa5, 0x66, 0x69, 0x6c, 0x65,
            0x73, 0xa6, 0x73, 0x68, 0x61, 0x35, 0x31, 0x32, 0x82, 0xac, 0x70, 0x61, 0x63, 0x6b,
            0x61, 0x67, 0x65, 0x2e, 0x6a, 0x73, 0x6f, 0x6e, 0xd4, 0x72, 0x41, 0x94, 0xa6, 0x64,
            0x69, 0x67, 0x65, 0x73, 0x74, 0xa4, 0x6d, 0x6f, 0x64, 0x65, 0xa4, 0x73, 0x69, 0x7a,
            0x65, 0xa9, 0x63, 0x68, 0x65, 0x63, 0x6b, 0x65, 0x64, 0x41, 0x74, 0xa3, 0x61, 0x62,
            0x63, 0xcd, 0x01, 0xa4, 0x11, 0xcb, 0x42, 0x78, 0xbc, 0xfe, 0x56, 0x80, 0x00, 0x00,
            0xa8, 0x69, 0x6e, 0x64, 0x65, 0x78, 0x2e, 0x6a, 0x73, 0x41, 0xa3, 0x64, 0x65, 0x66,
            0xcd, 0x01, 0xed, 0x2a, 0xc0,
        ];
        let decoded = decode(&bytes);
        assert_eq!(decoded.files.len(), 2);

        let pkg_json = decoded.files.get("package.json").unwrap();
        assert_eq!(pkg_json.digest, "abc");
        assert_eq!(pkg_json.mode, 0o644);
        assert_eq!(pkg_json.size, 17);
        assert_eq!(pkg_json.checked_at, Some(1_700_000_000_000));

        let index_js = decoded.files.get("index.js").unwrap();
        assert_eq!(index_js.digest, "def");
        assert_eq!(index_js.mode, 0o755);
        assert_eq!(index_js.size, 42);
        assert_eq!(index_js.checked_at, None);
    }

    /// Fixture: "with requiresBuild" — boolean top-level field.
    #[test]
    fn decodes_requires_build_true() {
        let bytes: [u8; 83] = [
            0xd4, 0x72, 0x40, 0x93, 0xa4, 0x61, 0x6c, 0x67, 0x6f, 0xad, 0x72, 0x65, 0x71, 0x75,
            0x69, 0x72, 0x65, 0x73, 0x42, 0x75, 0x69, 0x6c, 0x64, 0xa5, 0x66, 0x69, 0x6c, 0x65,
            0x73, 0xa6, 0x73, 0x68, 0x61, 0x35, 0x31, 0x32, 0xc3, 0x81, 0xa4, 0x61, 0x2e, 0x6a,
            0x73, 0xd4, 0x72, 0x41, 0x94, 0xa6, 0x64, 0x69, 0x67, 0x65, 0x73, 0x74, 0xa4, 0x6d,
            0x6f, 0x64, 0x65, 0xa4, 0x73, 0x69, 0x7a, 0x65, 0xa9, 0x63, 0x68, 0x65, 0x63, 0x6b,
            0x65, 0x64, 0x41, 0x74, 0xa3, 0x61, 0x61, 0x61, 0xcd, 0x01, 0xa4, 0x01, 0x0a,
        ];
        let decoded = decode(&bytes);
        assert_eq!(decoded.requires_build, Some(true));
    }

    /// Fixture: "no checkedAt" — proves msgpackr emits a *different* record
    /// shape (3 fields instead of 4) when an optional field is absent, and
    /// our `Option<u64>` deserializer copes.
    #[test]
    fn decodes_file_without_checked_at() {
        let bytes: [u8; 57] = [
            0xd4, 0x72, 0x40, 0x92, 0xa4, 0x61, 0x6c, 0x67, 0x6f, 0xa5, 0x66, 0x69, 0x6c, 0x65,
            0x73, 0xa6, 0x73, 0x68, 0x61, 0x35, 0x31, 0x32, 0x81, 0xa4, 0x61, 0x2e, 0x6a, 0x73,
            0xd4, 0x72, 0x41, 0x93, 0xa6, 0x64, 0x69, 0x67, 0x65, 0x73, 0x74, 0xa4, 0x6d, 0x6f,
            0x64, 0x65, 0xa4, 0x73, 0x69, 0x7a, 0x65, 0xa3, 0x61, 0x61, 0x61, 0xcd, 0x01, 0xa4,
            0x01,
        ];
        let decoded = decode(&bytes);
        let info = decoded.files.get("a.js").unwrap();
        assert_eq!(info.checked_at, None);
    }

    /// Fixture: "with sideEffects" — nested map inside a record field,
    /// plus a second record slot for the inner struct.
    #[test]
    fn decodes_side_effects() {
        let bytes: [u8; 113] = [
            0xd4, 0x72, 0x40, 0x93, 0xa4, 0x61, 0x6c, 0x67, 0x6f, 0xa5, 0x66, 0x69, 0x6c, 0x65,
            0x73, 0xab, 0x73, 0x69, 0x64, 0x65, 0x45, 0x66, 0x66, 0x65, 0x63, 0x74, 0x73, 0xa6,
            0x73, 0x68, 0x61, 0x35, 0x31, 0x32, 0x81, 0xa4, 0x61, 0x2e, 0x6a, 0x73, 0xd4, 0x72,
            0x41, 0x94, 0xa6, 0x64, 0x69, 0x67, 0x65, 0x73, 0x74, 0xa4, 0x6d, 0x6f, 0x64, 0x65,
            0xa4, 0x73, 0x69, 0x7a, 0x65, 0xa9, 0x63, 0x68, 0x65, 0x63, 0x6b, 0x65, 0x64, 0x41,
            0x74, 0xa3, 0x61, 0x61, 0x61, 0xcd, 0x01, 0xa4, 0x01, 0x0a, 0x81, 0xa5, 0x6c, 0x69,
            0x6e, 0x75, 0x78, 0xd4, 0x72, 0x42, 0x91, 0xa5, 0x61, 0x64, 0x64, 0x65, 0x64, 0x81,
            0xa4, 0x62, 0x2e, 0x73, 0x6f, 0x41, 0xa3, 0x62, 0x62, 0x62, 0xcd, 0x01, 0xa4, 0x02,
            0x14,
        ];
        let decoded = decode(&bytes);
        let side = decoded.side_effects.expect("side_effects present");
        let linux = side.get("linux").expect("linux entry");
        let added = linux.added.as_ref().expect("added map");
        let b_so = added.get("b.so").expect("b.so entry");
        assert_eq!(b_so.digest, "bbb");
        assert_eq!(b_so.mode, 0o644);
        assert_eq!(b_so.size, 2);
        assert_eq!(b_so.checked_at, Some(20));
    }

    /// A row pacquet wrote itself — vanilla msgpack via `rmp_serde::to_vec_named`
    /// — must decode to the same struct after passing through the
    /// transcoder. The bytes are *not* guaranteed to be byte-for-byte
    /// identical post-transcode: `CafsFileInfo::checked_at` is written
    /// as `float 64` for msgpackr/pnpm interop, and the transcoder's
    /// integer-valued-float narrowing rewrites it back to `uint 64`.
    /// What matters is that the decoded `PackageFilesIndex` round-trips.
    #[test]
    fn round_trips_plain_msgpack_through_transcoder() {
        let mut files = HashMap::new();
        files.insert(
            "README.md".to_string(),
            CafsFileInfo { digest: "x".repeat(128), mode: 0o644, size: 42, checked_at: Some(1) },
        );
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: Some(false),
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let bytes = rmp_serde::to_vec_named(&original).unwrap();
        let transcoded = transcode_to_plain_msgpack(&bytes).unwrap();
        let decoded: PackageFilesIndex = rmp_serde::from_slice(&transcoded).unwrap();
        assert_eq!(decoded, original);
    }

    /// Plain msgpack bytes that contain no `float`-encoded integers should
    /// still pass through the transcoder byte-for-byte — the narrowing
    /// rule must not touch anything that isn't a float header.
    #[test]
    fn plain_msgpack_without_floats_passes_through_unchanged() {
        // { "size": 17, "mode": 420 } — purely integer values, no
        // checked_at, so the encoded bytes have no float headers.
        let bytes = rmp_serde::to_vec_named(&serde_json::json!({
            "size": 17,
            "mode": 420,
        }))
        .unwrap();
        let transcoded = transcode_to_plain_msgpack(&bytes).unwrap();
        assert_eq!(transcoded, bytes);
    }

    /// A genuine non-integer float (π) must survive the transcoder as a
    /// float. We don't have `PackageFilesIndex` fields that carry such
    /// a value today, but the transcoder itself is a general utility —
    /// the narrowing should only fire for integer-valued floats.
    #[test]
    fn non_integer_floats_pass_through() {
        // [3.14] as fixarray(1) + float64
        let mut input = vec![0x91, 0xcb];
        input.extend_from_slice(&std::f64::consts::PI.to_be_bytes());

        let out = transcode_to_plain_msgpack(&input).unwrap();
        assert_eq!(out, input, "π must stay as float 64, not be narrowed");
    }

    /// A `float 64` whose value is exactly `2^64` must NOT narrow —
    /// `u64::MAX as f64` rounds up to 2^64, so a naive
    /// `v <= u64::MAX as f64` bound would admit the value and silently
    /// cast it to `u64::MAX`. Must pass through unchanged instead.
    #[test]
    fn float64_equal_to_2_pow_64_passes_through() {
        let mut input = vec![0x91, 0xcb];
        input.extend_from_slice(&18_446_744_073_709_551_616.0_f64.to_be_bytes());
        let out = transcode_to_plain_msgpack(&input).unwrap();
        assert_eq!(out, input, "2^64 must not be narrowed to u64::MAX");
    }

    /// An integer-valued float 32 must be narrowed too. Pnpm doesn't
    /// emit `float 32`, but a hand-crafted payload could, and the rule
    /// should be consistent.
    #[test]
    fn integer_valued_float32_is_narrowed_to_uint64() {
        // [42.0] as fixarray(1) + float32
        let mut input = vec![0x91, 0xca];
        input.extend_from_slice(&42.0_f32.to_be_bytes());

        let out = transcode_to_plain_msgpack(&input).unwrap();
        // Expect fixarray(1) + uint 64 (cf) + 42 as 8 big-endian bytes.
        let mut expected = vec![0x91, 0xcf];
        expected.extend_from_slice(&42u64.to_be_bytes());
        assert_eq!(out, expected);
    }

    #[test]
    fn rejects_reference_to_unknown_slot() {
        // fixarray(2):
        //   [0] def slot 0x40 (fields ["x"]) + inline first instance (nil)
        //   [1] bare reference to slot 0x41 — never defined
        let bytes: &[u8] = &[
            0x92, // fixarray(2)
            0xd4, 0x72, 0x40, // def slot 0x40
            0x91, 0xa1, b'x', // fields: ["x"]
            0xc0, // first instance: nil
            0x41, // ref to slot 0x41 — undefined
        ];
        let err = transcode_to_plain_msgpack(bytes).unwrap_err();
        assert!(matches!(err, DecodeError::UnknownSlot { slot: 0x41, .. }), "got {err:?}");
    }

    /// In plain MessagePack, a bare 0x40..=0x7f byte is a positive
    /// fixint (64..=127) — not a record slot reference. The transcoder
    /// must not touch it until a record definition has actually
    /// appeared in the stream.
    #[test]
    fn plain_positive_fixint_in_slot_range_passes_through() {
        // [65, 127] — both bytes would be "slot refs" under the old
        // always-records interpretation and would blow up as
        // `UnknownSlot`. Under records-mode tracking they're legitimate
        // positive fixints.
        let input = &[0x92, 0x41, 0x7f][..];
        let out = transcode_to_plain_msgpack(input).unwrap();
        assert_eq!(out, input);
    }

    #[test]
    fn rejects_truncated_buffer() {
        // Record def claims 2 field names but only one is present.
        let err = transcode_to_plain_msgpack(&[0xd4, 0x72, 0x40, 0x92, 0xa1, b'k']).unwrap_err();
        assert!(matches!(err, DecodeError::UnexpectedEof { .. }), "got {err:?}");
    }
}
