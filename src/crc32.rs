const fn make_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const TABLE: [u32; 256] = make_table();

/// zlib-compatible CRC32 update.
pub fn crc32_with_seed(data: &[u8], seed: u32) -> u32 {
    let mut crc = seed ^ 0xFFFF_FFFF;
    for &b in data {
        let idx = ((crc ^ u32::from(b)) & 0xFF) as usize;
        crc = (crc >> 8) ^ TABLE[idx];
    }
    crc ^ 0xFFFF_FFFF
}

pub fn crc32(data: &[u8]) -> u32 {
    crc32_with_seed(data, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_crc32() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn incremental_crc32() {
        let full = crc32(b"hello world");
        let seed = crc32_with_seed(b"hello ", 0);
        assert_eq!(crc32_with_seed(b"world", seed), full);
    }
}
