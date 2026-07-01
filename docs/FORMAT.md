# 5.5pro `.55pro` format, versions 1 and 3

All multi-byte integers are little-endian.

## Outer container

Header:

```text
magic          7 bytes   35 35 50 52 4f 1a 0a  (ASCII "55PRO", 0x1a, LF)
version        u8        1 for legacy methods, 3 when method 6/7 is emitted
level          u8        encoder effort, 0..9
original_size  u64       full decompressed payload size
block_size     u32       maximum uncompressed block size
crc32          u32       CRC32 of full decompressed payload
```

Blocks follow until `original_size` bytes have been produced:

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

Blocks are independent and can be encoded/decoded in parallel, then emitted in
block order.

## Method 0: raw

Payload is the original block bytes.

## Method 1: rle

PackBits-style packets. Each packet starts with a one-byte header.

```text
0xxxxxxx  literal packet: copy header + 1 bytes
1xxxxxxx  run packet: repeat next byte ((header & 0x7f) + 1) times
```

The encoder emits run packets only for runs of at least four bytes.

## Method 2: lz55

LZ77-family packet stream. A sequence starts with one token byte:

```text
high nibble: literal length code
low nibble:  match length code
```

If a nibble is `15`, extension bytes follow. Each extension byte is added to the
length and extension continues while the byte is `255`.

After the literal length, literal bytes appear directly. If the sequence is not
the final literal-only sequence, it continues with:

```text
offset       u16, distance back from current output position
match_len    low_nibble + 4, plus extension bytes when low_nibble == 15
```

Offsets are `1..65535`. Overlapping copies are decoded byte by byte.

## Method 3: hufraw

Canonical Huffman coding of the original block bytes.

```text
output_size     u32
code_lengths    256 bytes, one code length per byte value
bitstream       MSB-first canonical Huffman codes, zero-padded at end
```

The decoder emits exactly `output_size` symbols.

## Method 4: huf-lz55

Canonical Huffman coding of a method-2 LZ55 packet stream. The Huffman decoded
output is then decoded as LZ55 to reconstruct the block.

## Method 6: lz55x

LZ55X uses the same token byte, offset field, minimum match length, and match
copy rules as method 2. When a literal or match length nibble is `15`, the
extension is encoded as a ULEB128-style integer instead of LZ55's repeated
255-byte extension sum. This is more compact for long repeated matches.

## Method 7: huf-lz55x

Canonical Huffman coding of a method-6 LZ55X packet stream. The Huffman decoded
output is then decoded as LZ55X to reconstruct the block.

Archives using method 6 or 7 are written as outer format version 3 and require a
v0.6+ compatible decoder. Version-1 archives remain readable.

## Internal directory payload

A directory is converted to a byte stream before outer compression.

```text
magic           11 bytes  ASCII "55PROPATH", 0x1a, LF
version         u8        1
manifest_len    u64       UTF-8 JSON manifest length
file_data_len   u64       concatenated file-data section length
manifest_crc32  u32       CRC32 of manifest bytes
file_data_crc32 u32       CRC32 of file-data bytes
manifest        manifest_len bytes
file_data       file_data_len bytes
```

The manifest shape is:

```json
{
  "format": "5.5pro-path-archive",
  "version": 1,
  "root_name": "example-folder",
  "entries": [
    {"type": "dir", "path": "docs", "mode": 493, "mtime_ns": 0},
    {"type": "file", "path": "docs/readme.txt", "mode": 420, "mtime_ns": 0, "offset": 0, "size": 12, "crc32": 305419896}
  ]
}
```

Paths are POSIX-style relative UTF-8 paths. Decoders reject absolute paths,
empty paths, `.` or `..`, backslashes, NUL bytes, and symlink traversal during
extraction.
