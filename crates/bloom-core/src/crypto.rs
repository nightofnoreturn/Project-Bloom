use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use aes::Aes128;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use crc32fast::Hasher as Crc32Hasher;
use md5::{Digest, Md5};
use rayon::prelude::*;

use crate::error::{BloomError, Result};

const AUTH_PARAM: &[u8] = include_bytes!("../assets/auth_param.dat");

const SHIFT_INDICES: [usize; 16] = [0, 5, 10, 15, 4, 9, 14, 3, 8, 13, 2, 7, 12, 1, 6, 11];
const LEGACY_KEY_1: &[u8] = b"vicopx7dqu06emacgpnpy8j8zwhduwlh";
const LEGACY_KEY_2: &[u8] = b"9u7qab84rpc16gvk";

const VERIFY_BUF_SIZE: usize = 1024 * 1024;
const DECRYPT_CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4 MiB, multiple of 16 bytes
const PARALLEL_SUBCHUNK_SIZE: usize = 64 * 1024; // 64 KiB, multiple of 16 bytes

#[derive(Debug, Clone, Copy)]
struct AuthHeader {
    block1_size: usize,
    block2_size: usize,
    block3_size: usize,
}

fn parse_auth_header(data: &[u8]) -> Result<AuthHeader> {
    if data.len() < 56 {
        return Err(BloomError::Crypto("auth_param.dat header truncated".into()));
    }
    let read_i32 = |idx: usize| -> Result<usize> {
        let start = idx * 4;
        let bytes: [u8; 4] = data[start..start + 4]
            .try_into()
            .map_err(|_| BloomError::Crypto("invalid header slice".into()))?;
        let val = i32::from_le_bytes(bytes);
        usize::try_from(val)
            .map_err(|_| BloomError::Crypto("negative header size in auth_param.dat".into()))
    };

    Ok(AuthHeader {
        block1_size: read_i32(3)?,
        block2_size: read_i32(5)?,
        block3_size: read_i32(11)?,
    })
}

/// Evaluate Samsung's white-box transform (`auth_param.dat`) on a 16-byte nonce block.
pub fn authenticate_block(in_block: &[u8; 16]) -> Result<[u8; 16]> {
    let header = parse_auth_header(AUTH_PARAM)?;
    let stream = AUTH_PARAM
        .get(56..)
        .ok_or_else(|| BloomError::Crypto("auth_param.dat missing payload".into()))?;

    let stream_byte = |pos: usize| -> Result<u8> {
        stream
            .get(pos)
            .copied()
            .ok_or_else(|| BloomError::Crypto(format!("auth_param.dat out-of-bounds read at {pos}")))
    };
    let stream_slice = |pos: usize, len: usize| -> Result<&[u8]> {
        stream
            .get(pos..pos + len)
            .ok_or_else(|| BloomError::Crypto(format!("auth_param.dat out-of-bounds slice at {pos}..{}", pos + len)))
    };

    let mut temp_block = [0usize; 320];
    for (dst, &src) in temp_block[..16].iter_mut().zip(in_block.iter()) {
        *dst = src as usize;
    }

    let mut out_block = [0u8; 16];
    let mut v15 = [0usize; 64];
    let base_final = header.block1_size;
    const FINAL_SRC_START: usize = 288; // 9 * 32

    for j in 0..9 {
        let src_start = j * 32;
        let next_src_start = (j + 1) * 32;
        let blk_id_base = j * 16;
        let src_mid = src_start + 16;

        for idx in 0..16 {
            temp_block[src_start + 16 + idx] = temp_block[src_start + SHIFT_INDICES[idx]];
        }

        for i in 0..4 {
            let i4 = i << 2;
            let i16 = i << 4;
            let blk_id_row = blk_id_base + i4;
            let base262 =
                base_final + header.block2_size + header.block3_size + (6144 * (i + (j << 2)));

            for k in 0..4 {
                let idx_val = temp_block[src_mid + i4 + k];
                let blk_id = blk_id_row + k;
                let base257 = (blk_id << 12) + (idx_val << 4);
                let sel_offset = base_final + header.block2_size + (blk_id << 5);
                let src16 = stream_slice(base257, 16)?;
                let selector = stream_slice(sel_offset, 32)?;
                let out_start = i16 + (k << 2);

                for out_idx in 0..4 {
                    let mut acc = 0usize;
                    let sel_base = out_idx << 3;
                    for bit_idx in 0..8 {
                        let sel_byte = selector[sel_base + bit_idx] as usize;
                        let src_idx = (sel_byte >> 3) & 0x1F;
                        let bit_pos = 7 - (sel_byte & 0x7);
                        let src_byte = if src_idx < 16 { src16[src_idx] as usize } else { 0 };
                        acc |= ((src_byte >> bit_pos) & 1) << (7 - bit_idx);
                    }
                    v15[out_start + out_idx] = acc & 0xFF;
                }
            }

            for k2 in 0..4 {
                let a1 = v15[i16 + k2];
                let a2 = v15[i16 + k2 + 4];
                let a3 = v15[i16 + k2 + 8];
                let a4 = v15[i16 + k2 + 12];

                let tbl_base = base262 + 1536 * k2;
                let tbl = stream_slice(tbl_base, 1536)?;

                let hi1 = ((a1 & 0xF0) | (a2 >> 4)) & 0xFF;
                let lo1 = (((a1 & 0x0F) << 4) | (a2 & 0x0F)) & 0xFF;
                let v6 = ((16 * (tbl[hi1] as usize)) ^ (tbl[256 + lo1] as usize)) & 0xFF;

                let hi2 = ((a3 & 0xF0) | (a4 >> 4)) & 0xFF;
                let lo2 = (((a3 & 0x0F) << 4) | (a4 & 0x0F)) & 0xFF;
                let v7 = ((16 * (tbl[512 + hi2] as usize)) ^ (tbl[768 + lo2] as usize)) & 0xFF;

                let hi3 = ((v6 & 0xF0) | (v7 >> 4)) & 0xFF;
                let lo3 = (((v6 & 0x0F) << 4) | (v7 & 0x0F)) & 0xFF;

                temp_block[next_src_start + i4 + k2] =
                    ((16 * (tbl[1024 + hi3] as usize)) ^ (tbl[1280 + lo3] as usize)) & 0xFF;
            }
        }
    }

    for (idx, out) in out_block.iter_mut().enumerate() {
        let pos = base_final + (idx << 8) + temp_block[SHIFT_INDICES[idx] + FINAL_SRC_START];
        *out = stream_byte(pos)?;
    }

    Ok(out_block)
}

/// Sign a 16-byte nonce block using Samsung's `auth_param.dat` transform and return lowercase hex.
pub fn decrypt_nonce(nonce: &str) -> Result<String> {
    let mut block = [b'0'; 16];
    let bytes = nonce.as_bytes();
    let copy_len = bytes.len().min(16);
    block[..copy_len].copy_from_slice(&bytes[..copy_len]);
    let out = authenticate_block(&block)?;
    Ok(hex::encode(out))
}

/// Compute MD5 digest as a 16-byte key.
pub fn md5_bytes(input: &[u8]) -> [u8; 16] {
    let digest = Md5::digest(input);
    let mut out = [0u8; 16];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Compute lowercase hexadecimal MD5 digest.
pub fn md5_hex(input: &[u8]) -> String {
    hex::encode(Md5::digest(input))
}

/// Derive the `.enc2` firmware decryption key from `(fw, model, region)`.
pub fn get_v2_key(fw: &str, model: &str, region: &str) -> ([u8; 16], String) {
    let raw = format!("{region}:{model}:{fw}");
    (md5_bytes(raw.as_bytes()), raw)
}

/// Derive a 16-byte AES-128 key from a raw decryption key string (`.enc4` / `.enc2` key text).
pub fn key_from_string(raw_key: &str) -> [u8; 16] {
    md5_bytes(raw_key.as_bytes())
}

/// Legacy FUS auth helpers (fallback when hardcoded legacy AES-CBC is ever needed).
pub mod legacy {
    use super::*;

    fn pad(data: &[u8]) -> Vec<u8> {
        let pad_len = 16 - (data.len() % 16);
        let mut out = Vec::with_capacity(data.len() + pad_len);
        out.extend_from_slice(data);
        out.resize(data.len() + pad_len, pad_len as u8);
        out
    }

    fn unpad(data: &[u8]) -> Result<Vec<u8>> {
        let Some(&last) = data.last() else {
            return Err(BloomError::Crypto("empty padded payload".into()));
        };
        let last_i8 = last as i8;
        if last_i8 >= 0 {
            let n = last as usize;
            if n > data.len() {
                return Err(BloomError::Crypto("invalid padding length".into()));
            }
            Ok(data[..data.len() - n].to_vec())
        } else {
            let n = last_i8.unsigned_abs() as usize;
            Ok(data.iter().copied().take(n).collect())
        }
    }

    fn aes_cbc_encrypt(input: &[u8], key: &[u8]) -> Result<Vec<u8>> {
        let key_arr: [u8; 32] = key
            .try_into()
            .map_err(|_| BloomError::Crypto("legacy AES-256 key must be 32 bytes".into()))?;
        let mut prev: [u8; 16] = key_arr[..16]
            .try_into()
            .map_err(|_| BloomError::Crypto("invalid IV".into()))?;
        let cipher = aes::Aes256::new(&Array::from(key_arr));

        let mut buf = pad(input);
        for chunk in buf.as_chunks_mut::<16>().0 {
            for (b, p) in chunk.iter_mut().zip(prev.iter()) {
                *b ^= *p;
            }
            let mut block = Array::from(*chunk);
            cipher.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
            prev.copy_from_slice(&block);
        }
        Ok(buf)
    }

    fn aes_cbc_decrypt(input: &[u8], key: &[u8]) -> Result<Vec<u8>> {
        if input.len() % 16 != 0 {
            return Err(BloomError::Crypto("ciphertext length not multiple of 16".into()));
        }
        let key_arr: [u8; 32] = key
            .try_into()
            .map_err(|_| BloomError::Crypto("legacy AES-256 key must be 32 bytes".into()))?;
        let mut prev: [u8; 16] = key_arr[..16]
            .try_into()
            .map_err(|_| BloomError::Crypto("invalid IV".into()))?;
        let cipher = aes::Aes256::new(&Array::from(key_arr));

        let mut buf = input.to_vec();
        for chunk in buf.as_chunks_mut::<16>().0 {
            let ct = *chunk;
            let mut block = Array::from(ct);
            cipher.decrypt_block(&mut block);
            for (b, p) in block.iter_mut().zip(prev.iter()) {
                *b ^= *p;
            }
            chunk.copy_from_slice(&block);
            prev = ct;
        }
        unpad(&buf)
    }

    fn get_f_key(input: &[u8]) -> Vec<u8> {
        let mut key = Vec::with_capacity(32);
        for &b in input.iter().take(16) {
            key.push(LEGACY_KEY_1[(b as usize) % LEGACY_KEY_1.len()]);
        }
        key.extend_from_slice(LEGACY_KEY_2);
        key
    }

    pub fn decrypt_nonce(input: &str) -> Result<String> {
        let decoded = BASE64
            .decode(input.trim())
            .map_err(|e| BloomError::Crypto(format!("invalid base64 nonce: {e}")))?;
        let plain = aes_cbc_decrypt(&decoded, LEGACY_KEY_1)?;
        String::from_utf8(plain)
            .map_err(|e| BloomError::Crypto(format!("non-utf8 decrypted nonce: {e}")))
    }

    pub fn get_auth(nonce: &str) -> Result<String> {
        let key_data: Vec<u8> = nonce.bytes().map(|b| b % 16).collect();
        let f_key = get_f_key(&key_data);
        let encrypted = aes_cbc_encrypt(nonce.as_bytes(), &f_key)?;
        Ok(BASE64.encode(encrypted))
    }
}

/// Verify CRC32 and/or MD5 of `path` in a single sequential read pass.
pub fn verify_file_single_pass<F>(
    path: &Path,
    expected_crc32: Option<u32>,
    expected_md5: Option<&str>,
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u64, u64),
{
    if expected_crc32.is_none() && expected_md5.is_none() {
        return Ok(());
    }

    let file = File::open(path)?;
    let total_size = file.metadata()?.len();
    let mut reader = BufReader::with_capacity(VERIFY_BUF_SIZE, file);

    let mut crc_hasher = expected_crc32.map(|_| Crc32Hasher::new());
    let mut md5_hasher = expected_md5.map(|_| Md5::new());

    let mut buf = vec![0u8; VERIFY_BUF_SIZE];
    let mut processed = 0u64;

    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let slice = &buf[..n];
        if let Some(ref mut crc) = crc_hasher {
            crc.update(slice);
        }
        if let Some(ref mut md5) = md5_hasher {
            md5.update(slice);
        }
        processed += n as u64;
        on_progress(processed, total_size);
    }

    if let (Some(expected), Some(crc)) = (expected_crc32, crc_hasher) {
        let actual = crc.finalize();
        if actual != expected {
            return Err(BloomError::ChecksumMismatch {
                kind: "CRC32",
                expected: format!("{expected:#010x}"),
                actual: format!("{actual:#010x}"),
            });
        }
    }

    if let (Some(expected_raw), Some(md5)) = (expected_md5, md5_hasher) {
        let digest = md5.finalize();
        let actual_hex = hex::encode(digest);
        let trimmed = expected_raw.trim();
        let matches = if trimmed.len() == 32 {
            actual_hex.eq_ignore_ascii_case(trimmed)
        } else {
            BASE64.encode(digest) == trimmed
        };
        if !matches {
            return Err(BloomError::ChecksumMismatch {
                kind: "MD5",
                expected: trimmed.to_owned(),
                actual: actual_hex,
            });
        }
    }

    Ok(())
}

/// Decrypt an `.enc2` or `.enc4` firmware file using parallel multi-core AES-128-ECB.
pub fn decrypt_file_parallel<F>(
    input_path: &Path,
    output_path: &Path,
    key: &[u8; 16],
    mut on_progress: F,
) -> Result<u64>
where
    F: FnMut(u64, u64),
{
    let in_file = File::open(input_path)?;
    let total_size = in_file.metadata()?.len();
    if total_size == 0 || total_size % 16 != 0 {
        return Err(BloomError::Crypto(format!(
            "encrypted file size ({total_size} bytes) must be a non-zero multiple of 16 bytes"
        )));
    }

    let out_file = File::create(output_path)?;
    let mut reader = BufReader::with_capacity(DECRYPT_CHUNK_SIZE, in_file);
    let mut writer = BufWriter::with_capacity(DECRYPT_CHUNK_SIZE, out_file);
    let cipher = Aes128::new(&Array::from(*key));

    let mut buf = vec![0u8; DECRYPT_CHUNK_SIZE];
    let mut processed = 0u64;

    loop {
        let n = read_exact_or_eof(&mut reader, &mut buf)?;
        if n == 0 {
            break;
        }
        if n % 16 != 0 {
            return Err(BloomError::Crypto(
                "unexpected non-aligned block during decryption".into(),
            ));
        }

        buf[..n]
            .par_chunks_mut(PARALLEL_SUBCHUNK_SIZE)
            .for_each(|subchunk| {
                for block_slice in subchunk.as_chunks_mut::<16>().0 {
                    let mut block = Array::from(*block_slice);
                    cipher.decrypt_block(&mut block);
                    block_slice.copy_from_slice(&block);
                }
            });

        writer.write_all(&buf[..n])?;
        processed += n as u64;
        on_progress(processed, total_size);
    }

    writer.flush()?;
    Ok(processed)
}

const ECB_STREAM_CACHE_SIZE: usize = 64 * 1024;

/// Streaming random-access `Read + Seek` adapter that decrypts AES-128-ECB blocks on the fly
/// so encrypted Samsung `.enc4`/`.enc2` archives can be extracted directly without writing a
/// temporary multi-gigabyte `.zip` file to disk.
pub struct Aes128EcbReadSeek<R: Read + Seek> {
    inner: R,
    cipher: Aes128,
    logical_len: u64,
    pos: u64,
    cache_start: u64,
    cache: Vec<u8>,
}

impl<R: Read + Seek> Aes128EcbReadSeek<R> {
    pub fn new(mut inner: R, key: &[u8; 16]) -> Result<Self> {
        let raw_len = inner.seek(SeekFrom::End(0))?;
        if raw_len == 0 || raw_len % 16 != 0 {
            return Err(BloomError::Crypto(format!(
                "encrypted archive size ({raw_len} bytes) must be a non-zero multiple of 16 bytes"
            )));
        }
        let cipher = Aes128::new(&Array::from(*key));
        let logical_len = detect_zip_logical_len(&mut inner, &cipher, raw_len)?;
        inner.seek(SeekFrom::Start(0))?;

        Ok(Self {
            inner,
            cipher,
            logical_len,
            pos: 0,
            cache_start: u64::MAX,
            cache: Vec::new(),
        })
    }

    fn fill_cache_for(&mut self, offset: u64) -> io::Result<()> {
        let chunk_sz = ECB_STREAM_CACHE_SIZE as u64;
        let aligned_start = (offset / chunk_sz) * chunk_sz;
        let raw_total = self.inner.seek(SeekFrom::End(0))?;
        let to_read = (chunk_sz.min(raw_total.saturating_sub(aligned_start))) as usize;
        if to_read == 0 {
            self.cache.clear();
            self.cache_start = aligned_start;
            return Ok(());
        }

        self.inner.seek(SeekFrom::Start(aligned_start))?;
        self.cache.resize(to_read, 0);
        let actual = read_exact_or_eof(&mut self.inner, &mut self.cache)?;
        let aligned_actual = actual - (actual % 16);
        self.cache.truncate(aligned_actual);

        for block_slice in self.cache.as_chunks_mut::<16>().0 {
            let mut block = Array::from(*block_slice);
            self.cipher.decrypt_block(&mut block);
            block_slice.copy_from_slice(&block);
        }
        self.cache_start = aligned_start;
        Ok(())
    }
}

fn detect_zip_logical_len<R: Read + Seek>(
    inner: &mut R,
    cipher: &Aes128,
    raw_len: u64,
) -> Result<u64> {
    const EOCD_SIG: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];
    let window_raw = raw_len.min(66 * 1024);
    let tail_start = (raw_len - window_raw) & !15;
    let tail_len = (raw_len - tail_start) as usize;
    if tail_len < 22 {
        return Ok(raw_len);
    }

    inner.seek(SeekFrom::Start(tail_start))?;
    let mut tail = vec![0u8; tail_len];
    let n = read_exact_or_eof(inner, &mut tail)?;
    let aligned_n = n - (n % 16);
    tail.truncate(aligned_n);

    for block_slice in tail.as_chunks_mut::<16>().0 {
        let mut block = Array::from(*block_slice);
        cipher.decrypt_block(&mut block);
        block_slice.copy_from_slice(&block);
    }

    if tail.len() >= 22 {
        for i in (0..=tail.len() - 22).rev() {
            if tail[i..i + 4] == EOCD_SIG {
                let comment_len = u16::from_le_bytes([tail[i + 20], tail[i + 21]]) as usize;
                let end_in_tail = i + 22 + comment_len;
                if end_in_tail <= tail.len() {
                    let trailing = tail.len() - end_in_tail;
                    if trailing <= 16 {
                        return Ok(tail_start + end_in_tail as u64);
                    }
                }
            }
        }
    }

    Ok(raw_len)
}

impl<R: Read + Seek> Read for Aes128EcbReadSeek<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() || self.pos >= self.logical_len {
            return Ok(0);
        }

        let max_avail = (self.logical_len - self.pos).min(out.len() as u64) as usize;
        let in_cache = self.cache_start <= self.pos
            && self.pos < self.cache_start.saturating_add(self.cache.len() as u64);
        if !in_cache {
            self.fill_cache_for(self.pos)?;
        }

        let cache_end = self.cache_start.saturating_add(self.cache.len() as u64);
        if self.pos >= cache_end {
            return Ok(0);
        }

        let start_idx = (self.pos - self.cache_start) as usize;
        let copy_len = max_avail.min(self.cache.len() - start_idx);
        out[..copy_len].copy_from_slice(&self.cache[start_idx..start_idx + copy_len]);
        self.pos += copy_len as u64;
        Ok(copy_len)
    }
}

impl<R: Read + Seek> Seek for Aes128EcbReadSeek<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(v) => v as i128,
            SeekFrom::Current(delta) => self.pos as i128 + i128::from(delta),
            SeekFrom::End(delta) => self.logical_len as i128 + i128::from(delta),
        };
        if next < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot seek before start of decrypted archive",
            ));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}

/// Streams and extracts a Samsung `.enc4`/`.enc2` firmware archive directly into `output_dir`
/// without creating an intermediate `.zip` file on disk.
pub fn extract_encrypted_to_folder<F>(
    input_path: &Path,
    output_dir: &Path,
    key: &[u8; 16],
    mut on_progress: F,
) -> Result<Vec<PathBuf>>
where
    F: FnMut(u64, u64),
{
    let file = File::open(input_path)?;
    let stream = Aes128EcbReadSeek::new(file, key)?;
    let mut archive = zip::ZipArchive::new(stream)
        .map_err(|e| BloomError::Crypto(format!("invalid encrypted firmware zip: {e}")))?;

    fs::create_dir_all(output_dir)?;

    let mut total_uncompressed = 0u64;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            if !entry.is_dir() {
                total_uncompressed = total_uncompressed.saturating_add(entry.size());
            }
        }
    }

    let mut extracted = Vec::new();
    let mut extracted_bytes = 0u64;
    let mut buf = vec![0u8; DECRYPT_CHUNK_SIZE];

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| BloomError::Crypto(format!("zip entry error: {e}")))?;
        let Some(rel_path) = entry.enclosed_name() else {
            continue;
        };
        let target = output_dir.join(rel_path);
        if entry.is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        let out_file = File::create(&target)?;
        let mut writer = BufWriter::with_capacity(DECRYPT_CHUNK_SIZE, out_file);
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buf[..n])?;
            extracted_bytes = extracted_bytes.saturating_add(n as u64);
            on_progress(extracted_bytes, total_uncompressed.max(extracted_bytes));
        }
        writer.flush()?;
        extracted.push(target);
    }

    on_progress(total_uncompressed, total_uncompressed);
    Ok(extracted)
}

fn read_exact_or_eof<R: Read>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        let n = reader.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    Ok(filled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn auth_param_header_and_block_transform_work() {
        let block = *b"0123456789abcdef";
        let sig1 = authenticate_block(&block).expect("authenticate_block should succeed");
        let sig2 = authenticate_block(&block).expect("deterministic");
        assert_eq!(sig1, sig2);
        assert_ne!(sig1, [0u8; 16]);

        let hex_sig = decrypt_nonce("short").expect("padding short nonce should succeed");
        assert_eq!(hex_sig.len(), 32);
    }

    #[test]
    fn v2_key_matches_md5_format() {
        let (key, raw) = get_v2_key("A/B/C/D", "SM-G998B", "XID");
        assert_eq!(raw, "XID:SM-G998B:A/B/C/D");
        assert_eq!(hex::encode(key), md5_hex(b"XID:SM-G998B:A/B/C/D"));
    }

    #[test]
    fn legacy_auth_roundtrip_works() {
        let token = legacy::get_auth("ABCDEFGHIJKLMNOP").expect("legacy get_auth");
        assert!(!token.is_empty());
    }

    #[test]
    fn parallel_aes_ecb_decrypt_and_single_pass_verify_roundtrip() {
        let dir = tempdir().unwrap();
        let enc_path = dir.path().join("test.zip.enc4");
        let dec_path = dir.path().join("test.zip");

        let key = key_from_string("test-v4-key");
        let cipher = Aes128::new(&Array::from(key));

        // 256 KiB + 32 bytes payload (multiple of 16)
        let mut plaintext = Vec::with_capacity(256 * 1024 + 32);
        for i in 0..(256 * 1024 + 32) {
            plaintext.push((i % 251) as u8);
        }

        let mut ciphertext = plaintext.clone();
        for chunk in ciphertext.as_chunks_mut::<16>().0 {
            let mut block = Array::from(*chunk);
            cipher.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
        }

        std::fs::write(&enc_path, &ciphertext).unwrap();

        let expected_crc = crc32fast::hash(&ciphertext);
        let expected_md5 = md5_hex(&ciphertext).to_uppercase();

        verify_file_single_pass(&enc_path, Some(expected_crc), Some(&expected_md5), |_, _| {})
            .expect("single-pass verify should succeed");

        let decrypted_bytes =
            decrypt_file_parallel(&enc_path, &dec_path, &key, |_, _| {}).expect("decrypt");
        assert_eq!(decrypted_bytes, plaintext.len() as u64);
        assert_eq!(std::fs::read(&dec_path).unwrap(), plaintext);
    }

    #[test]
    fn streams_encrypted_zip_directly_to_folder_with_padding() {
        use zip::write::SimpleFileOptions;

        let dir = tempdir().unwrap();
        let enc_path = dir.path().join("fw.zip.enc4");
        let out_dir = dir.path().join("extracted");

        let mut zip_buf = io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut zip_buf);
            writer
                .start_file("AP_sample.tar.md5", SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"hello-samsung-firmware").unwrap();
            writer.finish().unwrap();
        }

        let mut plain_zip = zip_buf.into_inner();
        let pad_len = 16 - (plain_zip.len() % 16);
        plain_zip.extend(std::iter::repeat_n(pad_len as u8, pad_len));

        let key = key_from_string("stream-extract-key");
        let cipher = Aes128::new(&Array::from(key));
        for chunk in plain_zip.as_chunks_mut::<16>().0 {
            let mut block = Array::from(*chunk);
            cipher.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
        }
        std::fs::write(&enc_path, &plain_zip).unwrap();

        let files = extract_encrypted_to_folder(&enc_path, &out_dir, &key, |_, _| {})
            .expect("direct extract should succeed");
        assert_eq!(files.len(), 1);
        assert_eq!(
            std::fs::read(out_dir.join("AP_sample.tar.md5")).unwrap(),
            b"hello-samsung-firmware"
        );
    }
}
