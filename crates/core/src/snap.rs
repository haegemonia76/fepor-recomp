//! Byte-level writer and reader for save states (ADR 0035). Little endian, no field names: every
//! component writes its fields in a fixed order and reads them back in the same order, inside a section
//! of the container that carries a tag and a length, so that a reader can fail cleanly on a state from
//! another build instead of misreading it.

/// Appends values to a byte vector.
#[derive(Default)]
pub struct W {
    pub buf: Vec<u8>,
}

impl W {
    pub fn new() -> W {
        W::default()
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    pub fn f64(&mut self, v: f64) {
        self.u64(v.to_bits());
    }
    /// A length-prefixed byte string.
    pub fn bytes(&mut self, v: &[u8]) {
        self.u64(v.len() as u64);
        self.buf.extend_from_slice(v);
    }
    pub fn u32s(&mut self, v: &[u32]) {
        self.u64(v.len() as u64);
        for &x in v {
            self.u32(x);
        }
    }
    pub fn opt_u32(&mut self, v: Option<u32>) {
        self.bool(v.is_some());
        self.u32(v.unwrap_or(0));
    }
    /// A section: tag, length, body (written by `f`).
    pub fn section(&mut self, tag: &[u8; 4], f: impl FnOnce(&mut W)) {
        self.buf.extend_from_slice(tag);
        let at = self.buf.len();
        self.u64(0);
        f(self);
        let len = (self.buf.len() - at - 8) as u64;
        self.buf[at..at + 8].copy_from_slice(&len.to_le_bytes());
    }
}

/// Reads what `W` wrote. A read past the end sets `failed` and returns zeros; check it once at the end.
pub struct R<'a> {
    data: &'a [u8],
    pos: usize,
    pub failed: bool,
}

impl<'a> R<'a> {
    pub fn new(data: &'a [u8]) -> R<'a> {
        R {
            data,
            pos: 0,
            failed: false,
        }
    }
    fn take(&mut self, n: usize) -> &'a [u8] {
        if self.failed || self.pos + n > self.data.len() {
            self.failed = true;
            return &[];
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        s
    }
    pub fn u8(&mut self) -> u8 {
        self.take(1).first().copied().unwrap_or(0)
    }
    pub fn bool(&mut self) -> bool {
        self.u8() != 0
    }
    pub fn u16(&mut self) -> u16 {
        self.take(2).try_into().map_or(0, u16::from_le_bytes)
    }
    pub fn u32(&mut self) -> u32 {
        self.take(4).try_into().map_or(0, u32::from_le_bytes)
    }
    pub fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    pub fn u64(&mut self) -> u64 {
        self.take(8).try_into().map_or(0, u64::from_le_bytes)
    }
    pub fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }
    pub fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }
    pub fn bytes(&mut self) -> &'a [u8] {
        let n = self.u64() as usize;
        self.take(n)
    }
    /// A byte string that must have exactly `n` bytes, copied into `out`.
    pub fn bytes_into(&mut self, out: &mut [u8]) {
        let b = self.bytes();
        if b.len() == out.len() {
            out.copy_from_slice(b);
        } else {
            self.failed = true;
        }
    }
    pub fn array8(&mut self) -> [u8; 8] {
        self.take(8).try_into().unwrap_or([0; 8])
    }
    pub fn u32s(&mut self) -> Vec<u32> {
        let n = self.u64() as usize;
        if n > self.data.len() {
            self.failed = true;
            return Vec::new();
        }
        (0..n).map(|_| self.u32()).collect()
    }
    pub fn u32s_into(&mut self, out: &mut [u32]) {
        let v = self.u32s();
        if v.len() == out.len() {
            out.copy_from_slice(&v);
        } else {
            self.failed = true;
        }
    }
    pub fn opt_u32(&mut self) -> Option<u32> {
        let some = self.bool();
        let v = self.u32();
        some.then_some(v)
    }
    /// The body of the next section if it has `tag`.
    pub fn section(&mut self, tag: &[u8; 4]) -> R<'a> {
        let t = self.take(4);
        let n = self.u64() as usize;
        let body = self.take(n);
        if t != tag {
            self.failed = true;
        }
        R {
            data: body,
            pos: 0,
            failed: self.failed,
        }
    }
    pub fn at_end(&self) -> bool {
        self.pos == self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut w = W::new();
        w.section(b"TEST", |w| {
            w.u32(7);
            w.bytes(b"abc");
            w.opt_u32(Some(5));
            w.f64(1.5);
        });
        let mut r = R::new(&w.buf);
        let mut s = r.section(b"TEST");
        assert_eq!(s.u32(), 7);
        assert_eq!(s.bytes(), b"abc");
        assert_eq!(s.opt_u32(), Some(5));
        assert_eq!(s.f64(), 1.5);
        assert!(s.at_end() && !s.failed && !r.failed);
        let mut bad = R::new(&w.buf[..6]);
        bad.section(b"TEST");
        assert!(bad.failed);
    }
}
