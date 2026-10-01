//! EME wide-block mode over AES-256, byte-compatible with github.com/rfjakob/eme
//! as used by rclone for standard file name encryption.
use aes::cipher::{generic_array::GenericArray, BlockDecrypt, BlockEncrypt};
use aes::Aes256;

const BLOCK: usize = 16;
/// EME operates on 1..=128 AES blocks.
pub(super) const MAX_BLOCKS: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Direction {
    Encrypt,
    Decrypt,
}

fn mult_by_two(block: &mut [u8; BLOCK]) {
    let input = *block;
    block[0] = input[0].wrapping_mul(2);
    if input[15] >= 128 {
        block[0] ^= 135;
    }
    for j in 1..BLOCK {
        block[j] = input[j]
            .wrapping_mul(2)
            .wrapping_add(u8::from(input[j - 1] >= 128));
    }
}

fn xor_into(out: &mut [u8], a: &[u8], b: &[u8]) {
    for ((o, x), y) in out.iter_mut().zip(a).zip(b) {
        *o = x ^ y;
    }
}

fn aes(cipher: &Aes256, block: &mut [u8], direction: Direction) {
    let block = GenericArray::from_mut_slice(block);
    match direction {
        Direction::Encrypt => cipher.encrypt_block(block),
        Direction::Decrypt => cipher.decrypt_block(block),
    }
}

/// Panics are replaced by `None`: `data` must be 1..=128 whole blocks.
pub(super) fn transform(
    cipher: &Aes256,
    tweak: &[u8; BLOCK],
    data: &[u8],
    direction: Direction,
) -> Option<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(BLOCK) || data.len() / BLOCK > MAX_BLOCKS {
        return None;
    }
    let m = data.len() / BLOCK;

    // L table: L_i = 2^(i+1) * AES-enc(K, 0), always with the encrypt direction.
    let mut li = [0u8; BLOCK];
    cipher.encrypt_block(GenericArray::from_mut_slice(&mut li));
    let mut l_table = Vec::with_capacity(m);
    for _ in 0..m {
        mult_by_two(&mut li);
        l_table.push(li);
    }

    let mut c = vec![0u8; data.len()];
    for (j, l) in l_table.iter().enumerate() {
        let range = j * BLOCK..(j + 1) * BLOCK;
        xor_into(&mut c[range.clone()], &data[range.clone()], l);
        aes(cipher, &mut c[range], direction);
    }

    let mut mp = [0u8; BLOCK];
    xor_into(&mut mp, &c[..BLOCK], tweak);
    for j in 1..m {
        let prev = mp;
        xor_into(&mut mp, &prev, &c[j * BLOCK..(j + 1) * BLOCK]);
    }
    let mut mc = mp;
    aes(cipher, &mut mc, direction);
    let mut mm = [0u8; BLOCK];
    xor_into(&mut mm, &mp, &mc);

    for j in 1..m {
        mult_by_two(&mut mm);
        let range = j * BLOCK..(j + 1) * BLOCK;
        let pppj: [u8; BLOCK] = c[range.clone()].try_into().unwrap();
        xor_into(&mut c[range], &pppj, &mm);
    }

    let mut ccc1 = [0u8; BLOCK];
    xor_into(&mut ccc1, &mc, tweak);
    for j in 1..m {
        let prev = ccc1;
        xor_into(&mut ccc1, &prev, &c[j * BLOCK..(j + 1) * BLOCK]);
    }
    c[..BLOCK].copy_from_slice(&ccc1);

    for (j, l) in l_table.iter().enumerate() {
        let range = j * BLOCK..(j + 1) * BLOCK;
        aes(cipher, &mut c[range.clone()], direction);
        let ccj: [u8; BLOCK] = c[range.clone()].try_into().unwrap();
        xor_into(&mut c[range], &ccj, l);
    }
    Some(c)
}
