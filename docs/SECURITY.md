# Security notes

The decompressor and extractor parse untrusted data. The Rust port applies these
checks by default:

- validates outer magic/version/block sizes/method IDs
- validates per-block CRC32 and whole-payload CRC32 unless `--no-verify` is used
- rejects trailing bytes after the final outer block
- rejects malformed RLE, LZ55, and Huffman streams
- rejects path archive manifest/data CRC mismatches
- rejects absolute paths, empty paths, `.`/`..`, backslashes, and NUL bytes in folder payloads
- rejects symlinked extraction roots, symlinked parent directories, and symlink output targets
- rejects symlinks and special files while packing folders

`--no-verify` skips CRC checks for the outer compressed byte stream only. It does
not turn unsafe paths into safe paths.
