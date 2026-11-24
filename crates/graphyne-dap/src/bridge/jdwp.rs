//! JDWP, the Java Debug Wire Protocol (docs.oracle.com/en/java/javase/21/docs/specs/jdwp):
//! packets and the values in them. A packet is an 11-byte header (length, id, flags, then the
//! command set and command, or the error code of a reply) and big-endian data; object, class,
//! method, field and frame ids have the sizes the VM reports (`IDSizes`).

use std::io::{Read, Write};

pub const HANDSHAKE: &[u8] = b"JDWP-Handshake";

// Command sets and commands the bridge uses.
pub mod cmd {
    pub const VM_ALL_THREADS: (u8, u8) = (1, 4);
    pub const VM_ID_SIZES: (u8, u8) = (1, 7);
    pub const VM_SUSPEND: (u8, u8) = (1, 8);
    pub const VM_RESUME: (u8, u8) = (1, 9);
    pub const VM_EXIT: (u8, u8) = (1, 10);
    pub const VM_DISPOSE: (u8, u8) = (1, 6);
    pub const VM_ALL_CLASSES: (u8, u8) = (1, 3);
    pub const RT_SIGNATURE: (u8, u8) = (2, 1);
    pub const RT_FIELDS: (u8, u8) = (2, 4);
    pub const RT_METHODS: (u8, u8) = (2, 5);
    pub const RT_SOURCE_FILE: (u8, u8) = (2, 7);
    pub const CT_SUPERCLASS: (u8, u8) = (3, 1);
    pub const M_LINE_TABLE: (u8, u8) = (6, 1);
    pub const M_VARIABLE_TABLE: (u8, u8) = (6, 2);
    pub const OR_REFERENCE_TYPE: (u8, u8) = (9, 1);
    pub const OR_GET_VALUES: (u8, u8) = (9, 2);
    pub const OR_INVOKE_METHOD: (u8, u8) = (9, 6);
    pub const SR_VALUE: (u8, u8) = (10, 1);
    pub const TR_NAME: (u8, u8) = (11, 1);
    pub const TR_RESUME: (u8, u8) = (11, 3);
    pub const TR_FRAMES: (u8, u8) = (11, 6);
    pub const AR_LENGTH: (u8, u8) = (13, 1);
    pub const AR_GET_VALUES: (u8, u8) = (13, 2);
    pub const ER_SET: (u8, u8) = (15, 1);
    pub const ER_CLEAR: (u8, u8) = (15, 2);
    pub const SF_GET_VALUES: (u8, u8) = (16, 1);
    pub const SF_THIS_OBJECT: (u8, u8) = (16, 3);
    /// Events arrive as this command, sent by the VM.
    pub const EVENT_COMPOSITE: (u8, u8) = (64, 100);
}

pub mod event {
    pub const SINGLE_STEP: u8 = 1;
    pub const BREAKPOINT: u8 = 2;
    pub const EXCEPTION: u8 = 4;
    pub const THREAD_START: u8 = 6;
    pub const THREAD_DEATH: u8 = 7;
    pub const CLASS_PREPARE: u8 = 8;
    pub const CLASS_UNLOAD: u8 = 9;
    pub const VM_START: u8 = 90;
    pub const VM_DEATH: u8 = 99;
}

pub mod error {
    pub const ABSENT_INFORMATION: u16 = 101;
}

/// The sizes of the ids in this VM (8 bytes each on HotSpot).
#[derive(Clone, Copy, Debug)]
pub struct IdSizes {
    pub field: usize,
    pub method: usize,
    pub object: usize,
    pub reftype: usize,
    pub frame: usize,
}

impl Default for IdSizes {
    fn default() -> Self {
        Self { field: 8, method: 8, object: 8, reftype: 8, frame: 8 }
    }
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub id: u32,
    pub reply: bool,
    pub command: (u8, u8),
    pub error: u16,
    pub data: Vec<u8>,
}

pub fn read_packet(input: &mut impl Read) -> std::io::Result<Packet> {
    let mut header = [0u8; 11];
    input.read_exact(&mut header)?;
    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let id = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let flags = header[8];
    let mut data = vec![0; len.saturating_sub(11)];
    input.read_exact(&mut data)?;
    let reply = flags & 0x80 != 0;
    Ok(Packet {
        id,
        reply,
        command: if reply { (0, 0) } else { (header[9], header[10]) },
        error: if reply { u16::from_be_bytes([header[9], header[10]]) } else { 0 },
        data,
    })
}

pub fn write_command(out: &mut impl Write, id: u32, command: (u8, u8), data: &[u8]) -> std::io::Result<()> {
    let mut packet = Vec::with_capacity(11 + data.len());
    packet.extend_from_slice(&((11 + data.len()) as u32).to_be_bytes());
    packet.extend_from_slice(&id.to_be_bytes());
    packet.push(0);
    packet.push(command.0);
    packet.push(command.1);
    packet.extend_from_slice(data);
    out.write_all(&packet)
}

/// A place in the code: class, method and bytecode index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    pub tag: u8,
    pub class: u64,
    pub method: u64,
    pub index: u64,
}

/// A value: primitives, or an object id with its JDWP tag (`L` object, `s` string, `[`
/// array, `t` thread, `c` class object…; id 0 is null).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Void,
    Bool(bool),
    Byte(i8),
    Char(u16),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Object { tag: u8, id: u64 },
}

impl Value {
    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object { .. })
    }

    pub fn object_id(&self) -> Option<u64> {
        match self {
            Value::Object { id, .. } if *id != 0 => Some(*id),
            _ => None,
        }
    }

    /// The value as Java writes it (objects: None).
    pub fn primitive_text(&self) -> Option<String> {
        Some(match self {
            Value::Void => "void".into(),
            Value::Bool(b) => b.to_string(),
            Value::Byte(v) => v.to_string(),
            Value::Char(c) => format!("'{}'", char::from_u32(*c as u32).unwrap_or('?')),
            Value::Short(v) => v.to_string(),
            Value::Int(v) => v.to_string(),
            Value::Long(v) => v.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Double(v) => v.to_string(),
            Value::Object { id: 0, .. } => "null".into(),
            Value::Object { .. } => return None,
        })
    }

    pub fn as_f64(&self) -> Option<f64> {
        Some(match self {
            Value::Byte(v) => *v as f64,
            Value::Char(v) => *v as f64,
            Value::Short(v) => *v as f64,
            Value::Int(v) => *v as f64,
            Value::Long(v) => *v as f64,
            Value::Float(v) => *v as f64,
            Value::Double(v) => *v,
            _ => return None,
        })
    }

    pub fn as_i64(&self) -> Option<i64> {
        Some(match self {
            Value::Byte(v) => *v as i64,
            Value::Char(v) => *v as i64,
            Value::Short(v) => *v as i64,
            Value::Int(v) => *v as i64,
            Value::Long(v) => *v,
            _ => return None,
        })
    }
}

/// Builds a command's data.
pub struct Writer {
    pub buf: Vec<u8>,
    ids: IdSizes,
}

impl Writer {
    pub fn new(ids: IdSizes) -> Self {
        Self { buf: Vec::new(), ids }
    }
    pub fn u8(mut self, v: u8) -> Self {
        self.buf.push(v);
        self
    }
    pub fn i32(mut self, v: i32) -> Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(mut self, v: u64) -> Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn sized(mut self, v: u64, size: usize) -> Self {
        let bytes = v.to_be_bytes();
        self.buf.extend_from_slice(&bytes[8 - size.min(8)..]);
        self
    }
    pub fn object(self, v: u64) -> Self {
        let n = self.ids.object;
        self.sized(v, n)
    }
    pub fn reftype(self, v: u64) -> Self {
        let n = self.ids.reftype;
        self.sized(v, n)
    }
    pub fn method(self, v: u64) -> Self {
        let n = self.ids.method;
        self.sized(v, n)
    }
    pub fn field(self, v: u64) -> Self {
        let n = self.ids.field;
        self.sized(v, n)
    }
    pub fn frame(self, v: u64) -> Self {
        let n = self.ids.frame;
        self.sized(v, n)
    }
    pub fn string(mut self, s: &str) -> Self {
        self.buf.extend_from_slice(&(s.len() as u32).to_be_bytes());
        self.buf.extend_from_slice(s.as_bytes());
        self
    }
    pub fn location(self, l: &Location) -> Self {
        self.u8(l.tag).reftype(l.class).method(l.method).u64(l.index)
    }
    /// A tagged value (method arguments).
    pub fn value(self, v: &Value) -> Self {
        match *v {
            Value::Void => self.u8(b'V'),
            Value::Bool(b) => self.u8(b'Z').u8(b as u8),
            Value::Byte(x) => self.u8(b'B').u8(x as u8),
            Value::Char(x) => {
                let w = self.u8(b'C');
                let mut w = w;
                w.buf.extend_from_slice(&x.to_be_bytes());
                w
            }
            Value::Short(x) => {
                let mut w = self.u8(b'S');
                w.buf.extend_from_slice(&x.to_be_bytes());
                w
            }
            Value::Int(x) => self.u8(b'I').i32(x),
            Value::Long(x) => self.u8(b'J').u64(x as u64),
            Value::Float(x) => {
                let mut w = self.u8(b'F');
                w.buf.extend_from_slice(&x.to_bits().to_be_bytes());
                w
            }
            Value::Double(x) => self.u8(b'D').u64(x.to_bits()),
            Value::Object { tag, id } => self.u8(tag).object(id),
        }
    }
    pub fn done(self) -> Vec<u8> {
        self.buf
    }
}

/// Reads a reply's data.
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    ids: IdSizes,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], ids: IdSizes) -> Self {
        Self { data, pos: 0, ids }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos + n;
        let slice = self.data.get(self.pos..end).ok_or("JDWP: reply too short")?;
        self.pos = end;
        Ok(slice)
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    fn sized(&mut self, n: usize) -> Result<u64, String> {
        let b = self.take(n)?;
        Ok(b.iter().fold(0u64, |acc, &x| (acc << 8) | x as u64))
    }
    pub fn object(&mut self) -> Result<u64, String> {
        self.sized(self.ids.object)
    }
    pub fn reftype(&mut self) -> Result<u64, String> {
        self.sized(self.ids.reftype)
    }
    pub fn method(&mut self) -> Result<u64, String> {
        self.sized(self.ids.method)
    }
    pub fn field(&mut self) -> Result<u64, String> {
        self.sized(self.ids.field)
    }
    pub fn frame(&mut self) -> Result<u64, String> {
        self.sized(self.ids.frame)
    }
    pub fn string(&mut self) -> Result<String, String> {
        let n = self.i32()?.max(0) as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    pub fn location(&mut self) -> Result<Location, String> {
        Ok(Location { tag: self.u8()?, class: self.reftype()?, method: self.method()?, index: self.u64()? })
    }
    /// A value whose tag was given separately (array regions of primitives).
    pub fn untagged(&mut self, tag: u8) -> Result<Value, String> {
        Ok(match tag {
            b'V' => Value::Void,
            b'Z' => Value::Bool(self.u8()? != 0),
            b'B' => Value::Byte(self.u8()? as i8),
            b'C' => Value::Char(self.u16()?),
            b'S' => Value::Short(self.u16()? as i16),
            b'I' => Value::Int(self.i32()?),
            b'J' => Value::Long(self.u64()? as i64),
            b'F' => Value::Float(f32::from_bits(self.i32()? as u32)),
            b'D' => Value::Double(f64::from_bits(self.u64()?)),
            _ => Value::Object { tag, id: self.object()? },
        })
    }
    pub fn value(&mut self) -> Result<Value, String> {
        let tag = self.u8()?;
        self.untagged(tag)
    }
}

/// `int[]` from `[I`, `java.lang.String` from `Ljava/lang/String;`.
pub fn type_name(signature: &str) -> String {
    let dims = signature.chars().take_while(|c| *c == '[').count();
    let base = &signature[dims..];
    let name = match base.as_bytes().first() {
        Some(b'Z') => "boolean".to_string(),
        Some(b'B') => "byte".to_string(),
        Some(b'C') => "char".to_string(),
        Some(b'S') => "short".to_string(),
        Some(b'I') => "int".to_string(),
        Some(b'J') => "long".to_string(),
        Some(b'F') => "float".to_string(),
        Some(b'D') => "double".to_string(),
        Some(b'V') => "void".to_string(),
        Some(b'L') => base[1..].trim_end_matches(';').replace('/', "."),
        _ => base.to_string(),
    };
    format!("{name}{}", "[]".repeat(dims))
}

/// The part a person reads: `java.util.ArrayList` → `ArrayList`, `Main$Point` → `Point`.
pub fn simple_name(type_name: &str) -> &str {
    let after_dot = type_name.rsplit('.').next().unwrap_or(type_name);
    after_dot.rsplit('$').find(|p| !p.is_empty() && !p.chars().all(|c| c.is_ascii_digit())).unwrap_or(after_dot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_and_values() {
        let ids = IdSizes::default();
        let data = Writer::new(ids).object(7).i32(-1).string("ab").done();
        assert_eq!(data.len(), 8 + 4 + 4 + 2);
        let mut out = Vec::new();
        write_command(&mut out, 3, cmd::TR_FRAMES, &data).unwrap();
        let p = read_packet(&mut std::io::Cursor::new(out)).unwrap();
        assert_eq!((p.id, p.reply, p.command), (3, false, cmd::TR_FRAMES));
        let mut r = Reader::new(&p.data, ids);
        assert_eq!((r.object().unwrap(), r.i32().unwrap(), r.string().unwrap()), (7, -1, "ab".to_string()));
        let v = Writer::new(ids).value(&Value::Int(42)).value(&Value::Object { tag: b's', id: 9 }).done();
        let mut r = Reader::new(&v, ids);
        assert_eq!(r.value().unwrap(), Value::Int(42));
        assert_eq!(r.value().unwrap(), Value::Object { tag: b's', id: 9 });
    }

    #[test]
    fn names() {
        assert_eq!(type_name("[I"), "int[]");
        assert_eq!(type_name("Ljava/util/ArrayList;"), "java.util.ArrayList");
        assert_eq!(type_name("[[Ljava/lang/String;"), "java.lang.String[][]");
        assert_eq!(simple_name("java.util.ArrayList"), "ArrayList");
        assert_eq!(simple_name("demo.Main$Point"), "Point");
        assert_eq!(simple_name("Main$1"), "Main");
    }
}
