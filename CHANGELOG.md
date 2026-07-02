# Changelog

## 0.6.0

- Added README attribution that the compression algorithm was made by GPT-5.5 Pro (Extended).
- Removed the `5.5pro` CLI alias; `55pro` is the supported executable name.
- Added v3 block methods `lz55x` and `huf-lz55x` for compact long-match length encoding.
- Kept v1 archive decode compatibility and emit v3 only when method 6/7 is used.
- Removed avoidable block and compressed-payload copies in the block pipeline.
- Changed decompress, test, and deep info defaults to `-T 0` auto/max threads.
- Kept the default block size at 1 MiB and documented `--block-size 4m` as a ratio knob.
- Added elapsed time and throughput metrics to compression, decompression, extraction, and test summaries.
- Reduced duplicate LZ work, raw-block copies, Huffman sizing scans, decompression copy loops, and small worker-batch churn.
- Improved folder pack/extract memory behavior by avoiding per-file temporary buffers and duplicate payload parsing.
- Removed legacy fixture files from the test suite.
- Cleaned hot paths and warning issues so `cargo clippy -- -D warnings` can pass.

## 0.4.0

- Ported the 55pro codec and CLI to Rust.
- Preserved the `.55pro` v1 outer container and current method IDs.
- Added native OS-thread block compression/decompression with `-T 0..1024`.
- Added dependency-free CRC32, RLE, LZ55, canonical Huffman, and path archive code.
- Kept overwrite-by-default behavior with `--no-overwrite` refusal mode.
