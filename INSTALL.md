# Install

Build:

```bash
cargo build --release
```

Run from the source tree:

```bash
./bin/55pro --version
./bin/5.5pro --version
```

Compression, decompression, test, and deep info use `-T 0` auto/max threads by
default. Explicit thread values accept `0`, `auto`, `cpu`, `cpus`, and
`1..1024`; values above `1024` are rejected.

The default block size remains 1 MiB. Use `--block-size 4m` when repetitive
files benefit from a larger compression window; it can slow random or
incompressible data.

Install aliases system-wide:

```bash
make install
```

Or manually:

```bash
install -m 0755 target/release/fivefivepro /usr/local/bin/55pro
ln -sf /usr/local/bin/55pro /usr/local/bin/5.5pro
```
