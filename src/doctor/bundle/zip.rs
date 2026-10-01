//! A minimal ZIP writer: stored (uncompressed) entries, UTF-8 names, no
//! ZIP64. Enough for a diagnostics bundle of small text files that Windows
//! Explorer, macOS Archive Utility and `unzip` open without extra tools, and
//! it keeps RPool free of an archive dependency.
use std::io::{self, Write};

/// Archives above this need ZIP64, which this writer does not produce.
const MAX_ARCHIVE_BYTES: u64 = u32::MAX as u64;

pub(crate) struct ZipWriter<W: Write> {
    out: W,
    offset: u64,
    central: Vec<u8>,
    entries: u16,
    dos_time: u16,
    dos_date: u16,
}

impl<W: Write> ZipWriter<W> {
    /// `unix_time` is stamped on every entry (DOS time, UTC, 2 s resolution).
    pub(crate) fn new(out: W, unix_time: u64) -> Self {
        let (dos_time, dos_date) = dos_datetime(unix_time);
        Self {
            out,
            offset: 0,
            central: Vec::new(),
            entries: 0,
            dos_time,
            dos_date,
        }
    }

    pub(crate) fn add(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let name_bytes = name.as_bytes();
        let fits = name_bytes.len() <= u16::MAX as usize
            && self.entries < u16::MAX
            && self.offset
                + 30
                + name_bytes.len() as u64
                + data.len() as u64
                + self.central.len() as u64
                + 46
                + name_bytes.len() as u64
                + 22
                <= MAX_ARCHIVE_BYTES;
        if !fits || name.is_empty() || name.starts_with('/') || name.contains('\\') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot add {name:?} to the diagnostics archive"),
            ));
        }
        let crc = crc32(data);
        let size = data.len() as u32;
        let offset = self.offset as u32;
        let mut local = Vec::with_capacity(30 + name_bytes.len());
        local.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        self.common_fields(&mut local, crc, size, name_bytes.len() as u16);
        local.extend_from_slice(&0u16.to_le_bytes()); // extra length
        local.extend_from_slice(name_bytes);
        self.out.write_all(&local)?;
        self.out.write_all(data)?;
        self.offset += local.len() as u64 + data.len() as u64;

        let mut central = Vec::with_capacity(46 + name_bytes.len());
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        self.common_fields(&mut central, crc, size, name_bytes.len() as u16);
        central.extend_from_slice(&0u16.to_le_bytes()); // extra length
        central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name_bytes);
        self.central.extend_from_slice(&central);
        self.entries += 1;
        Ok(())
    }

    /// Fields shared by the local and central headers, from "version needed"
    /// to the name length.
    fn common_fields(&self, out: &mut Vec<u8>, crc: u32, size: u32, name_len: u16) {
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
        out.extend_from_slice(&0u16.to_le_bytes()); // stored
        out.extend_from_slice(&self.dos_time.to_le_bytes());
        out.extend_from_slice(&self.dos_date.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes()); // compressed
        out.extend_from_slice(&size.to_le_bytes()); // uncompressed
        out.extend_from_slice(&name_len.to_le_bytes());
    }

    pub(crate) fn finish(mut self) -> io::Result<W> {
        let central_offset = self.offset as u32;
        self.out.write_all(&self.central)?;
        let mut end = Vec::with_capacity(22);
        end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes()); // this disk
        end.extend_from_slice(&0u16.to_le_bytes()); // central directory disk
        end.extend_from_slice(&self.entries.to_le_bytes());
        end.extend_from_slice(&self.entries.to_le_bytes());
        end.extend_from_slice(&(self.central.len() as u32).to_le_bytes());
        end.extend_from_slice(&central_offset.to_le_bytes());
        end.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.out.write_all(&end)?;
        self.out.flush()?;
        Ok(self.out)
    }
}

/// CRC-32 (IEEE 802.3, reflected 0xEDB88320), as ZIP requires.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// MS-DOS (time, date) of a UTC unix time; clamped to 1980..=2107.
fn dos_datetime(unix_time: u64) -> (u16, u16) {
    let days = (unix_time / 86_400) as i64;
    let seconds = unix_time % 86_400;
    let (year, month, day) = crate::monitor::history::civil_from_days(days);
    if year < 1980 {
        return (0, (1 << 5) | 1);
    }
    let year = year.min(2107) as u16;
    let time = ((seconds / 3600) as u16) << 11
        | (((seconds % 3600) / 60) as u16) << 5
        | ((seconds % 60) / 2) as u16;
    let date = (year - 1980) << 9 | (month as u16) << 5 | day as u16;
    (time, date)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Reads a stored archive back through its central directory, checking
    /// every CRC: `(name, data)` in order.
    pub(crate) fn read_stored(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
        let u32_at = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let end = bytes.len() - 22;
        assert_eq!(u32_at(end), 0x0605_4b50, "end of central directory");
        let count = u16_at(end + 10);
        let mut at = u32_at(end + 16);
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(at), 0x0201_4b50, "central header");
            let crc = u32_at(at + 16) as u32;
            let size = u32_at(at + 24);
            let name_len = u16_at(at + 28);
            let local = u32_at(at + 42);
            let name = String::from_utf8(bytes[at + 46..at + 46 + name_len].to_vec()).unwrap();
            assert_eq!(u32_at(local), 0x0403_4b50, "local header");
            let data_at = local + 30 + u16_at(local + 26) + u16_at(local + 28);
            let data = bytes[data_at..data_at + size].to_vec();
            assert_eq!(crc32(&data), crc, "crc of {name}");
            out.push((name, data));
            at += 46 + name_len;
        }
        out
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn writes_a_readable_stored_archive() {
        let mut zip = ZipWriter::new(Vec::new(), 1_727_740_800);
        zip.add("manifest.txt", b"hello").unwrap();
        zip.add("dir/한글.json", b"{}").unwrap();
        zip.add("empty.txt", b"").unwrap();
        let bytes = zip.finish().unwrap();
        let entries = read_stored(&bytes);
        assert_eq!(
            entries,
            vec![
                ("manifest.txt".into(), b"hello".to_vec()),
                ("dir/한글.json".into(), b"{}".to_vec()),
                ("empty.txt".into(), Vec::new()),
            ]
        );
    }

    #[test]
    fn rejects_absolute_and_backslash_names() {
        let mut zip = ZipWriter::new(Vec::new(), 0);
        assert!(zip.add("/etc/passwd", b"").is_err());
        assert!(zip.add("a\\b", b"").is_err());
        assert!(zip.add("", b"").is_err());
    }

    #[test]
    fn dos_datetime_of_a_known_moment() {
        // 2024-10-01 00:00:00 UTC.
        assert_eq!(dos_datetime(1_727_740_800), (0, (44 << 9) | (10 << 5) | 1));
        assert_eq!(dos_datetime(0), (0, (1 << 5) | 1));
    }
}
