use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommitId(pub [u8; 20]);

impl CommitId {
    pub fn from_hex(s: &str) -> Option<CommitId> {
        let b = s.as_bytes();
        if b.len() != 40 {
            return None;
        }
        let mut out = [0u8; 20];
        for (i, o) in out.iter_mut().enumerate() {
            let hi = (b[2 * i] as char).to_digit(16)?;
            let lo = (b[2 * i + 1] as char).to_digit(16)?;
            *o = (hi * 16 + lo) as u8;
        }
        Some(CommitId(out))
    }
    pub fn to_hex(&self) -> String {
        const H: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(40);
        for b in self.0 {
            s.push(H[(b >> 4) as usize] as char);
            s.push(H[(b & 15) as usize] as char);
        }
        s
    }
    pub fn short(&self, n: usize) -> String {
        let mut h = self.to_hex();
        h.truncate(n.min(40));
        h
    }
}

impl fmt::Display for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}
impl fmt::Debug for CommitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CommitId({})", self.short(10))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Signature {
    pub name: String,
    pub email: String,
    /// Seconds since the Unix epoch.
    pub time: i64,
    pub offset_secs: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_roundtrip() {
        let h = "4f69cdad0123456789abcdef0123456789abcdef";
        let id = CommitId::from_hex(h).unwrap();
        assert_eq!(id.to_hex(), h);
        assert_eq!(id.short(7), "4f69cda");
        assert!(CommitId::from_hex("xyz").is_none());
        assert!(CommitId::from_hex(&"g".repeat(40)).is_none());
    }
}
