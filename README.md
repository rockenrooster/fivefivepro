# 5.5pro Rust

This is the Rust implementation of the experimental **5.5pro** lossless
compressor and `.55pro` file format.

The Python implementation is now treated as the v0.3 reference prototype. This
crate is the native-code port intended to become the production engine. It currently uses only the Rust standard library.

## Features

- `.55pro` version-1 decode compatibility, plus v3 output when LZ55X methods win
- file compression and decompression
- directory packing/extraction using the internal `55PROPATH` payload layer
- compression levels `0..9`
- multithreaded independent block compression/decompression, defaulting to `-T 0` auto/max and accepting `-T 0..1024`, `auto`, `cpu`, and `cpus`
- overwrite-by-default behavior with `--no-overwrite` for refusal mode
- per-block CRC32 and whole-payload CRC32 verification
- LZ55X and HUF-LZ55X for compact long-match encoding without manual `.55pro.55pro` second passes
- safe extraction checks for absolute paths, `..`, backslashes, NUL bytes, and symlink traversal
- Python v0.3 compatibility fixtures in `tests/fixtures`

## Build

```bash
cargo build --release
```

The compiled Rust binary is:

```bash
./target/release/fivefivepro
```

Source-tree wrappers are included so you can use the shorter CLI names after building:

```bash
./bin/55pro --version
./bin/5.5pro --version
```

For a system install, copy the compiled binary under either CLI name:

```bash
install -m 0755 target/release/fivefivepro /usr/local/bin/55pro
ln -sf /usr/local/bin/55pro /usr/local/bin/5.5pro
```

## Test

```bash
cargo test
```

The test suite covers byte round trips, all compression levels, 1024-thread
limit validation, Python v0.3 archive decoding, directory archive decoding,
folder extraction, random and repetitive data, LZ55X, and a CLI file round trip
when Cargo exposes the test binary.

## Usage

Compress a file:

```bash
55pro c input.bin input.bin.55pro -l 7
```

Decompress a file:

```bash
55pro d input.bin.55pro restored.bin
```

Compress a folder:

```bash
55pro c my-folder my-folder.55pro -l 7 -T 0
```

Extract a folder archive:

```bash
55pro x my-folder.55pro restored-folder -T 0
```

Verify an archive:

```bash
55pro test input.55pro
```

Inspect an archive:

```bash
55pro info input.55pro
55pro info input.55pro --deep
```

Refuse overwriting an existing output:

```bash
55pro c input.bin output.55pro --no-overwrite
55pro d output.55pro restored.bin --no-overwrite
```

## Compression levels

| Level | Enabled methods | Search effort | Intended use |
|---:|---|---|---|
| `0` | `raw` | none | fastest storage/framing only |
| `1` | `raw`, `rle` | none | fast handling of simple runs |
| `2` | `raw`, `rle`, `lz55` | shallow | fast general compression |
| `3` | `raw`, `rle`, `lz55` | shallow | slightly stronger fast mode |
| `4` | plus `hufraw` | medium | data where byte skew helps |
| `5` | plus `huf-lz55`, `lz55x`, `huf-lz55x` | medium | balanced default mode |
| `6` | all current methods | deeper | better general compression |
| `7` | all current methods | deeper | high compression mode |
| `8` | all current methods | aggressive | ratio over speed |
| `9` | all current methods | maximum | best ratio in this implementation |

5.5pro chooses the smallest enabled representation per block. Incompressible
blocks are stored as `raw`, even at high levels.

The default block size remains 1 MiB. `--block-size 4m` can improve ratio on
large repetitive files, but can slow random or incompressible data because each
block costs more work.

## Compatibility status

The Rust decoder reads `.55pro` version-1 archives from the Python v0.3
implementation. Archives using `lz55x` or `huf-lz55x` are written as v3 and
require a v0.6+ compatible decoder. The path archive layer still uses a JSON
manifest; Rust writes a compact JSON manifest and computes the manifest CRC over
the exact bytes stored in each archive, so Rust and Python payloads can be
decoded without requiring byte-identical manifest serialization.

## Current production notes

This port removes the Python GIL limitation and uses native worker threads. It
still keeps the simple v1 design of reading the full input to compute the
container header CRC and original size before writing the archive. A future v2
format could add a streaming footer or seek-back header mode for very large
inputs.

Non-UTF-8 path names are rejected in folder mode because the v1 path archive
manifest is UTF-8 JSON.
