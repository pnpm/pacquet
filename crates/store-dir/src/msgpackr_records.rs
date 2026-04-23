//! Encoder and decoder for the narrow subset of
//! [msgpackr](https://github.com/kriszyp/msgpackr)'s wire format that
//! pnpm v11 uses to write `index.db` rows — standard MessagePack
//! extended with msgpackr's **records** extension.
//!
//! ## Why this exists
//!
//! pnpm packs every `PackageFilesIndex` with `new Packr({ useRecords: true,
//! moreTypes: true })` (see
//! [`store/index/src/index.ts`](https://github.com/pnpm/pnpm/blob/main/store/index/src/index.ts)
//! line 12). `useRecords` replaces repeated string keys in same-shape
//! structs with a compact slot reference — roughly, Protobuf field numbers
//! inline. Plain `rmp_serde` output round-trips through msgpackr badly
//! in *both* directions:
//!
//! - **Reading pnpm → pacquet**: standard `rmp_serde` has no idea what
//!   records bytes mean, so a row pnpm wrote would fail to decode and
//!   look like a cache miss, forcing a full re-download.
//! - **Reading pacquet → pnpm**: msgpackr with `useRecords: true`
//!   decodes every plain msgpack map (at any nesting level) as a JS
//!   `Map`, including the top-level `PackageFilesIndex`. pnpm's code
//!   then does `pkgIndex.files` (a property access on that `Map`),
//!   gets `undefined`, and crashes with `files is not iterable`.
//!
//! This module provides both halves — [`transcode_to_plain_msgpack`]
//! for the read side and [`encode_package_files_index`] for the write
//! side — so a shared `index.db` actually works.
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
//! **Read side** ([`transcode_to_plain_msgpack`]): rather than
//! deserialize `PackageFilesIndex` directly from msgpackr bytes, we
//! transcode to vanilla MessagePack (expanding each record instance
//! into a string-keyed map) and hand the result to `rmp_serde`.
//! Reusing the existing `Deserialize` derive keeps the decoder focused
//! on the wire-format transformation and nothing else.
//!
//! **Write side** ([`encode_package_files_index`]): a hand-written
//! emitter that allocates slots lazily per distinct *record shape*
//! for `PackageFilesIndex`, `CafsFileInfo`, and `SideEffectsDiff` —
//! `0x40` is reserved for the top-level `PackageFilesIndex`, and
//! inner slots in `0x41..=0x7f` are handed out in first-seen order,
//! so a single Rust type can consume more than one slot when its
//! optional-field presence varies within the same row. `HashMap`
//! fields (`files`, `sideEffects`, `added`) stay as plain msgpack
//! maps. That shape matches what msgpackr itself emits for a JS
//! object containing `Map` fields, so pnpm's reader round-trips the
//! bytes correctly.

use crate::{CafsFileInfo, PackageFilesIndex, SideEffectsDiff};
use derive_more::{Display, Error};
use miette::Diagnostic;
use std::{collections::HashMap, rc::Rc};

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
    #[display("Unexpected end of MessagePack buffer at offset {offset}")]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::unexpected_eof))]
    UnexpectedEof { offset: usize },

    #[display(
        "Reference to unknown record slot 0x{slot:02x} at offset {offset} — \
         the definition was missing or appeared later than its use"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::unknown_slot))]
    UnknownSlot { slot: u8, offset: usize },

    #[display(
        "Record definition at offset {offset} has slot 0x{slot:02x}, which \
         is outside the valid reference range 0x40..=0x7f — any reference \
         written for this slot would be unreachable"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::slot_out_of_range))]
    SlotOutOfRange { slot: u8, offset: usize },

    #[display(
        "Expected a msgpack array header (fixarray, array16, or array32) \
         for a record-definition field-name list at offset {offset}, got \
         byte 0x{byte:02x}"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::expected_array_header))]
    ExpectedArrayHeader { byte: u8, offset: usize },

    #[display(
        "Expected a msgpack string header (fixstr, str8, str16, or str32) \
         for a record-definition field name at offset {offset}, got byte \
         0x{byte:02x}"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::expected_string_header))]
    ExpectedStringHeader { byte: u8, offset: usize },

    #[display(
        "Field name in a record definition at offset {offset} contains \
         invalid UTF-8"
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::invalid_field_name_utf8))]
    InvalidFieldNameUtf8 { offset: usize },

    #[display("Unsupported msgpack header byte 0x{byte:02x} at offset {offset}")]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::unsupported))]
    Unsupported { byte: u8, offset: usize },

    #[display("{count} bytes left over after decoding the top-level value")]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::trailing_bytes))]
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
///
/// Slot schemas live under `Rc<[String]>` so reference-path decoding
/// can bump a refcount instead of deep-cloning the field-name vector
/// on every record instance. A row with 200 files used to allocate
/// 200 `Vec<String>`s plus one `String` per field name per clone; now
/// it allocates once at definition time.
#[derive(Default)]
struct TranscodeState {
    slots: HashMap<u8, Rc<[String]>>,
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
        // `Rc::clone` is a refcount bump — the `Vec<String>` of field
        // names isn't duplicated. We clone instead of borrowing so the
        // recursive `transcode_value` call below can take `&mut state`.
        let fields = Rc::clone(
            state.slots.get(&head).ok_or(DecodeError::UnknownSlot { slot: head, offset: start })?,
        );
        write_map_header(w, fields.len());
        for name in fields.iter() {
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
        let fields: Rc<[String]> = read_string_array(r)?.into();
        state.slots.insert(slot, Rc::clone(&fields));
        state.records_mode = true;
        write_map_header(w, fields.len());
        for name in fields.iter() {
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

/// Encode a [`PackageFilesIndex`] to msgpackr-records bytes that match
/// pnpm v11's wire format closely enough that `Packr({useRecords: true,
/// moreTypes: true}).unpack(bytes)` decodes to the same JS shape pnpm
/// produces itself.
///
/// ## Why not `rmp_serde::to_vec_named`?
///
/// `rmp_serde` emits plain MessagePack — every struct becomes a `fixmap`
/// / `map16` / `map32`. That's a perfectly valid MessagePack encoding,
/// but msgpackr with `useRecords: true` interprets *every* msgpack map
/// (no matter the nesting depth) as a JS `Map` object, including the
/// top-level `PackageFilesIndex`. pnpm's reader then does
/// `pkgIndex.files` (a property access) on what is actually a `Map`,
/// gets `undefined`, and crashes with `files is not iterable`.
///
/// pnpm itself sidesteps this because it packs the outer struct with
/// `useRecords: true`, which makes msgpackr emit a **record**: the
/// `d4 72 <slot>` fixext1 header followed by a field-name array and the
/// values. Records decode back as plain JS objects, while legitimate JS
/// `Map` values (pnpm's `files` / `sideEffects` / `added`) are still
/// encoded as msgpack maps and decode back as `Map`. The decoder can
/// tell the two apart because records are marked with the fixext1
/// envelope; plain maps aren't.
///
/// So to interop with pnpm, pacquet has to emit records for the Rust
/// `struct`s (object-shape on the pnpm side) and keep plain msgpack
/// maps for the Rust `HashMap`s (`Map`-shape on the pnpm side). That's
/// what this encoder does.
///
/// ## Slot allocation
///
/// Slot `0x40` is reserved for the top-level [`PackageFilesIndex`] —
/// one per row, always first in the stream. Inner slots in
/// `0x41..=0x7f` are allocated **lazily, in first-seen order, one per
/// distinct record shape** (where "shape" is the set of fields that
/// instance actually carries). A single Rust type may therefore span
/// multiple slots if different optional-field combinations show up in
/// the same row: a `CafsFileInfo` carrying `checkedAt` lands in one
/// slot and a `CafsFileInfo` without it lands in another. Same-shape
/// instances downstream collapse to a single bare-slot byte, which is
/// the record-compression win records exist for.
///
/// This is what msgpackr itself does for the same traversal and shape
/// set, so pacquet's output is **wire-compatible** with msgpackr (same
/// record schemas, same slot numbers, same value encodings) — pnpm's
/// reader reconstructs the same JS shape from both. Exact bytes can
/// still differ when Rust's `HashMap` iterates `files` / `sideEffects`
/// / `added` entries in a different order than msgpackr's JS `Map`
/// iteration, which is fine for correctness but worth keeping in mind
/// when diffing bytes against a pnpm-written reference row.
///
/// ## Optional-field handling
///
/// - **`PackageFilesIndex`**: `algo` and `files` are always emitted;
///   `requires_build` and `side_effects` are included in the record
///   schema only when `Some`. `manifest` is always `None` in pacquet
///   today and not yet wired through; the encoder returns
///   [`EncodeError::ManifestNotSupported`] if it ever gets a `Some`,
///   which is louder than silently dropping it.
/// - **`CafsFileInfo`**: optional `checkedAt` is omitted from the
///   record schema entirely when `None` rather than written as `nil`,
///   so the presence of `checkedAt` determines the shape and thus
///   the slot. When `Some`, it's written as `float 64` (see
///   [`CafsFileInfo::checked_at`] for why — msgpackr reads `uint 64`
///   as `BigInt`, which crashes pnpm's `mtimeMs - (checkedAt ?? 0)`).
/// - **`SideEffectsDiff`**: `added` and `deleted` are both optional;
///   each is included in the schema only when `Some`. The four
///   possible shapes (`{added}`, `{deleted}`, `{added, deleted}`,
///   `{}`) each get their own slot on first use.
///
/// Matching msgpackr's omit-when-absent convention (rather than
/// padding with `nil`) means pnpm's reader sees the same JS object
/// shape regardless of which tool wrote the row — a `SideEffectsDiff
/// { added: Some, deleted: None }` decodes to `{ added: Map }`, not
/// `{ added: Map, deleted: null }`.
pub fn encode_package_files_index(index: &PackageFilesIndex) -> Result<Vec<u8>, EncodeError> {
    let mut state = EncodeState::new();
    let mut out = Vec::with_capacity(256);
    encode_pkg_files_index_value(&mut out, &mut state, index)?;
    Ok(out)
}

/// Error type of [`encode_package_files_index`].
#[derive(Debug, Display, Error, Diagnostic)]
#[non_exhaustive]
pub enum EncodeError {
    #[display(
        "PackageFilesIndex.manifest is Some, but the msgpackr-records \
         encoder doesn't yet know how to serialize `serde_json::Value` \
         — pacquet doesn't populate this field today, so the code path \
         is unimplemented. Add manifest encoding if/when pacquet starts \
         writing bundled manifests."
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::manifest_not_supported))]
    ManifestNotSupported,

    #[display(
        "Ran out of msgpackr record slots: encountered more than \
         {max} distinct record shapes (slot range is 0x41..=0x7f). \
         This shouldn't happen for current pacquet payloads — \
         `CafsFileInfo` has at most 2 shapes and `SideEffectsDiff` \
         at most 4. Reaching this error likely means a new record \
         type was added to the encoder without bumping the shape \
         accounting, or the encoder is being reused for a schema \
         it wasn't designed for."
    )]
    #[diagnostic(code(pacquet_store_dir::msgpackr_records::out_of_record_slots))]
    OutOfRecordSlots { max: usize },
}

/// Slot allocated to the top-level [`PackageFilesIndex`] record.
/// A single stream always has exactly one of these, so it gets the
/// base slot. Inner records (`CafsFileInfo`, `SideEffectsDiff`) are
/// allocated lazily from `FIRST_INNER_SLOT` upwards, one slot per
/// distinct shape — see [`EncodeState::allocate_slot`].
const PKG_FILES_INDEX_SLOT: u8 = SLOT_LO; // 0x40
const FIRST_INNER_SLOT: u8 = SLOT_LO + 1; // 0x41

/// Tracks which shapes have been defined and what slot each got.
/// Mirrors msgpackr's own strategy: when it sees a new record instance
/// whose field set differs from anything previously packed, it
/// allocates a new slot rather than redefining an existing one — so
/// same-shape instances downstream collapse to a single bare-slot byte
/// (the point of records), and mixed-shape streams still decode
/// correctly without per-instance re-defs.
///
/// Shape keys are small bitmasks over the optional fields of each
/// record type, see `cafs_shape` / `side_effects_shape`. Each type has
/// at most a handful of possible shapes (2 for `CafsFileInfo`, 4 for
/// `SideEffectsDiff`), so the 0x40..=0x7f slot range is vastly
/// over-provisioned for realistic workloads.
struct EncodeState {
    /// Shape → slot for every `CafsFileInfo` shape seen so far. The
    /// index is the shape bitmask produced by [`cafs_shape`] (2
    /// possible values today). `None` = shape hasn't been emitted yet
    /// in this stream.
    cafs_slots: [Option<u8>; 2],
    /// Same for `SideEffectsDiff`, indexed by [`side_effects_shape`]
    /// (4 possible values).
    side_effects_slots: [Option<u8>; 4],
    /// Next unused slot in the 0x41..=0x7f range. Starts above
    /// `PKG_FILES_INDEX_SLOT` because the top-level record always
    /// takes slot 0x40.
    next_slot: u8,
}

impl EncodeState {
    fn new() -> Self {
        EncodeState {
            cafs_slots: [None; 2],
            side_effects_slots: [None; 4],
            next_slot: FIRST_INNER_SLOT,
        }
    }

    fn allocate_slot(&mut self) -> Result<u8, EncodeError> {
        if self.next_slot > SLOT_HI {
            return Err(EncodeError::OutOfRecordSlots {
                max: (SLOT_HI - FIRST_INNER_SLOT + 1) as usize,
            });
        }
        let slot = self.next_slot;
        self.next_slot += 1;
        Ok(slot)
    }
}

/// Bitmask describing which optional fields a [`CafsFileInfo`] carries.
/// Bit 0 = `checked_at`. Required fields (digest, mode, size) don't
/// affect the shape because they're always present.
fn cafs_shape(info: &CafsFileInfo) -> u8 {
    u8::from(info.checked_at.is_some())
}

/// Bitmask describing which optional fields a [`SideEffectsDiff`]
/// carries. Bit 0 = `added`, bit 1 = `deleted`.
fn side_effects_shape(diff: &SideEffectsDiff) -> u8 {
    u8::from(diff.added.is_some()) | (u8::from(diff.deleted.is_some()) << 1)
}

fn encode_pkg_files_index_value(
    w: &mut Vec<u8>,
    state: &mut EncodeState,
    idx: &PackageFilesIndex,
) -> Result<(), EncodeError> {
    if idx.manifest.is_some() {
        return Err(EncodeError::ManifestNotSupported);
    }

    // Build the record schema from the fields we're going to emit.
    // `algo` and `files` are always present. The two optional fields are
    // included only when `Some`, matching msgpackr's own behaviour when
    // packing a plain JS object with missing properties.
    let mut fields: Vec<&str> = Vec::with_capacity(4);
    fields.push("algo");
    fields.push("files");
    if idx.requires_build.is_some() {
        fields.push("requiresBuild");
    }
    if idx.side_effects.is_some() {
        fields.push("sideEffects");
    }

    write_record_def_header(w, PKG_FILES_INDEX_SLOT, &fields);

    // Values in the same order as `fields` above.
    write_str(w, &idx.algo);
    write_map_header(w, idx.files.len());
    for (name, info) in &idx.files {
        write_str(w, name);
        encode_cafs_file_info(w, state, info)?;
    }
    if let Some(rb) = idx.requires_build {
        write_bool(w, rb);
    }
    if let Some(se) = &idx.side_effects {
        write_map_header(w, se.len());
        for (platform, diff) in se {
            write_str(w, platform);
            encode_side_effects_diff(w, state, diff)?;
        }
    }

    Ok(())
}

fn encode_cafs_file_info(
    w: &mut Vec<u8>,
    state: &mut EncodeState,
    info: &CafsFileInfo,
) -> Result<(), EncodeError> {
    let shape = cafs_shape(info);
    if let Some(slot) = state.cafs_slots[shape as usize] {
        w.push(slot); // bare slot = record reference; no def needed
    } else {
        // New shape for this stream — allocate a slot and emit a
        // record def inline. `digest`, `mode`, `size` are required;
        // `checkedAt` is included only when `Some`, matching msgpackr's
        // field-omit-when-absent behaviour so pnpm's reader sees the
        // same object shape on round-trip. Field order matches pnpm's
        // own output.
        let slot = state.allocate_slot()?;
        state.cafs_slots[shape as usize] = Some(slot);
        let fields: &[&str] = if info.checked_at.is_some() {
            &["digest", "mode", "size", "checkedAt"]
        } else {
            &["digest", "mode", "size"]
        };
        write_record_def_header(w, slot, fields);
    }

    write_str(w, &info.digest);
    write_uint(w, info.mode as u64);
    write_uint(w, info.size);
    if let Some(v) = info.checked_at {
        // Float 64 — not uint 64 — because msgpackr decodes `uint 64`
        // as a JS `BigInt`, and pnpm's integrity check does
        // `mtimeMs - (checkedAt ?? 0)` which throws `TypeError: Cannot
        // mix BigInt and other types`. Packing as a double matches
        // what pnpm writes for the same millisecond-epoch value (JS
        // Number is a double, so msgpackr emits `cb` + 8 bytes for
        // values past int32 range).
        write_float64(w, v as f64);
    }
    Ok(())
}

fn encode_side_effects_diff(
    w: &mut Vec<u8>,
    state: &mut EncodeState,
    diff: &SideEffectsDiff,
) -> Result<(), EncodeError> {
    let shape = side_effects_shape(diff);
    if let Some(slot) = state.side_effects_slots[shape as usize] {
        w.push(slot);
    } else {
        // Msgpackr omits absent `added` / `deleted` from the schema
        // rather than writing them as explicit `null`. Match that so
        // downstream JS code checking `diff.added != null` /
        // `diff.deleted != null` sees the same shape regardless of
        // which tool wrote the row.
        let slot = state.allocate_slot()?;
        state.side_effects_slots[shape as usize] = Some(slot);
        let fields: &[&str] = match (diff.added.is_some(), diff.deleted.is_some()) {
            (true, true) => &["added", "deleted"],
            (true, false) => &["added"],
            (false, true) => &["deleted"],
            (false, false) => &[],
        };
        write_record_def_header(w, slot, fields);
    }

    if let Some(added) = &diff.added {
        write_map_header(w, added.len());
        for (name, info) in added {
            write_str(w, name);
            encode_cafs_file_info(w, state, info)?;
        }
    }
    if let Some(deleted) = &diff.deleted {
        write_array_header(w, deleted.len());
        for name in deleted {
            write_str(w, name);
        }
    }
    Ok(())
}

/// `d4 72 <slot>` fixext1 header + msgpack array of `fields` as strings.
fn write_record_def_header(w: &mut Vec<u8>, slot: u8, fields: &[&str]) {
    w.push(0xd4);
    w.push(RECORD_DEF_EXT_TYPE);
    w.push(slot);
    write_array_header(w, fields.len());
    for field in fields {
        write_str(w, field);
    }
}

fn write_array_header(w: &mut Vec<u8>, n: usize) {
    if n < 16 {
        w.push(0x90 | (n as u8));
    } else if n <= u16::MAX as usize {
        w.push(0xdc);
        w.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        w.push(0xdd);
        w.extend_from_slice(&(n as u32).to_be_bytes());
    }
}

/// Write an unsigned integer in the smallest MessagePack encoding that
/// is safe inside an active records stream. Values `0x40..=0x7f` cannot
/// be emitted as positive fixints — their byte representation collides
/// with record-slot references — so they get promoted to `uint 8`.
/// msgpackr does the same thing under `useRecords: true` for exactly
/// the same reason. `mode: u32` (e.g. `0o755` = 493) and `size: u64`
/// round-trip through this.
fn write_uint(w: &mut Vec<u8>, v: u64) {
    if v < SLOT_LO as u64 {
        // Positive fixint 0x00..=0x3f — below the slot range, safe to
        // emit bare.
        w.push(v as u8);
    } else if v <= u8::MAX as u64 {
        // Covers 0x40..=0xff; the 0x40..=0x7f sub-range must use uint 8
        // so the decoder doesn't mistake it for a slot byte.
        w.push(0xcc);
        w.push(v as u8);
    } else if v <= u16::MAX as u64 {
        w.push(0xcd);
        w.extend_from_slice(&(v as u16).to_be_bytes());
    } else if v <= u32::MAX as u64 {
        w.push(0xce);
        w.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        w.push(0xcf);
        w.extend_from_slice(&v.to_be_bytes());
    }
}

fn write_float64(w: &mut Vec<u8>, v: f64) {
    w.push(0xcb);
    w.extend_from_slice(&v.to_be_bytes());
}

fn write_bool(w: &mut Vec<u8>, b: bool) {
    w.push(if b { 0xc3 } else { 0xc2 });
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

    // ===== Encoder tests =====
    //
    // The round-trip pattern is: `encode` → `transcode_to_plain_msgpack`
    // → `rmp_serde::from_slice`. The transcoder is the Rust
    // implementation of msgpackr's records wire format, so if bytes
    // round-trip through it cleanly, msgpackr 1.11.8 will too. pnpm's
    // store uses exactly that version, pinned in its `catalog:`.

    fn roundtrip(original: &PackageFilesIndex) -> PackageFilesIndex {
        let bytes = encode_package_files_index(original).expect("encode succeeds");
        let plain = transcode_to_plain_msgpack(&bytes).expect("transcode succeeds");
        rmp_serde::from_slice(&plain).expect("deserialize")
    }

    fn sample_cafs(size: u64, with_checked_at: bool) -> CafsFileInfo {
        CafsFileInfo {
            digest: "a".repeat(128),
            mode: 0o644,
            size,
            checked_at: with_checked_at.then_some(1_700_000_000_000),
        }
    }

    #[test]
    fn encode_emits_record_header_for_top_level_struct() {
        // The whole point: outer struct is a record (fixext1 `d4 72 40`),
        // not a plain msgpack map. Without this, pnpm's msgpackr would
        // decode the row as a top-level JS `Map`, and `pkgIndex.files`
        // (a property access) would be `undefined`.
        let idx = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files: HashMap::new(),
            side_effects: None,
        };
        let bytes = encode_package_files_index(&idx).unwrap();
        assert_eq!(&bytes[0..3], &[0xd4, RECORD_DEF_EXT_TYPE, PKG_FILES_INDEX_SLOT]);
    }

    #[test]
    fn encode_roundtrips_single_file() {
        let mut files = HashMap::new();
        files.insert("index.js".to_string(), sample_cafs(10, true));
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        assert_eq!(roundtrip(&original), original);
    }

    #[test]
    fn encode_roundtrips_many_files_sharing_one_slot() {
        // Second and subsequent `CafsFileInfo` instances must be
        // emitted as bare slot references (one byte), not re-defined.
        // A tarball with N files collapses N × ~34 bytes of field
        // names into N × 1 byte — that's the whole point of records.
        let mut files = HashMap::new();
        for i in 0..5 {
            files.insert(format!("file{i}.js"), sample_cafs(1000 + i as u64, true));
        }
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let bytes = encode_package_files_index(&original).unwrap();
        let record_def_headers =
            bytes.windows(2).filter(|w| *w == [0xd4, RECORD_DEF_EXT_TYPE]).count();
        // Exactly two record defs: one for `PackageFilesIndex`, one
        // for the first `CafsFileInfo` instance. The other four
        // `CafsFileInfo` instances must reference that slot.
        assert_eq!(
            record_def_headers, 2,
            "expected one def per distinct shape, got bytes {bytes:02x?}"
        );
        assert_eq!(roundtrip(&original), original);
    }

    #[test]
    fn encode_handles_fixint_in_slot_range_safely() {
        // `size: 0x7b` (= 123) falls inside the slot-reference range
        // 0x40..=0x7f. A naive encoder that emits it as a positive
        // fixint would produce a byte stream the decoder then
        // interprets as a reference to slot 0x7b, which is never
        // defined — the classic "UnknownSlot" blow-up. msgpackr
        // promotes all integers in this range to `uint 8` for exactly
        // this reason.
        let mut files = HashMap::new();
        files.insert("f".to_string(), sample_cafs(0x7b, true));
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        assert_eq!(roundtrip(&original).files.get("f").unwrap().size, 0x7b);
    }

    #[test]
    fn encode_omits_checked_at_when_none() {
        // `None` `checkedAt` is *omitted* from the record schema
        // rather than encoded as `nil` — matches msgpackr's
        // field-omit-when-absent behaviour, so pnpm's reader sees the
        // same object shape it would produce on its own output (the
        // `checkedAt` property is missing, not `null`). Round-trip
        // through our transcoder still recovers `None` because
        // `Option<u64>` deserializes a missing field to `None`.
        let mut files = HashMap::new();
        files.insert("f".to_string(), sample_cafs(100, false));
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let bytes = encode_package_files_index(&original).unwrap();
        // No `checkedAt` string should appear — the schema for this
        // `CafsFileInfo` only has `[digest, mode, size]`.
        let needle = b"checkedAt";
        assert!(
            bytes.windows(needle.len()).all(|w| w != needle),
            "checkedAt leaked into output when the field was None: {bytes:02x?}"
        );
        assert_eq!(roundtrip(&original).files.get("f").unwrap().checked_at, None);
    }

    #[test]
    fn encode_allocates_separate_slots_for_distinct_cafs_shapes() {
        // Two `CafsFileInfo` instances with different shapes — one
        // carries `checkedAt`, the other doesn't — must land in
        // different slots. Same shape reuses its slot, which is the
        // whole point of records. msgpackr does the same: slot 0x41
        // for the first shape seen, 0x42 for the next new one, etc.
        let mut files = HashMap::new();
        files.insert("with_ts.js".to_string(), sample_cafs(10, true));
        files.insert("no_ts_a.js".to_string(), sample_cafs(20, false));
        files.insert("no_ts_b.js".to_string(), sample_cafs(30, false));
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: None,
        };
        let bytes = encode_package_files_index(&original).unwrap();
        let record_def_headers =
            bytes.windows(2).filter(|w| *w == [0xd4, RECORD_DEF_EXT_TYPE]).count();
        // Exactly three defs: `PackageFilesIndex`, `CafsFileInfo` with
        // checkedAt, `CafsFileInfo` without checkedAt. The third file
        // (second no-ts instance) shares the no-ts slot, so no fourth
        // def.
        assert_eq!(
            record_def_headers, 3,
            "expected three defs (outer + two CafsFileInfo shapes), got bytes {bytes:02x?}"
        );
        assert_eq!(roundtrip(&original), original);
    }

    #[test]
    fn encode_requires_build_when_set() {
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: Some(true),
            algo: "sha512".to_string(),
            files: HashMap::new(),
            side_effects: None,
        };
        let roundtripped = roundtrip(&original);
        assert_eq!(roundtripped.requires_build, Some(true));
    }

    #[test]
    fn encode_omits_requires_build_when_none() {
        // When `requires_build` is `None`, it must not appear in the
        // record schema at all — matching msgpackr's own behaviour of
        // field-omit-when-absent for plain JS objects with missing
        // properties. This keeps the byte output minimal for the
        // common case (pacquet rarely populates requires_build).
        let idx = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files: HashMap::new(),
            side_effects: None,
        };
        let bytes = encode_package_files_index(&idx).unwrap();
        // Scan for the bytes that spell "requiresBuild" — must not
        // appear anywhere in the output.
        let needle = b"requiresBuild";
        assert!(
            bytes.windows(needle.len()).all(|w| w != needle),
            "requiresBuild leaked into output when the field was None: {bytes:02x?}"
        );
    }

    #[test]
    fn encode_side_effects_roundtrip() {
        let mut added = HashMap::new();
        added.insert("foo.so".to_string(), sample_cafs(42, true));
        let mut side_effects = HashMap::new();
        side_effects.insert(
            "linux".to_string(),
            SideEffectsDiff { added: Some(added), deleted: Some(vec!["bar.o".to_string()]) },
        );
        let mut files = HashMap::new();
        files.insert("main.js".to_string(), sample_cafs(10, true));
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files,
            side_effects: Some(side_effects),
        };
        assert_eq!(roundtrip(&original), original);
    }

    #[test]
    fn encode_side_effects_with_only_added_omits_deleted_field() {
        // A `SideEffectsDiff` with `deleted: None` must not emit a
        // `deleted` field name in the record schema. This is the case
        // Copilot flagged: the fixed-schema encoder used to write
        // `deleted: nil` here, producing a JS shape (`{ added, deleted:
        // null }`) different from what msgpackr itself produces for
        // the same Rust input (`{ added }`).
        let mut added = HashMap::new();
        added.insert("foo.so".to_string(), sample_cafs(42, true));
        let mut side_effects = HashMap::new();
        side_effects
            .insert("linux".to_string(), SideEffectsDiff { added: Some(added), deleted: None });
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files: HashMap::new(),
            side_effects: Some(side_effects),
        };
        let bytes = encode_package_files_index(&original).unwrap();
        assert!(
            bytes.windows(7).all(|w| w != b"deleted"),
            "`deleted` field name appeared in output when the field was None: {bytes:02x?}"
        );
        assert_eq!(roundtrip(&original), original);
    }

    #[test]
    fn encode_allocates_separate_slots_for_distinct_side_effects_shapes() {
        // Two `SideEffectsDiff` instances with distinct shapes (one
        // with only `added`, one with only `deleted`) must land in
        // different slots, mirroring msgpackr's behaviour on the same
        // input.
        let mut linux_added = HashMap::new();
        linux_added.insert("foo.so".to_string(), sample_cafs(42, true));
        let mut side_effects = HashMap::new();
        side_effects.insert(
            "linux".to_string(),
            SideEffectsDiff { added: Some(linux_added), deleted: None },
        );
        side_effects.insert(
            "darwin".to_string(),
            SideEffectsDiff { added: None, deleted: Some(vec!["bar.o".to_string()]) },
        );
        let original = PackageFilesIndex {
            manifest: None,
            requires_build: None,
            algo: "sha512".to_string(),
            files: HashMap::new(),
            side_effects: Some(side_effects),
        };
        let bytes = encode_package_files_index(&original).unwrap();
        let record_def_headers =
            bytes.windows(2).filter(|w| *w == [0xd4, RECORD_DEF_EXT_TYPE]).count();
        // Three defs: outer `PackageFilesIndex`, `SideEffectsDiff`
        // shape-`added`, `SideEffectsDiff` shape-`deleted`. The inner
        // `CafsFileInfo` adds a fourth.
        assert_eq!(
            record_def_headers, 4,
            "expected defs for outer + two distinct side-effects shapes + CafsFileInfo, got bytes {bytes:02x?}"
        );
        assert_eq!(roundtrip(&original), original);
    }

    #[test]
    fn allocate_slot_returns_error_past_0x7f() {
        // Exhaust the inner-slot range (0x41..=0x7f, 63 slots) and
        // verify the next call returns `OutOfRecordSlots` rather than
        // panicking. Not reachable through the public encoder for
        // current pacquet payloads — `CafsFileInfo` has 2 shapes,
        // `SideEffectsDiff` has 4 — but the error path must exist in
        // case a future record type is added without bumping the
        // shape accounting.
        let mut state = EncodeState::new();
        for _ in FIRST_INNER_SLOT..=SLOT_HI {
            state.allocate_slot().expect("should succeed within the slot range");
        }
        let err = state.allocate_slot().expect_err("64th allocation must fail");
        assert!(matches!(err, EncodeError::OutOfRecordSlots { max: 63 }), "got {err:?}");
    }

    #[test]
    fn encode_rejects_manifest_some() {
        // pacquet doesn't populate `manifest` today; encoding a
        // `Some` value is unimplemented. Fail loudly rather than
        // silently dropping it — if/when the field starts carrying
        // real data, this test trips and we implement the path.
        let idx = PackageFilesIndex {
            manifest: Some(serde_json::json!({ "name": "x" })),
            requires_build: None,
            algo: "sha512".to_string(),
            files: HashMap::new(),
            side_effects: None,
        };
        let err = encode_package_files_index(&idx).unwrap_err();
        assert!(matches!(err, EncodeError::ManifestNotSupported), "got {err:?}");
    }
}
