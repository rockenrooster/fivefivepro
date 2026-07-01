# 5.5pro format specification, Rust draft 0.4

All multi-byte integers are little-endian.

## Outer `.55pro` container

### Header

```text
magic          7 bytes   35 35 50 52 4f 1a 0a  (ASCII "55PRO", 0x1a, LF)
version        u8        1 for legacy methods, 3 when method 6/7 is emitted
level          u8        encoder effort, 0..9
original_size  u64       full decompressed size
block_size     u32       maximum uncompressed block size
crc32          u32       CRC32 of the full decompressed payload
```

### Blocks

Blocks follow until `original_size` bytes have been produced.

```text
method             u8
uncompressed_size  u32
compressed_size    u32
block_crc32        u32
payload            compressed_size bytes
```

Methods:

```text
0 raw
1 rle
2 lz55
3 hufraw
4 huf-lz55
6 lz55x
7 huf-lz55x
```

Blocks are independent and may be compressed/decompressed in parallel. Output is
always emitted in block order.

## Compression levels

```text
0      raw only
1      raw + rle
2..3   raw + rle + lz55 with shallow search
4      raw + rle + lz55 + hufraw
5..9   all current methods, with progressively deeper LZ55/LZ55X search
```

A decoder does not depend on the stored level. It only needs to support the
method IDs found in the block stream.

## RAW

Payload is exactly the original block.

## RLE

PackBits-style packets. Each packet begins with one header byte.

```text
0xxxxxxx  literal packet: copy header + 1 bytes
1xxxxxxx  run packet: repeat following byte ((header & 0x7f) + 1) times
```

Runs are encoded only for repeated sequences of at least four bytes. Long runs
are split into chunks of at most 128 bytes.

## LZ55

LZ77-family match/literal stream. Each sequence begins with one token byte.

```text
high nibble: literal length code
low nibble:  match length code
```

If the literal length nibble is 15, extension bytes follow and are summed until
an extension byte below 255 appears. Literal bytes follow directly.

If the sequence is not final literal-only data, it then contains:

```text
offset       u16, distance backward from current output position
match_len    low_nibble + 4, with extension bytes if low_nibble == 15
```

Offsets are `1..65535`. Overlapping copies are decoded byte-by-byte.

## HUFRAW

Canonical Huffman coding of original block bytes.

```text
output_size     u32
code_lengths    256 bytes
bitstream       MSB-first canonical Huffman codes, zero-padded at the end
```

The decoder emits exactly `output_size` bytes and ignores final padding bits.

## HUF-LZ55

Canonical Huffman coding of an LZ55 packet stream. Huffman decoding produces the
LZ55 packet bytes, which are then decoded to the original block size.

## LZ55X

LZ55X uses the same token shape, offsets, match search, and minimum match length
as LZ55, but replaces LZ55 length-extension runs with ULEB128-style extension
integers. This keeps long repeated matches compact in a single compression pass.

## HUF-LZ55X

Canonical Huffman coding of an LZ55X packet stream. Huffman decoding produces
the LZ55X packet bytes, which are then decoded to the original block size.

Archives using method 6 or 7 are written as outer format version 3 and require a
v0.6+ compatible decoder. Version-1 archives remain readable.

The default block size remains 1 MiB. `--block-size 4m` can improve ratio on
large repetitive files, but may slow random or incompressible data.

## Internal directory payload

A directory is first packed into an internal byte stream, then compressed by the
outer `.55pro` container.

```text
magic           11 bytes  ASCII "55PROPATH", 0x1a, LF
version         u8        currently 1
manifest_len    u64       UTF-8 JSON manifest length
file_data_len   u64       concatenated file-data length
manifest_crc32  u32       CRC32 of manifest bytes
file_data_crc32 u32       CRC32 of file-data bytes
manifest        manifest_len bytes
file_data       file_data_len bytes
```

The manifest is compact JSON:

```json
{
  "format": "5.5pro-path-archive",
  "version": 1,
  "root_name": "example-folder",
  "entries": [
    {"type":"dir","path":"docs","mode":493,"mtime_ns":0},
    {"type":"file","path":"docs/readme.txt","mode":420,"mtime_ns":0,"offset":0,"size":12,"crc32":305419896}
  ]
}
```

Paths are POSIX-style relative paths. Decoders reject absolute paths, empty path
components, `.`, `..`, backslashes, NULs, path traversal, symlinked output
parents, symlink output targets, and special files.
