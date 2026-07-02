use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use fivefivepro::codec::{
    compress_bytes, decompress_bytes, inspect_archive_bytes, lz55_compress, lz55_decompress,
    lz55x_compress, lz55x_decompress, normalize_threads, rle_compress, rle_decompress, MAX_THREADS,
    MIN_BLOCK_SIZE,
};
use fivefivepro::path_archive::{extract_path_archive, pack_directory};

fn unique_temp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "55pro-rust-test-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn byte_roundtrips_all_levels() {
    let mut data = Vec::new();
    for _ in 0..700 {
        data.extend_from_slice(b"level behavior test data\n");
    }
    for i in 0..4096 {
        data.push((i % 251) as u8);
    }
    for level in 0..=9 {
        let archive = compress_bytes(&data, level, MIN_BLOCK_SIZE, 4).unwrap();
        let info = inspect_archive_bytes(&archive).unwrap();
        assert_eq!(info.level, level);
        assert_eq!(decompress_bytes(&archive, true, 3).unwrap(), data);
    }
}

#[test]
fn direct_primitives_roundtrip() {
    let data = b"mississippi ".repeat(1000);
    let packet = lz55_compress(&data, 9).unwrap();
    assert_eq!(lz55_decompress(&packet, data.len()).unwrap(), data);

    let long_data = b"A".repeat(1_000_000);
    let xpacket = lz55x_compress(&long_data, 9).unwrap();
    assert_eq!(
        lz55x_decompress(&xpacket, long_data.len()).unwrap(),
        long_data
    );

    let mut rle_data = b"A".repeat(300);
    rle_data.extend_from_slice(b"BCDE");
    rle_data.extend_from_slice(&b"Z".repeat(129));
    let rle = rle_compress(&rle_data);
    assert_eq!(rle_decompress(&rle, rle_data.len()).unwrap(), rle_data);
}

#[test]
fn thread_limit_is_enforced() {
    assert_eq!(
        normalize_threads(0).unwrap(),
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .clamp(1, MAX_THREADS)
    );
    assert_eq!(normalize_threads(MAX_THREADS).unwrap(), MAX_THREADS);
    assert!(normalize_threads(MAX_THREADS + 1).is_err());
}

#[test]
fn random_data_roundtrip() {
    let mut data = Vec::with_capacity(1 << 20);
    let mut x = 0x1234_5678_9abc_def0u64;
    for _ in 0..(1 << 20) {
        x ^= x << 7;
        x ^= x >> 9;
        x ^= x << 8;
        data.push((x >> 32) as u8);
    }
    let archive = compress_bytes(&data, 5, MIN_BLOCK_SIZE, 0).unwrap();
    assert_eq!(decompress_bytes(&archive, true, 0).unwrap(), data);
}

#[test]
fn repetitive_data_uses_lz55x_family() {
    let data = b"A".repeat(1_000_000);
    let archive = compress_bytes(&data, 9, MIN_BLOCK_SIZE, 0).unwrap();
    let info = inspect_archive_bytes(&archive).unwrap();
    assert!(info.methods.contains_key("lz55x") || info.methods.contains_key("huf-lz55x"));
    assert_eq!(decompress_bytes(&archive, true, 0).unwrap(), data);
}

#[test]
fn directory_payload_roundtrip() {
    let root = unique_temp_dir("dir");
    let src = root.join("project");
    fs::create_dir_all(src.join("docs/deep")).unwrap();
    fs::create_dir_all(src.join("empty-dir")).unwrap();
    fs::write(src.join("README.txt"), b"hello folder support\n".repeat(20)).unwrap();
    let mut nested_data = vec![7u8; 1024];
    nested_data.extend(0u8..=255);
    fs::write(src.join("docs/deep/data.bin"), nested_data).unwrap();

    let payload = pack_directory(&src).unwrap();
    let archive = compress_bytes(&payload, 7, MIN_BLOCK_SIZE, 3).unwrap();
    let restored_payload = decompress_bytes(&archive, true, 3).unwrap();
    let out = root.join("restored");
    extract_path_archive(&restored_payload, &out, true).unwrap();
    assert_eq!(
        fs::read(out.join("README.txt")).unwrap(),
        fs::read(src.join("README.txt")).unwrap()
    );
    assert_eq!(
        fs::read(out.join("docs/deep/data.bin")).unwrap(),
        fs::read(src.join("docs/deep/data.bin")).unwrap()
    );
    assert!(out.join("empty-dir").is_dir());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn cli_smoke_roundtrip_when_binary_is_available() {
    let Some(bin) = option_env!("CARGO_BIN_EXE_55pro") else {
        return;
    };
    let root = unique_temp_dir("cli");
    let src = root.join("sample.bin");
    let arc = root.join("sample.bin.55pro");
    let out = root.join("sample.out");
    fs::write(&src, b"55pro Rust CLI roundtrip\n".repeat(1000)).unwrap();
    fs::write(&arc, b"old archive").unwrap();
    assert!(Command::new(bin)
        .args([
            "c",
            src.to_str().unwrap(),
            arc.to_str().unwrap(),
            "-l",
            "7",
            "-b",
            "4k",
            "-T",
            "4"
        ])
        .status()
        .unwrap()
        .success());
    assert!(Command::new(bin)
        .args(["t", arc.to_str().unwrap(), "-T", "4"])
        .status()
        .unwrap()
        .success());
    assert!(Command::new(bin)
        .args(["d", arc.to_str().unwrap(), out.to_str().unwrap(), "-T", "4"])
        .status()
        .unwrap()
        .success());
    assert_eq!(fs::read(&out).unwrap(), fs::read(&src).unwrap());

    let refused = root.join("refused.55pro");
    fs::write(&refused, b"existing").unwrap();
    assert!(!Command::new(bin)
        .args([
            "c",
            src.to_str().unwrap(),
            refused.to_str().unwrap(),
            "--no-overwrite"
        ])
        .status()
        .unwrap()
        .success());
    let _ = fs::remove_dir_all(root);
}
