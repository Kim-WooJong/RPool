//! Streaming file content format of rclone crypt:
//! `RCLONE\0\0` + 24-byte nonce, then XSalsa20-Poly1305 secretbox blocks of up to
//! 64 KiB plaintext, each sealed with the nonce incremented once per block.
use super::cipher::Cipher;
use anyhow::{anyhow, bail, Result};
use crypto_secretbox::aead::AeadInPlace;
use crypto_secretbox::{KeyInit, Tag, XSalsa20Poly1305};
use std::io::{self, Read};

pub(crate) const MAGIC: &[u8; 8] = b"RCLONE\0\0";
pub(crate) const NONCE_SIZE: usize = 24;
pub(crate) const HEADER_SIZE: u64 = 8 + NONCE_SIZE as u64;
pub(crate) const BLOCK_DATA: u64 = 64 * 1024;
pub(crate) const BLOCK_TAG: u64 = 16;
pub(crate) const BLOCK_SIZE: u64 = BLOCK_DATA + BLOCK_TAG;

/// 192-bit little-endian counter nonce (`nonce` in rclone's cipher.go).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Nonce(pub(super) [u8; NONCE_SIZE]);

impl Nonce {
    pub(crate) fn random() -> Result<Self> {
        let mut bytes = [0u8; NONCE_SIZE];
        getrandom::fill(&mut bytes).map_err(|_| anyhow!("OS random generator failed"))?;
        Ok(Self(bytes))
    }

    fn carry(&mut self, from: usize) {
        for byte in &mut self.0[from..] {
            *byte = byte.wrapping_add(1);
            if *byte != 0 {
                break;
            }
        }
    }

    pub(crate) fn increment(&mut self) {
        self.carry(0);
    }

    /// Adds `x` to the low 64 bits, carrying into the rest (rclone `nonce.add`).
    pub(crate) fn add(&mut self, mut x: u64) {
        let mut carry = 0u16;
        for byte in &mut self.0[..8] {
            carry += u16::from(*byte) + (x & 0xff) as u16;
            x >>= 8;
            *byte = carry as u8;
            carry >>= 8;
        }
        if carry != 0 {
            self.carry(8);
        }
    }
}

/// rclone `EncryptedSize`.
pub(crate) fn encrypted_size(plain: u64) -> u64 {
    let blocks = plain / BLOCK_DATA;
    let residue = plain % BLOCK_DATA;
    let mut size = HEADER_SIZE + blocks * BLOCK_SIZE;
    if residue != 0 {
        size += BLOCK_TAG + residue;
    }
    size
}

/// rclone `DecryptedSize`.
pub(crate) fn decrypted_size(encrypted: u64) -> Result<u64> {
    let body = encrypted
        .checked_sub(HEADER_SIZE)
        .ok_or_else(|| anyhow!("encrypted file too short"))?;
    let blocks = body / BLOCK_SIZE;
    let residue = body % BLOCK_SIZE;
    if residue != 0 && residue <= BLOCK_TAG {
        bail!("encrypted file has a bad block header");
    }
    Ok(blocks * BLOCK_DATA + residue.saturating_sub(BLOCK_TAG))
}

/// Where a ranged read starting at plaintext `offset` begins in the stored object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RangeStart {
    /// Byte offset in the encrypted object to start reading from (after the header).
    pub(crate) encrypted_offset: u64,
    /// Plaintext bytes of the first block to drop.
    pub(crate) discard: u64,
    pub(crate) block: u64,
}

pub(crate) fn range_start(offset: u64) -> RangeStart {
    let block = offset / BLOCK_DATA;
    RangeStart {
        encrypted_offset: HEADER_SIZE + block * BLOCK_SIZE,
        discard: offset % BLOCK_DATA,
        block,
    }
}

/// Parses a 32-byte object header, returning the initial nonce.
pub(crate) fn parse_header(header: &[u8]) -> Result<Nonce> {
    if header.len() < HEADER_SIZE as usize {
        bail!("encrypted file too short");
    }
    if &header[..MAGIC.len()] != MAGIC {
        bail!("not an encrypted file - bad magic string");
    }
    let mut nonce = [0u8; NONCE_SIZE];
    nonce.copy_from_slice(&header[MAGIC.len()..HEADER_SIZE as usize]);
    Ok(Nonce(nonce))
}

fn read_fill(source: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match source.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

impl Cipher {
    fn secretbox(&self) -> XSalsa20Poly1305 {
        XSalsa20Poly1305::new(self.data_key.as_ref().into())
    }

    /// Encrypting reader with a fresh random nonce.
    pub(crate) fn encrypter<R: Read>(&self, source: R) -> Result<Encrypter<R>> {
        Ok(self.encrypter_with_nonce(source, Nonce::random()?))
    }

    pub(crate) fn encrypter_with_nonce<R: Read>(&self, source: R, nonce: Nonce) -> Encrypter<R> {
        let mut pending = Vec::with_capacity(BLOCK_SIZE as usize);
        pending.extend_from_slice(MAGIC);
        pending.extend_from_slice(&nonce.0);
        Encrypter {
            source,
            secretbox: self.secretbox(),
            nonce,
            pending,
            position: 0,
            finished: false,
        }
    }

    /// Decrypting reader over a whole object (header included).
    pub(crate) fn decrypter<R: Read>(&self, mut source: R) -> Result<Decrypter<R>> {
        let mut header = [0u8; HEADER_SIZE as usize];
        let n = read_fill(&mut source, &mut header)?;
        let nonce = parse_header(&header[..n])?;
        Ok(self.decrypter_from(source, nonce, 0))
    }

    /// Decrypting reader for a ranged read. `source` must start at
    /// `start.encrypted_offset` of the object whose header yielded `nonce`.
    pub(crate) fn decrypter_at<R: Read>(
        &self,
        source: R,
        mut nonce: Nonce,
        start: RangeStart,
    ) -> Decrypter<R> {
        nonce.add(start.block);
        self.decrypter_from(source, nonce, start.discard as usize)
    }

    fn decrypter_from<R: Read>(&self, source: R, nonce: Nonce, discard: usize) -> Decrypter<R> {
        Decrypter {
            source,
            secretbox: self.secretbox(),
            nonce,
            block: Vec::with_capacity(BLOCK_SIZE as usize),
            position: 0,
            discard,
            finished: false,
        }
    }

    /// Opens one sealed block (tag + ciphertext) with `nonce`.
    pub(crate) fn open_block(&self, nonce: &Nonce, sealed: &[u8]) -> Result<Vec<u8>> {
        if sealed.len() <= BLOCK_TAG as usize || sealed.len() > BLOCK_SIZE as usize {
            bail!("encrypted file has a bad block header");
        }
        let tag = Tag::clone_from_slice(&sealed[..BLOCK_TAG as usize]);
        let mut plain = sealed[BLOCK_TAG as usize..].to_vec();
        self.secretbox()
            .decrypt_in_place_detached(&nonce.0.into(), b"", &mut plain, &tag)
            .map_err(|_| anyhow!("failed to authenticate decrypted block - bad password?"))?;
        Ok(plain)
    }

    /// Whole-buffer helpers.
    pub(crate) fn encrypt_bytes(&self, plain: &[u8], nonce: Nonce) -> Vec<u8> {
        let mut out = Vec::with_capacity(encrypted_size(plain.len() as u64) as usize);
        self.encrypter_with_nonce(plain, nonce)
            .read_to_end(&mut out)
            .expect("in-memory encryption cannot fail");
        out
    }

    pub(crate) fn decrypt_bytes(&self, encrypted: &[u8]) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.decrypter(encrypted)?.read_to_end(&mut out)?;
        Ok(out)
    }
}

pub(crate) struct Encrypter<R> {
    source: R,
    secretbox: XSalsa20Poly1305,
    nonce: Nonce,
    pending: Vec<u8>,
    position: usize,
    finished: bool,
}

impl<R: Read> Read for Encrypter<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.position == self.pending.len() {
            if self.finished {
                return Ok(0);
            }
            let mut plain = vec![0u8; BLOCK_DATA as usize];
            let n = read_fill(&mut self.source, &mut plain)?;
            if n == 0 {
                self.finished = true;
                return Ok(0);
            }
            plain.truncate(n);
            let tag = self
                .secretbox
                .encrypt_in_place_detached(&self.nonce.0.into(), b"", &mut plain)
                .map_err(|_| invalid("crypt block encryption failed"))?;
            self.nonce.increment();
            self.pending.clear();
            self.pending.extend_from_slice(&tag);
            self.pending.extend_from_slice(&plain);
            self.position = 0;
            if n < BLOCK_DATA as usize {
                self.finished = true;
            }
        }
        let n = out.len().min(self.pending.len() - self.position);
        out[..n].copy_from_slice(&self.pending[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}

pub(crate) struct Decrypter<R> {
    source: R,
    secretbox: XSalsa20Poly1305,
    nonce: Nonce,
    block: Vec<u8>,
    position: usize,
    discard: usize,
    finished: bool,
}

impl<R: Read> Read for Decrypter<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.position == self.block.len() {
            if self.finished {
                return Ok(0);
            }
            let mut sealed = vec![0u8; BLOCK_SIZE as usize];
            let n = read_fill(&mut self.source, &mut sealed)?;
            if n == 0 {
                self.finished = true;
                return Ok(0);
            }
            if n <= BLOCK_TAG as usize {
                return Err(invalid("encrypted file has a bad block header"));
            }
            sealed.truncate(n);
            let tag = Tag::clone_from_slice(&sealed[..BLOCK_TAG as usize]);
            let mut plain = sealed.split_off(BLOCK_TAG as usize);
            self.secretbox
                .decrypt_in_place_detached(&self.nonce.0.into(), b"", &mut plain, &tag)
                .map_err(|_| invalid("failed to authenticate decrypted block - bad password?"))?;
            self.nonce.increment();
            if n < BLOCK_SIZE as usize {
                self.finished = true;
            }
            self.block = plain;
            self.position = self.discard.min(self.block.len());
            self.discard = 0;
        }
        let n = out.len().min(self.block.len() - self.position);
        out[..n].copy_from_slice(&self.block[self.position..self.position + n]);
        self.position += n;
        Ok(n)
    }
}
