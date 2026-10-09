// Borrowed from PhotoCraft (https://github.com/storytold/photocraft)
// Copyright (c) 2026 storytold / PhotoCraft authors
// Licensed under MIT or Apache-2.0

//! Photoshop ActionDescriptor structures (OSType-tagged key/value trees).
//!
//! These appear inside many tagged blocks (`lfx2`, `SoCo`, `GdFl`, `TySh`,
//! `PlLd`, …) and brush preset files (`.abr`), usually wrapped in a
//! [`VersionedDescriptor`] (version 16).
//! Parsing and writing are exact: `write(parse(b)) == b` for well-formed input.

use crate::psd_io::{ByteReader, WriteExt, read_unicode_units, write_unicode_units};
use crate::{Error, Result};

fn bad(what: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("Descriptor: {what}"))
}

/// Maximum nesting depth accepted when parsing (fuzz/recursion safety).
pub const MAX_DEPTH: usize = 64;

/// A Photoshop Unicode string: UTF-16 code units, preserved exactly
/// (including any trailing NUL and unpaired surrogates).
#[derive(Debug, Clone, PartialEq, Eq, Default, Hash, serde::Serialize, serde::Deserialize)]
pub struct UnicodeString(pub Vec<u16>);

impl UnicodeString {
    /// Encodes `s` as UTF-16 without a trailing NUL.
    pub fn new(s: &str) -> Self {
        UnicodeString(s.encode_utf16().collect())
    }

    /// Encodes `s` as UTF-16 followed by a NUL terminator (as Photoshop
    /// commonly writes in descriptors).
    pub fn new_nul(s: &str) -> Self {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        UnicodeString(v)
    }

    /// Lossy conversion, stripping trailing NULs.
    pub fn to_string_lossy(&self) -> String {
        let mut end = self.0.len();
        while end > 0 && self.0[end - 1] == 0 {
            end -= 1;
        }
        String::from_utf16_lossy(&self.0[..end])
    }

    pub(crate) fn read(r: &mut ByteReader<'_>) -> Result<Self> {
        Ok(UnicodeString(read_unicode_units(r)?))
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        write_unicode_units(out, &self.0);
    }
}

/// A class / key / type identifier: a length-prefixed string, where a stored
/// length of 0 means a 4-character code follows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Id {
    /// Stored as length 0 + 4 bytes.
    Code([u8; 4]),
    /// Stored as explicit length + bytes (e.g. `"artboardRect"`).
    Str(Vec<u8>),
}

impl Id {
    /// Convenience constructor: 4-byte strings become [`Id::Code`], others
    /// [`Id::Str`].
    pub fn new(s: &str) -> Self {
        let b = s.as_bytes();
        if b.len() == 4 {
            Id::Code([b[0], b[1], b[2], b[3]])
        } else {
            Id::Str(b.to_vec())
        }
    }

    /// The identifier's bytes.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Id::Code(c) => c,
            Id::Str(s) => s,
        }
    }

    /// `true` if the identifier's bytes equal `s`.
    pub fn is(&self, s: &str) -> bool {
        self.as_bytes() == s.as_bytes()
    }

    pub(crate) fn read(r: &mut ByteReader<'_>) -> Result<Self> {
        let n = r.u32()?;
        if n == 0 {
            Ok(Id::Code(r.array()?))
        } else {
            Ok(Id::Str(r.bytes_u64(u64::from(n))?.to_vec()))
        }
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        match self {
            Id::Code(c) => {
                out.put_u32(0);
                out.put(c);
            }
            Id::Str(s) => {
                out.put_u32(s.len() as u32);
                out.put(s);
            }
        }
    }
}

/// A class reference: display name + class id.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Class {
    /// Display name.
    pub name: UnicodeString,
    /// Class identifier.
    pub class_id: Id,
}

impl Class {
    pub(crate) fn read(r: &mut ByteReader<'_>) -> Result<Self> {
        Ok(Class {
            name: UnicodeString::read(r)?,
            class_id: Id::read(r)?,
        })
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        self.name.write(out);
        self.class_id.write(out);
    }
}

/// One element of an `'obj '` reference.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ReferenceItem {
    /// `prop`
    Property { class: Class, key: Id },
    /// `Clss`
    Class(Class),
    /// `Enmr`
    Enumerated { class: Class, type_id: Id, value: Id },
    /// `rele`
    Offset { class: Class, offset: i32 },
    /// `Idnt`
    Identifier(u32),
    /// `indx`
    Index(u32),
    /// `name`
    Name { class: Class, name: UnicodeString },
}

/// A descriptor value, tagged by its OSType.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Value {
    /// `obj `
    Reference(Vec<ReferenceItem>),
    /// `Objc`
    Descriptor(Descriptor),
    /// `GlbO`
    GlobalObject(Descriptor),
    /// `VlLs`
    List(Vec<Value>),
    /// `doub`
    Double(f64),
    /// `UntF`: unit (`#Ang`, `#Rsl`, `#Rlt`, `#Nne`, `#Prc`, `#Pxl`, …) + value.
    UnitFloat { unit: [u8; 4], value: f64 },
    /// `UnFl`: unit + array of doubles (used in `ObAr`).
    UnitFloats { unit: [u8; 4], values: Vec<f64> },
    /// `TEXT`
    Text(UnicodeString),
    /// `enum`
    Enumerated { type_id: Id, value: Id },
    /// `long`
    Integer(i32),
    /// `comp`
    LargeInteger(i64),
    /// `bool`
    Boolean(bool),
    /// `type`
    Class(Class),
    /// `GlbC`
    GlobalClass(Class),
    /// `alis`: raw alias data.
    Alias(Vec<u8>),
    /// `Pth `: raw file path data (length-prefixed).
    Path(Vec<u8>),
    /// `tdta`: raw data.
    RawData(Vec<u8>),
    /// `ObAr`: object array (best effort; see [`ObjectArray`]).
    ObjectArray(ObjectArray),
}

impl Value {
    /// Returns the value as `f64` if it is `Double`, `UnitFloat`, `Integer`, or `LargeInteger`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Double(d) => Some(*d),
            Value::UnitFloat { value, .. } => Some(*value),
            Value::Integer(i) => Some(*i as f64),
            Value::LargeInteger(l) => Some(*l as f64),
            _ => None,
        }
    }

    /// Returns the text as a lossy string if it is `Text`.
    pub fn as_str(&self) -> Option<String> {
        match self {
            Value::Text(u) => Some(u.to_string_lossy()),
            _ => None,
        }
    }

    /// Returns the descriptor if it is `Descriptor` or `GlobalObject`.
    pub fn as_descriptor(&self) -> Option<&Descriptor> {
        match self {
            Value::Descriptor(d) | Value::GlobalObject(d) => Some(d),
            _ => None,
        }
    }
}

/// `ObAr` object array. Undocumented in the Adobe spec; the layout follows
/// ag-psd (MIT): a u32 (observed as the element count / version 16) followed
/// by a descriptor-shaped body whose values are usually `UnFl`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ObjectArray {
    /// Leading u32, preserved.
    pub prefix: u32,
    /// Body.
    pub body: Descriptor,
}

/// An ActionDescriptor: class plus ordered key/value items.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Descriptor {
    /// Display name of the class (often empty).
    pub name: UnicodeString,
    /// Class id.
    pub class_id: Id,
    /// Items in stored order.
    pub items: Vec<(Id, Value)>,
}

impl Default for Descriptor {
    fn default() -> Self {
        Descriptor::new("null")
    }
}

impl Descriptor {
    /// Creates an empty descriptor of the given class.
    pub fn new(class_id: &str) -> Self {
        Descriptor {
            name: UnicodeString::default(),
            class_id: Id::new(class_id),
            items: Vec::new(),
        }
    }

    /// Builder-style item append.
    pub fn with(mut self, key: &str, value: Value) -> Self {
        self.items.push((Id::new(key), value));
        self
    }

    /// Looks up the first item with key `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.items.iter().find(|(k, _)| k.is(key)).map(|(_, v)| v)
    }

    /// Parses a descriptor from `data`, which must be consumed entirely.
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut r = ByteReader::new(data);
        let d = Self::read(&mut r)?;
        if !r.is_empty() {
            return Err(bad("trailing bytes after descriptor"));
        }
        Ok(d)
    }

    /// Serializes the descriptor.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.write(&mut v);
        v
    }

    pub(crate) fn read(r: &mut ByteReader<'_>) -> Result<Self> {
        read_descriptor(r, 0)
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        self.name.write(out);
        self.class_id.write(out);
        out.put_u32(self.items.len() as u32);
        for (k, v) in &self.items {
            k.write(out);
            write_value(v, out);
        }
    }

    // --- Helpers for Photoshop brush presets ---

    /// Returns the preset name if this descriptor has a `"Nm  "` string.
    pub fn name(&self) -> Option<String> {
        self.get("Nm  ").and_then(Value::as_str)
    }

    /// Returns the embedded brush tip descriptor (`"Brsh"`).
    pub fn brush_tip_descriptor(&self) -> Option<&Descriptor> {
        self.get("Brsh").and_then(Value::as_descriptor)
    }

    /// Returns the sampled tip UUID if this is a sampled brush (`"sampledData"` in `"Brsh"`).
    pub fn sampled_tip_id(&self) -> Option<String> {
        self.brush_tip_descriptor()
            .and_then(|b| b.get("sampledData"))
            .and_then(Value::as_str)
    }

    /// Returns diameter in pixels (`"Dmtr"` in `"Brsh"`).
    pub fn diameter(&self) -> Option<f64> {
        self.brush_tip_descriptor()
            .and_then(|b| b.get("Dmtr"))
            .and_then(Value::as_f64)
    }

    /// Returns hardness 0..=100 (`"Hrdn"` in `"Brsh"`).
    pub fn hardness(&self) -> Option<f64> {
        self.brush_tip_descriptor()
            .and_then(|b| b.get("Hrdn"))
            .and_then(Value::as_f64)
    }

    /// Returns angle in degrees (`"Angl"` in `"Brsh"`).
    pub fn angle(&self) -> Option<f64> {
        self.brush_tip_descriptor()
            .and_then(|b| b.get("Angl"))
            .and_then(Value::as_f64)
    }

    /// Returns roundness 0..=100 (`"Rndn"` in `"Brsh"`).
    pub fn roundness(&self) -> Option<f64> {
        self.brush_tip_descriptor()
            .and_then(|b| b.get("Rndn"))
            .and_then(Value::as_f64)
    }

    /// Returns spacing percentage (`"Spcn"` in `"Brsh"`).
    pub fn spacing(&self) -> Option<f64> {
        self.brush_tip_descriptor()
            .and_then(|b| b.get("Spcn"))
            .and_then(Value::as_f64)
    }
}

/// A descriptor prefixed by a u32 version (16 in all current Photoshop files).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VersionedDescriptor {
    /// Descriptor version (16).
    pub version: u32,
    /// The descriptor.
    pub descriptor: Descriptor,
}

impl VersionedDescriptor {
    /// Wraps a descriptor with version 16.
    pub fn new(descriptor: Descriptor) -> Self {
        VersionedDescriptor {
            version: 16,
            descriptor,
        }
    }

    /// Parses from bytes (must be fully consumed).
    pub fn from_bytes(data: &[u8]) -> Result<Self> {
        let mut r = ByteReader::new(data);
        let v = Self::read(&mut r)?;
        if !r.is_empty() {
            return Err(bad("trailing bytes after versioned descriptor"));
        }
        Ok(v)
    }

    /// Parses a versioned descriptor at the start of `data`, returning it and
    /// the number of bytes consumed.
    pub fn parse_prefix(data: &[u8]) -> Result<(Self, usize)> {
        let mut r = ByteReader::new(data);
        let v = Self::read(&mut r)?;
        Ok((v, r.pos()))
    }

    /// Serializes to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.write(&mut v);
        v
    }

    pub(crate) fn read(r: &mut ByteReader<'_>) -> Result<Self> {
        let version = r.u32()?;
        if version != 16 {
            return Err(bad(format!("descriptor version {version}, expected 16")));
        }
        Ok(VersionedDescriptor {
            version,
            descriptor: Descriptor::read(r)?,
        })
    }

    pub(crate) fn write(&self, out: &mut Vec<u8>) {
        out.put_u32(self.version);
        self.descriptor.write(out);
    }
}

fn read_descriptor(r: &mut ByteReader<'_>, depth: usize) -> Result<Descriptor> {
    if depth > MAX_DEPTH {
        return Err(bad("descriptor nesting too deep"));
    }
    let name = UnicodeString::read(r)?;
    let class_id = Id::read(r)?;
    let n = r.u32()?;
    r.check_count(u64::from(n), 10)?;
    let mut items = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let key = Id::read(r)?;
        let v = read_value(r, depth + 1)?;
        items.push((key, v));
    }
    Ok(Descriptor {
        name,
        class_id,
        items,
    })
}

fn read_value(r: &mut ByteReader<'_>, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH {
        return Err(bad("descriptor nesting too deep"));
    }
    let ty = r.array::<4>()?;
    read_typed_value(r, &ty, depth)
}

fn read_typed_value(r: &mut ByteReader<'_>, ty: &[u8; 4], depth: usize) -> Result<Value> {
    Ok(match ty {
        b"obj " => {
            let n = r.u32()?;
            r.check_count(u64::from(n), 8)?;
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                items.push(read_reference_item(r)?);
            }
            Value::Reference(items)
        }
        b"Objc" => Value::Descriptor(read_descriptor(r, depth + 1)?),
        b"GlbO" => Value::GlobalObject(read_descriptor(r, depth + 1)?),
        b"VlLs" => {
            let n = r.u32()?;
            r.check_count(u64::from(n), 4)?;
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                items.push(read_value(r, depth + 1)?);
            }
            Value::List(items)
        }
        b"doub" => Value::Double(r.f64()?),
        b"UntF" => Value::UnitFloat {
            unit: r.array()?,
            value: r.f64()?,
        },
        b"UnFl" => {
            let unit = r.array()?;
            let n = r.u32()?;
            r.check_count(u64::from(n), 8)?;
            let mut values = Vec::with_capacity(n as usize);
            for _ in 0..n {
                values.push(r.f64()?);
            }
            Value::UnitFloats { unit, values }
        }
        b"TEXT" => Value::Text(UnicodeString::read(r)?),
        b"enum" => Value::Enumerated {
            type_id: Id::read(r)?,
            value: Id::read(r)?,
        },
        b"long" => Value::Integer(r.i32()?),
        b"comp" => Value::LargeInteger(r.i64()?),
        b"bool" => Value::Boolean(r.u8()? != 0),
        b"type" => Value::Class(Class::read(r)?),
        b"GlbC" => Value::GlobalClass(Class::read(r)?),
        b"alis" => {
            let n = r.u32()?;
            Value::Alias(r.bytes_u64(u64::from(n))?.to_vec())
        }
        b"Pth " => {
            let n = r.u32()?;
            Value::Path(r.bytes_u64(u64::from(n))?.to_vec())
        }
        b"tdta" => {
            let n = r.u32()?;
            Value::RawData(r.bytes_u64(u64::from(n))?.to_vec())
        }
        b"ObAr" => {
            let prefix = r.u32()?;
            Value::ObjectArray(ObjectArray {
                prefix,
                body: read_descriptor(r, depth + 1)?,
            })
        }
        other => {
            return Err(bad(format!(
                "descriptor OSType {:?}",
                String::from_utf8_lossy(other)
            )));
        }
    })
}

fn read_reference_item(r: &mut ByteReader<'_>) -> Result<ReferenceItem> {
    let ty = r.array::<4>()?;
    Ok(match &ty {
        b"prop" => ReferenceItem::Property {
            class: Class::read(r)?,
            key: Id::read(r)?,
        },
        b"Clss" => ReferenceItem::Class(Class::read(r)?),
        b"Enmr" => ReferenceItem::Enumerated {
            class: Class::read(r)?,
            type_id: Id::read(r)?,
            value: Id::read(r)?,
        },
        b"rele" => ReferenceItem::Offset {
            class: Class::read(r)?,
            offset: r.i32()?,
        },
        b"Idnt" => ReferenceItem::Identifier(r.u32()?),
        b"indx" => ReferenceItem::Index(r.u32()?),
        b"name" => ReferenceItem::Name {
            class: Class::read(r)?,
            name: UnicodeString::read(r)?,
        },
        other => {
            return Err(bad(format!(
                "reference item type {:?}",
                String::from_utf8_lossy(other)
            )));
        }
    })
}

fn value_type(v: &Value) -> &'static [u8; 4] {
    match v {
        Value::Reference(_) => b"obj ",
        Value::Descriptor(_) => b"Objc",
        Value::GlobalObject(_) => b"GlbO",
        Value::List(_) => b"VlLs",
        Value::Double(_) => b"doub",
        Value::UnitFloat { .. } => b"UntF",
        Value::UnitFloats { .. } => b"UnFl",
        Value::Text(_) => b"TEXT",
        Value::Enumerated { .. } => b"enum",
        Value::Integer(_) => b"long",
        Value::LargeInteger(_) => b"comp",
        Value::Boolean(_) => b"bool",
        Value::Class(_) => b"type",
        Value::GlobalClass(_) => b"GlbC",
        Value::Alias(_) => b"alis",
        Value::Path(_) => b"Pth ",
        Value::RawData(_) => b"tdta",
        Value::ObjectArray(_) => b"ObAr",
    }
}

fn write_value(v: &Value, out: &mut Vec<u8>) {
    out.put(value_type(v));
    match v {
        Value::Reference(items) => {
            out.put_u32(items.len() as u32);
            for it in items {
                write_reference_item(it, out);
            }
        }
        Value::Descriptor(d) | Value::GlobalObject(d) => d.write(out),
        Value::List(items) => {
            out.put_u32(items.len() as u32);
            for it in items {
                write_value(it, out);
            }
        }
        Value::Double(d) => out.put_f64(*d),
        Value::UnitFloat { unit, value } => {
            out.put(unit);
            out.put_f64(*value);
        }
        Value::UnitFloats { unit, values } => {
            out.put(unit);
            out.put_u32(values.len() as u32);
            for v in values {
                out.put_f64(*v);
            }
        }
        Value::Text(s) => s.write(out),
        Value::Enumerated { type_id, value } => {
            type_id.write(out);
            value.write(out);
        }
        Value::Integer(i) => out.put_i32(*i),
        Value::LargeInteger(c) => out.put_i64(*c),
        Value::Boolean(b) => out.put_u8(u8::from(*b)),
        Value::Class(c) | Value::GlobalClass(c) => c.write(out),
        Value::Alias(a) => {
            out.put_u32(a.len() as u32);
            out.put(a);
        }
        Value::Path(p) => {
            out.put_u32(p.len() as u32);
            out.put(p);
        }
        Value::RawData(d) => {
            out.put_u32(d.len() as u32);
            out.put(d);
        }
        Value::ObjectArray(o) => {
            out.put_u32(o.prefix);
            o.body.write(out);
        }
    }
}

fn write_reference_item(it: &ReferenceItem, out: &mut Vec<u8>) {
    match it {
        ReferenceItem::Property { class, key } => {
            out.put(b"prop");
            class.write(out);
            key.write(out);
        }
        ReferenceItem::Class(class) => {
            out.put(b"Clss");
            class.write(out);
        }
        ReferenceItem::Enumerated {
            class,
            type_id,
            value,
        } => {
            out.put(b"Enmr");
            class.write(out);
            type_id.write(out);
            value.write(out);
        }
        ReferenceItem::Offset { class, offset } => {
            out.put(b"rele");
            class.write(out);
            out.put_i32(*offset);
        }
        ReferenceItem::Identifier(id) => {
            out.put(b"Idnt");
            out.put_u32(*id);
        }
        ReferenceItem::Index(idx) => {
            out.put(b"indx");
            out.put_u32(*idx);
        }
        ReferenceItem::Name { class, name } => {
            out.put(b"name");
            class.write(out);
            name.write(out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(name: &str, id: &str) -> Class {
        Class {
            name: UnicodeString::new(name),
            class_id: Id::new(id),
        }
    }

    pub(crate) fn sample() -> Descriptor {
        Descriptor::new("null")
            .with("Nm  ", Value::Text(UnicodeString::new_nul("Hello \u{1F600}")))
            .with("Opct", Value::UnitFloat { unit: *b"#Prc", value: 75.5 })
            .with("enab", Value::Boolean(true))
            .with("long", Value::Integer(-5))
            .with("comp", Value::LargeInteger(1 << 40))
            .with("doub", Value::Double(0.25))
            .with(
                "Md  ",
                Value::Enumerated {
                    type_id: Id::new("BlnM"),
                    value: Id::new("Mltp"),
                },
            )
            .with(
                "artboardRect",
                Value::Descriptor(Descriptor::new("classFloatRect").with("Top ", Value::Double(1.0))),
            )
            .with(
                "list",
                Value::List(vec![Value::Integer(1), Value::Text(UnicodeString::new("x"))]),
            )
            .with(
                "null",
                Value::Reference(vec![
                    ReferenceItem::Property {
                        class: class("", "Lyr "),
                        key: Id::new("Nm  "),
                    },
                    ReferenceItem::Class(class("", "Dcmn")),
                    ReferenceItem::Enumerated {
                        class: class("", "Lyr "),
                        type_id: Id::new("Ordn"),
                        value: Id::new("Trgt"),
                    },
                    ReferenceItem::Offset {
                        class: class("", "Lyr "),
                        offset: -1,
                    },
                    ReferenceItem::Identifier(7),
                    ReferenceItem::Index(2),
                    ReferenceItem::Name {
                        class: class("", "Lyr "),
                        name: UnicodeString::new("Bg"),
                    },
                ]),
            )
            .with("type", Value::Class(class("c", "Clr ")))
            .with("glbc", Value::GlobalClass(class("c", "RGBC")))
            .with("glbo", Value::GlobalObject(Descriptor::default()))
            .with("alis", Value::Alias(vec![1, 2, 3]))
            .with("tdta", Value::RawData(vec![0xde, 0xad]))
            .with("pth ", Value::Path(vec![9, 9]))
            .with(
                "ObAr",
                Value::ObjectArray(ObjectArray {
                    prefix: 16,
                    body: Descriptor::new("rationalPoint").with(
                        "Hrzn",
                        Value::UnitFloats {
                            unit: *b"#Pxl",
                            values: vec![1.0, 2.0],
                        },
                    ),
                }),
            )
    }

    #[test]
    fn roundtrip_all_types() {
        let d = sample();
        let b = d.to_bytes();
        let p = Descriptor::from_bytes(&b).unwrap();
        assert_eq!(p, d);
        assert_eq!(p.to_bytes(), b);
    }

    #[test]
    fn versioned_roundtrip() {
        let v = VersionedDescriptor::new(sample());
        let b = v.to_bytes();
        assert_eq!(&b[..4], &[0, 0, 0, 16]);
        assert_eq!(VersionedDescriptor::from_bytes(&b).unwrap(), v);
    }

    #[test]
    fn versioned_parse_prefix() {
        let v = VersionedDescriptor::new(sample());
        let mut b = v.to_bytes();
        let n = b.len();
        b.extend_from_slice(&[1, 2, 3]);
        let (p, used) = VersionedDescriptor::parse_prefix(&b).unwrap();
        assert_eq!(p, v);
        assert_eq!(used, n);
    }

    #[test]
    fn versioned_rejects_other_version() {
        let mut b = VersionedDescriptor::new(Descriptor::new("null")).to_bytes();
        b[3] = 15;
        assert!(VersionedDescriptor::from_bytes(&b).is_err());
    }

    #[test]
    fn id_encoding() {
        let mut v = Vec::new();
        Id::new("Nm  ").write(&mut v);
        assert_eq!(v, [0, 0, 0, 0, b'N', b'm', b' ', b' ']);
        let mut v = Vec::new();
        Id::new("abc").write(&mut v);
        assert_eq!(v, [0, 0, 0, 3, b'a', b'b', b'c']);

        let data = [0, 0, 0, 4, b'a', b'b', b'c', b'd'];
        let id = Id::read(&mut ByteReader::new(&data)).unwrap();
        assert_eq!(id, Id::Str(b"abcd".to_vec()));
        let mut v = Vec::new();
        id.write(&mut v);
        assert_eq!(v, data);
    }

    #[test]
    fn get_and_lookup() {
        let d = sample();
        assert_eq!(d.get("long"), Some(&Value::Integer(-5)));
        assert!(d.get("nope").is_none());
        assert_eq!(d.name(), Some("Hello \u{1F600}".to_string()));
    }

    #[test]
    fn unicode_lossy() {
        assert_eq!(UnicodeString::new_nul("ab").to_string_lossy(), "ab");
        assert_eq!(
            UnicodeString(vec![0xd800, 0x41]).to_string_lossy(),
            "\u{fffd}A"
        );
    }

    #[test]
    fn truncations_error() {
        let b = sample().to_bytes();
        for cut in 0..b.len() {
            assert!(Descriptor::from_bytes(&b[..cut]).is_err(), "cut {cut}");
        }
    }

    #[test]
    fn trailing_bytes_error() {
        let mut b = sample().to_bytes();
        b.push(0);
        assert!(Descriptor::from_bytes(&b).is_err());
    }

    #[test]
    fn unknown_ostype_errors() {
        let mut b = Descriptor::new("null").to_bytes();
        let n = b.len();
        b[n - 1] = 1; // one item
        b.extend_from_slice(&[0, 0, 0, 0, b'k', b'e', b'y', b' ']);
        b.extend_from_slice(b"????");
        assert!(Descriptor::from_bytes(&b).is_err());
    }

    #[test]
    fn deep_nesting_is_limited() {
        let mut d = Descriptor::new("null");
        for _ in 0..(MAX_DEPTH + 5) {
            d = Descriptor::new("null").with("in  ", Value::Descriptor(d));
        }
        let b = d.to_bytes();
        assert!(Descriptor::from_bytes(&b).is_err());
    }

    #[test]
    fn huge_counts_rejected_without_alloc() {
        let mut b = Vec::new();
        b.put_u32(0);
        b.put_u32(0);
        b.put(b"null");
        b.put_u32(u32::MAX);
        assert!(Descriptor::from_bytes(&b).is_err());
    }
}
