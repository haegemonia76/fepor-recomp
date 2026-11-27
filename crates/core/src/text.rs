//! The game's message files (`Mess/*.m`) and their editable text form (ADR 0025).
//!
//! A `.m` file is big-endian: a 0x20-byte header (file size, size of the text area, 0, entry count, rest zero),
//! the text area (NUL-terminated strings, each padded to 4 bytes), a table of `count` pairs (text offset
//! relative to the text area, name offset) and the names (NUL-terminated, in table order, no padding).
//! Strings are raw bytes: ASCII with `$` control codes, single-byte accented letters and Shift-JIS runs,
//! so the editable form keeps every byte that is not printable ASCII as `\xHH`.

use std::fmt::Write as _;

#[derive(Debug, thiserror::Error)]
pub enum TextError {
    #[error("not a message file: {0}")]
    Format(String),
    #[error("line {line}: {msg}")]
    Edit { line: usize, msg: String },
    #[error("{0}")]
    Mismatch(String),
}

fn fmt_err(msg: impl Into<String>) -> TextError {
    TextError::Format(msg.into())
}

/// One message: its name, its position in the entry table and its text bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub index: usize,
    /// Raw bytes: some names contain Shift-JIS.
    pub name: Vec<u8>,
    pub text: Vec<u8>,
}

/// A message file with the messages in text-area order (the order they are stored in the file).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFile {
    pub messages: Vec<Message>,
}

const HEADER: usize = 0x20;

fn be32(b: &[u8], at: usize) -> Result<usize, TextError> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| fmt_err("truncated"))
}

fn cstr(b: &[u8], at: usize) -> Result<&[u8], TextError> {
    let tail = b.get(at..).ok_or_else(|| fmt_err("offset out of range"))?;
    let end = tail
        .iter()
        .position(|&c| c == 0)
        .ok_or_else(|| fmt_err("unterminated string"))?;
    Ok(&tail[..end])
}

impl MessageFile {
    /// Parse a `.m` file. Only files that [`build`](Self::build) writes back byte for byte are accepted,
    /// so nothing is lost silently.
    pub fn parse(data: &[u8]) -> Result<MessageFile, TextError> {
        if data.len() < HEADER {
            return Err(fmt_err("shorter than the header"));
        }
        let text_size = be32(data, 4)?;
        let count = be32(data, 12)?;
        let table = HEADER
            .checked_add(text_size)
            .ok_or_else(|| fmt_err("text size"))?;
        let names = count
            .checked_mul(8)
            .and_then(|t| t.checked_add(table))
            .ok_or_else(|| fmt_err("entry count"))?;
        if names > data.len() {
            return Err(fmt_err("entry table past the end of the file"));
        }
        let text = &data[HEADER..table];
        let names_area = &data[names..];
        let mut messages = Vec::with_capacity(count);
        for index in 0..count {
            let text_off = be32(data, table + 8 * index)?;
            let name_off = be32(data, table + 8 * index + 4)?;
            let name = cstr(names_area, name_off)?;
            if name.is_empty() {
                return Err(fmt_err(format!("entry {index}: empty name")));
            }
            messages.push(Message {
                index,
                name: name.to_vec(),
                text: cstr(text, text_off)?.to_vec(),
            });
        }
        // Text-area order. Entries sharing a string are not representable (none exist on the disc).
        let mut order: Vec<(usize, usize)> = (0..count)
            .map(|i| (be32(data, table + 8 * i).unwrap_or(0), i))
            .collect();
        order.sort_unstable();
        let mut sorted = Vec::with_capacity(count);
        for (_, i) in order {
            sorted.push(messages[i].clone());
        }
        let file = MessageFile { messages: sorted };
        let rebuilt = file.build()?;
        if rebuilt != data {
            return Err(fmt_err(
                "layout differs from the canonical one (shared strings, gaps or extra data)",
            ));
        }
        Ok(file)
    }

    /// Serialise: strings in the order of `messages`, table in `index` order, `index` must be a permutation of `0..len`.
    pub fn build(&self) -> Result<Vec<u8>, TextError> {
        let n = self.messages.len();
        let mut slot = vec![None; n];
        let mut text = Vec::new();
        for m in &self.messages {
            if m.index >= n || slot[m.index].is_some() {
                return Err(TextError::Mismatch(format!(
                    "entry indices must be 0..{n}, each once (bad: {} {})",
                    m.index,
                    show(&m.name)
                )));
            }
            if m.name.is_empty() || m.name.contains(&0) {
                return Err(TextError::Mismatch(format!("entry {}: bad name", m.index)));
            }
            if m.text.contains(&0) {
                return Err(TextError::Mismatch(format!(
                    "{}: NUL in text",
                    show(&m.name)
                )));
            }
            slot[m.index] = Some((text.len(), m));
            text.extend_from_slice(&m.text);
            text.push(0);
            while text.len() % 4 != 0 {
                text.push(0);
            }
        }
        let mut table = Vec::with_capacity(8 * n);
        let mut names = Vec::new();
        for s in slot {
            let (text_off, m) = s.expect("permutation checked above");
            table.extend_from_slice(&(text_off as u32).to_be_bytes());
            table.extend_from_slice(&(names.len() as u32).to_be_bytes());
            names.extend_from_slice(&m.name);
            names.push(0);
        }
        let total = HEADER + text.len() + table.len() + names.len();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&(text.len() as u32).to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&(n as u32).to_be_bytes());
        out.resize(HEADER, 0);
        out.extend_from_slice(&text);
        out.extend_from_slice(&table);
        out.extend_from_slice(&names);
        Ok(out)
    }

    /// The editable form (see ADR 0025).
    pub fn to_edit(&self, file_name: &str) -> String {
        let mut s = format!("# fepor-recomp text v1: {file_name}\n");
        for m in &self.messages {
            let _ = write!(s, "## {} ", m.index);
            escape(&m.name, true, &mut s);
            s.push('\n');
            for line in m.text.split(|&c| c == b'\n') {
                if line.first() == Some(&b'#') {
                    s.push('\\');
                }
                escape(line, false, &mut s);
                s.push('\n');
            }
            s.push('\n');
        }
        s
    }

    /// Parse the editable form back.
    pub fn from_edit(src: &str) -> Result<MessageFile, TextError> {
        let lines: Vec<&str> = src
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l))
            .collect();
        // A file ending in LF splits into a trailing empty piece that is no line.
        let lines = match lines.split_last() {
            Some((&"", rest)) => rest.to_vec(),
            _ => lines,
        };
        let mut messages: Vec<Message> = Vec::new();
        // (line number of header, index, name, body lines)
        let mut cur: Option<(usize, usize, Vec<u8>, Vec<&str>)> = None;
        let finish = |cur: &mut Option<(usize, usize, Vec<u8>, Vec<&str>)>,
                      messages: &mut Vec<Message>|
         -> Result<(), TextError> {
            if let Some((at, index, name, mut body)) = cur.take() {
                if body.pop() != Some("") {
                    return Err(TextError::Edit {
                        line: at,
                        msg: format!("{}: a blank line must end the text", show(&name)),
                    });
                }
                let mut text = Vec::new();
                for (i, l) in body.iter().enumerate() {
                    if i > 0 {
                        text.push(b'\n');
                    }
                    unescape(l, at + 1 + i, &mut text)?;
                }
                messages.push(Message { index, name, text });
            }
            Ok(())
        };
        for (i, line) in lines.iter().enumerate() {
            let no = i + 1;
            if let Some(h) = line.strip_prefix("## ") {
                finish(&mut cur, &mut messages)?;
                let mut it = h.split(' ');
                let (Some(idx), Some(name), None) = (it.next(), it.next(), it.next()) else {
                    return Err(TextError::Edit {
                        line: no,
                        msg: "expected `## <index> <NAME>`".into(),
                    });
                };
                let index = idx.parse().map_err(|_| TextError::Edit {
                    line: no,
                    msg: format!("bad index {idx:?}"),
                })?;
                let mut name_bytes = Vec::new();
                unescape(name, no, &mut name_bytes)?;
                cur = Some((no, index, name_bytes, Vec::new()));
            } else if let Some((_, _, _, body)) = cur.as_mut() {
                body.push(line);
            } else if !line.starts_with('#') && !line.is_empty() {
                return Err(TextError::Edit {
                    line: no,
                    msg: "text before the first `## ` header".into(),
                });
            }
        }
        finish(&mut cur, &mut messages)?;
        Ok(MessageFile { messages })
    }

    /// Check that `self` (an edited file) has the same entries (index and name) as `original`.
    pub fn check_against(&self, original: &MessageFile) -> Result<(), TextError> {
        if self.messages.len() != original.messages.len() {
            return Err(TextError::Mismatch(format!(
                "{} entries, the game's file has {}",
                self.messages.len(),
                original.messages.len()
            )));
        }
        let mut theirs: Vec<(usize, &[u8])> = original
            .messages
            .iter()
            .map(|m| (m.index, &m.name[..]))
            .collect();
        let mut ours: Vec<(usize, &[u8])> = self
            .messages
            .iter()
            .map(|m| (m.index, &m.name[..]))
            .collect();
        theirs.sort_unstable();
        ours.sort_unstable();
        match theirs.iter().zip(&ours).find(|(a, b)| a != b) {
            Some((a, b)) => Err(TextError::Mismatch(format!(
                "entry {} {} does not match the game's {} {}",
                b.0,
                show(b.1),
                a.0,
                show(a.1)
            ))),
            None => Ok(()),
        }
    }
}

/// Printable ASCII stays, everything else is `\xHH`, `\` doubles; names also escape the space that separates header fields.
fn escape(bytes: &[u8], name: bool, s: &mut String) {
    for &c in bytes {
        match c {
            b'\\' => s.push_str("\\\\"),
            b' ' if name => s.push_str("\\x20"),
            0x20..=0x7e => s.push(c as char),
            _ => {
                let _ = write!(s, "\\x{c:02X}");
            }
        }
    }
}

fn show(bytes: &[u8]) -> String {
    let mut s = String::new();
    escape(bytes, true, &mut s);
    s
}

fn unescape(line: &str, no: usize, out: &mut Vec<u8>) -> Result<(), TextError> {
    let err = |msg: String| TextError::Edit { line: no, msg };
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                match b.get(i + 1) {
                    Some(b'\\') => out.push(b'\\'),
                    Some(b'#') => out.push(b'#'),
                    Some(b'x') => {
                        let hex = line
                            .get(i + 2..i + 4)
                            .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()));
                        let Some(hex) = hex else {
                            return Err(err("`\\x` needs two hex digits".into()));
                        };
                        out.push(u8::from_str_radix(hex, 16).expect("checked"));
                        i += 2;
                    }
                    _ => return Err(err("unknown escape (use `\\\\`, `\\#` or `\\xHH`)".into())),
                }
                i += 2;
            }
            c @ 0x20..=0x7e => {
                out.push(c);
                i += 1;
            }
            _ => {
                return Err(err(
                    "only printable ASCII is allowed; write other bytes as `\\xHH`".into(),
                ))
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic file in the game's layout: texts stored in a different order than the table.
    fn sample() -> Vec<u8> {
        let texts: [&[u8]; 3] = [b"Hello\n$KWorld", b"", b"a\\b #x\xdc\x8f\xe3"];
        let names: [&[u8]; 3] = [b"MESS_B", b"MESS_A", b"MG_\x82\xcd x"];
        // text order: 0, 1, 2; table order: 1 (A), 0 (B), 2 (C)
        let mut text = Vec::new();
        let mut offs = [0usize; 3];
        for (i, t) in texts.iter().enumerate() {
            offs[i] = text.len();
            text.extend_from_slice(t);
            text.push(0);
            while text.len() % 4 != 0 {
                text.push(0);
            }
        }
        let table_order = [1usize, 0, 2];
        let mut names_blob = Vec::new();
        let mut table = Vec::new();
        for &i in &table_order {
            table.extend_from_slice(&(offs[i] as u32).to_be_bytes());
            table.extend_from_slice(&(names_blob.len() as u32).to_be_bytes());
            names_blob.extend_from_slice(names[i]);
            names_blob.push(0);
        }
        let total = 0x20 + text.len() + table.len() + names_blob.len();
        let mut out = Vec::new();
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&(text.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&3u32.to_be_bytes());
        out.resize(0x20, 0);
        out.extend(text);
        out.extend(table);
        out.extend(names_blob);
        out
    }

    #[test]
    fn binary_round_trip() {
        let data = sample();
        let f = MessageFile::parse(&data).unwrap();
        assert_eq!(f.messages[0].name, b"MESS_B");
        assert_eq!(f.messages[0].index, 1);
        assert_eq!(f.messages[0].text, b"Hello\n$KWorld");
        assert_eq!(f.build().unwrap(), data);
    }

    #[test]
    fn edit_round_trip() {
        let data = sample();
        let f = MessageFile::parse(&data).unwrap();
        let edit = f.to_edit("Mess/x.m");
        assert!(edit.contains("## 1 MESS_B\nHello\n$KWorld\n\n"));
        assert!(edit.contains("## 2 MG_\\x82\\xCD\\x20x\n"));
        assert!(edit.contains("a\\\\b #x\\xDC\\x8F\\xE3"));
        let back = MessageFile::from_edit(&edit).unwrap();
        assert_eq!(back, f);
        assert_eq!(back.build().unwrap(), data);
        // CRLF files (edited on Windows) read the same.
        let crlf = edit.replace('\n', "\r\n");
        assert_eq!(MessageFile::from_edit(&crlf).unwrap(), f);
    }

    #[test]
    fn hash_at_line_start_and_trailing_newline() {
        let f = MessageFile {
            messages: vec![Message {
                index: 0,
                name: b"N".to_vec(),
                text: b"#a\n\n#b\n".to_vec(),
            }],
        };
        let back = MessageFile::from_edit(&f.to_edit("x")).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn longer_translation_rebuilds_and_reparses() {
        let mut f = MessageFile::parse(&sample()).unwrap();
        f.messages[1].text = b"much longer text than before, easily".to_vec();
        let built = f.build().unwrap();
        let again = MessageFile::parse(&built).unwrap();
        assert_eq!(again, f);
        again
            .check_against(&MessageFile::parse(&sample()).unwrap())
            .unwrap();
    }

    #[test]
    fn rejects_bad_input() {
        let f = MessageFile::parse(&sample()).unwrap();
        let edit = f.to_edit("x");
        assert!(MessageFile::from_edit(&edit.replace("World", "Wörld")).is_err());
        assert!(MessageFile::from_edit(&edit.replace("\\xDC", "\\xZZ")).is_err());
        assert!(MessageFile::from_edit(&edit.replace("a\\\\b", "a\\b")).is_err());
        assert!(MessageFile::from_edit(edit.trim_end_matches('\n')).is_err());
        let mut renamed = f.clone();
        renamed.messages[0].name = b"MESS_Z".to_vec();
        assert!(renamed.check_against(&f).is_err());
        let mut dup = f.clone();
        dup.messages[0].index = 0;
        assert!(dup.build().is_err());
        let mut short = f.clone();
        short.messages.pop();
        assert!(short.check_against(&f).is_err());
        // not canonical: trailing junk
        let mut junk = sample();
        junk.push(0);
        assert!(MessageFile::parse(&junk).is_err());
        assert!(MessageFile::parse(&[0; 8]).is_err());
    }
}
