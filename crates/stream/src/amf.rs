//! AMF0, the value encoding RTMP commands and stream metadata use.

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq)]
pub enum Amf {
    Number(f64),
    Bool(bool),
    String(String),
    Object(Vec<(String, Amf)>),
    Null,
    Undefined,
    /// An associative array (`onMetaData` uses one).
    EcmaArray(Vec<(String, Amf)>),
    StrictArray(Vec<Amf>),
}

impl Amf {
    pub fn str(s: &str) -> Self {
        Self::String(s.to_owned())
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// A property of an object or associative array.
    pub fn get(&self, key: &str) -> Option<&Amf> {
        match self {
            Self::Object(p) | Self::EcmaArray(p) => p.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
}

pub fn encode(values: &[Amf]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in values {
        write(v, &mut out);
    }
    out
}

fn write_key(key: &str, out: &mut Vec<u8>) {
    out.extend((key.len() as u16).to_be_bytes());
    out.extend(key.as_bytes());
}

fn write_props(props: &[(String, Amf)], out: &mut Vec<u8>) {
    for (k, v) in props {
        write_key(k, out);
        write(v, out);
    }
    out.extend([0, 0, 9]);
}

fn write(v: &Amf, out: &mut Vec<u8>) {
    match v {
        Amf::Number(n) => {
            out.push(0);
            out.extend(n.to_be_bytes());
        }
        Amf::Bool(b) => out.extend([1, u8::from(*b)]),
        Amf::String(s) if s.len() <= usize::from(u16::MAX) => {
            out.push(2);
            write_key(s, out);
        }
        Amf::String(s) => {
            out.push(0x0C);
            out.extend((s.len() as u32).to_be_bytes());
            out.extend(s.as_bytes());
        }
        Amf::Object(p) => {
            out.push(3);
            write_props(p, out);
        }
        Amf::Null => out.push(5),
        Amf::Undefined => out.push(6),
        Amf::EcmaArray(p) => {
            out.push(8);
            out.extend((p.len() as u32).to_be_bytes());
            write_props(p, out);
        }
        Amf::StrictArray(items) => {
            out.push(0x0A);
            out.extend((items.len() as u32).to_be_bytes());
            for i in items {
                write(i, out);
            }
        }
    }
}

/// Every value in `data`.
pub fn decode(mut data: &[u8]) -> Result<Vec<Amf>> {
    let mut values = Vec::new();
    while !data.is_empty() {
        values.push(read(&mut data, 0)?);
    }
    Ok(values)
}

fn take<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if data.len() < n {
        bail!("AMF data ends early");
    }
    let (head, rest) = data.split_at(n);
    *data = rest;
    Ok(head)
}

fn read_u16(data: &mut &[u8]) -> Result<usize> {
    let b = take(data, 2)?;
    Ok(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

fn read_u32(data: &mut &[u8]) -> Result<usize> {
    let b = take(data, 4)?;
    Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

fn read_string(data: &mut &[u8], len: usize) -> Result<String> {
    Ok(String::from_utf8_lossy(take(data, len)?).into_owned())
}

fn read_props(data: &mut &[u8], depth: usize) -> Result<Vec<(String, Amf)>> {
    let mut props = Vec::new();
    loop {
        let len = read_u16(data)?;
        if len == 0 && data.first() == Some(&9) {
            take(data, 1)?;
            return Ok(props);
        }
        let key = read_string(data, len)?;
        props.push((key, read(data, depth + 1)?));
    }
}

fn read(data: &mut &[u8], depth: usize) -> Result<Amf> {
    if depth > 32 {
        bail!("AMF data nests too deep");
    }
    let marker = take(data, 1).context("AMF value")?[0];
    Ok(match marker {
        0 => {
            let b = take(data, 8)?;
            Amf::Number(f64::from_be_bytes(b.try_into().expect("8 bytes")))
        }
        1 => Amf::Bool(take(data, 1)?[0] != 0),
        2 => {
            let len = read_u16(data)?;
            Amf::String(read_string(data, len)?)
        }
        3 => Amf::Object(read_props(data, depth)?),
        5 => Amf::Null,
        6 => Amf::Undefined,
        8 => {
            read_u32(data)?;
            Amf::EcmaArray(read_props(data, depth)?)
        }
        0x0A => {
            let n = read_u32(data)?;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push(read(data, depth + 1)?);
            }
            Amf::StrictArray(items)
        }
        // A date: milliseconds and a time zone nobody uses.
        0x0B => {
            let b = take(data, 10)?;
            Amf::Number(f64::from_be_bytes(b[..8].try_into().expect("8 bytes")))
        }
        0x0C => {
            let len = read_u32(data)?;
            Amf::String(read_string(data, len)?)
        }
        other => bail!("unsupported AMF0 type {other:#x}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_survive_a_round_trip() {
        let values = vec![
            Amf::str("connect"),
            Amf::Number(1.0),
            Amf::Object(vec![
                ("app".into(), Amf::str("live2")),
                ("fpad".into(), Amf::Bool(false)),
                (
                    "nested".into(),
                    Amf::Object(vec![("n".into(), Amf::Number(-2.5))]),
                ),
            ]),
            Amf::Null,
            Amf::EcmaArray(vec![("width".into(), Amf::Number(1920.0))]),
            Amf::StrictArray(vec![Amf::Undefined, Amf::str("x")]),
        ];
        let bytes = encode(&values);
        assert_eq!(decode(&bytes).unwrap(), values);
        assert_eq!(values[2].get("app").and_then(Amf::as_str), Some("live2"));
    }

    #[test]
    fn known_bytes() {
        assert_eq!(encode(&[Amf::str("hi"), Amf::Null]), [2, 0, 2, b'h', b'i', 5]);
        assert!(decode(&[2, 0, 5, b'a']).is_err());
    }
}
