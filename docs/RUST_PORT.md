# Rust implementation notes

The decoder still reads outer `.55pro` format version `1`. Version `3` is used
only when the encoder emits `lz55x` or `huf-lz55x` blocks.

Implementation shape:

- native worker threads
- Rust library API exposed through `fivefivepro`
- CLI binaries are `55pro` and `5.5pro`
- no third-party Rust dependencies
- internal JSON parser for the folder manifest format

Known intentional choices:

- JSON key order in folder payloads is implementation-defined. This is valid because each payload stores its own manifest CRC.
- folder mode rejects non-UTF-8 path names explicitly.
- file modification times are stored for format compatibility but are not restored by this no-dependency build.
- same-directory temporary files are used before replacement. Unix replacement is atomic; platforms without rename-over-existing support may need additional platform-specific hardening before release binaries are published.
