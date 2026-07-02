use std::cmp;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crate::crc32::{crc32, crc32_with_seed};
use crate::error::{Pro55Error, Result};

pub const LEGACY_FORMAT_VERSION: u8 = 1;
pub const FORMAT_VERSION: u8 = 3;
pub const MAGIC: &[u8; 7] = b"55PRO\x1a\n";

pub const METHOD_RAW: u8 = 0;
pub const METHOD_RLE: u8 = 1;
pub const METHOD_LZ55: u8 = 2;
pub const METHOD_HUFRAW: u8 = 3;
pub const METHOD_HUF_LZ55: u8 = 4;
pub const METHOD_LZ55X: u8 = 6;
pub const METHOD_HUF_LZ55X: u8 = 7;

pub const MIN_MATCH: usize = 4;
pub const MAX_OFFSET: usize = 0xFFFF;
pub const DEFAULT_BLOCK_SIZE: usize = 1 << 20;
pub const MIN_BLOCK_SIZE: usize = 4 * 1024;
pub const MAX_BLOCK_SIZE: usize = 16 * 1024 * 1024;
pub const MAX_THREADS: usize = 1024;

const HEADER_SIZE: usize = 25;
const BLOCK_HEADER_SIZE: usize = 13;
const WORK_BATCH_FACTOR: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveInfo {
    pub version: u8,
    pub level: u8,
    pub original_size: u64,
    pub block_size: u32,
    pub crc32: u32,
    pub block_count: usize,
    pub compressed_size: u64,
    pub methods: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
enum Payload<'a> {
    Borrowed(&'a [u8]),
    Owned(Vec<u8>),
}

impl Payload<'_> {
    fn len(&self) -> usize {
        match self {
            Payload::Borrowed(bytes) => bytes.len(),
            Payload::Owned(bytes) => bytes.len(),
        }
    }

    fn as_slice(&self) -> &[u8] {
        match self {
            Payload::Borrowed(bytes) => bytes,
            Payload::Owned(bytes) => bytes,
        }
    }
}

#[derive(Debug, Clone)]
struct EncodedBlock<'a> {
    index: usize,
    method: u8,
    raw_size: usize,
    payload: Payload<'a>,
    crc32: u32,
}

#[derive(Debug, Clone)]
struct BlockRecord<'a> {
    index: usize,
    method: u8,
    uncompressed_size: usize,
    crc32: u32,
    payload: &'a [u8],
}

#[derive(Debug, Clone)]
struct DecodedBlock {
    index: usize,
    data: Vec<u8>,
}

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Pro55Error::new(message))
}

pub fn method_name(method: u8) -> Option<&'static str> {
    match method {
        METHOD_RAW => Some("raw"),
        METHOD_RLE => Some("rle"),
        METHOD_LZ55 => Some("lz55"),
        METHOD_HUFRAW => Some("hufraw"),
        METHOD_HUF_LZ55 => Some("huf-lz55"),
        METHOD_LZ55X => Some("lz55x"),
        METHOD_HUF_LZ55X => Some("huf-lz55x"),
        _ => None,
    }
}

pub fn normalize_threads(threads: usize) -> Result<usize> {
    match threads {
        0 => {
            let auto = thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            Ok(auto.clamp(1, MAX_THREADS))
        }
        1..=MAX_THREADS => Ok(threads),
        _ => err(format!("threads must be 0..{MAX_THREADS}; 0 means auto")),
    }
}

fn worker_count(threads: usize, jobs: usize) -> usize {
    cmp::max(1, cmp::min(threads, cmp::max(1, jobs)))
}

fn validate_level(level: u8) -> Result<u8> {
    if level <= 9 {
        Ok(level)
    } else {
        err("compression level must be between 0 and 9")
    }
}

fn validate_block_size(block_size: usize) -> Result<usize> {
    if (MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size) {
        Ok(block_size)
    } else {
        err(format!(
            "block size must be between {MIN_BLOCK_SIZE} and {MAX_BLOCK_SIZE} bytes"
        ))
    }
}

fn put_u16_le(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u32_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u64_le(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u16_at(data: &[u8], pos: &mut usize, what: &str) -> Result<u16> {
    if *pos + 2 > data.len() {
        return err(format!("unexpected end of file while reading {what}"));
    }
    let value = u16::from_le_bytes([data[*pos], data[*pos + 1]]);
    *pos += 2;
    Ok(value)
}

fn read_u32_at(data: &[u8], pos: &mut usize, what: &str) -> Result<u32> {
    if *pos + 4 > data.len() {
        return err(format!("unexpected end of file while reading {what}"));
    }
    let value = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(value)
}

fn read_u64_at(data: &[u8], pos: &mut usize, what: &str) -> Result<u64> {
    if *pos + 8 > data.len() {
        return err(format!("unexpected end of file while reading {what}"));
    }
    let value = u64::from_le_bytes([
        data[*pos],
        data[*pos + 1],
        data[*pos + 2],
        data[*pos + 3],
        data[*pos + 4],
        data[*pos + 5],
        data[*pos + 6],
        data[*pos + 7],
    ]);
    *pos += 8;
    Ok(value)
}

fn read_bytes<'a>(data: &'a [u8], pos: &mut usize, size: usize, what: &str) -> Result<&'a [u8]> {
    if *pos + size > data.len() {
        return err(format!("unexpected end of file while reading {what}"));
    }
    let out = &data[*pos..*pos + size];
    *pos += size;
    Ok(out)
}

fn write_varlen(out: &mut Vec<u8>, mut value: usize) {
    while value >= 255 {
        out.push(255);
        value -= 255;
    }
    out.push(value as u8);
}

fn write_uleb128(out: &mut Vec<u8>, mut value: usize) {
    while value >= 0x80 {
        out.push(((value & 0x7F) as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn read_varlen(data: &[u8], pos: &mut usize) -> Result<usize> {
    let mut total = 0usize;
    loop {
        if *pos >= data.len() {
            return err("truncated length extension in LZ55 packet");
        }
        let b = data[*pos] as usize;
        *pos += 1;
        total = total
            .checked_add(b)
            .ok_or_else(|| Pro55Error::new("LZ55 length extension overflow"))?;
        if b != 255 {
            return Ok(total);
        }
    }
}

fn read_uleb128(data: &[u8], pos: &mut usize, what: &str) -> Result<usize> {
    let mut total = 0usize;
    let mut shift = 0u32;
    loop {
        if *pos >= data.len() {
            return err(format!("truncated {what} in LZ55X packet"));
        }
        let b = data[*pos];
        *pos += 1;
        let part = usize::from(b & 0x7F)
            .checked_shl(shift)
            .ok_or_else(|| Pro55Error::new(format!("LZ55X {what} overflow")))?;
        total = total
            .checked_add(part)
            .ok_or_else(|| Pro55Error::new(format!("LZ55X {what} overflow")))?;
        if b & 0x80 == 0 {
            return Ok(total);
        }
        shift = shift
            .checked_add(7)
            .ok_or_else(|| Pro55Error::new(format!("LZ55X {what} overflow")))?;
    }
}

fn hash4(data: &[u8], pos: usize) -> u32 {
    (data[pos] as u32)
        | ((data[pos + 1] as u32) << 8)
        | ((data[pos + 2] as u32) << 16)
        | ((data[pos + 3] as u32) << 24)
}

fn match_len(data: &[u8], p: usize, q: usize, limit: usize) -> usize {
    let mut length = 0usize;
    while length < limit && data[p + length] == data[q + length] {
        length += 1;
    }
    length
}

fn level_params(level: u8) -> (usize, usize) {
    match level {
        0 | 1 => (0, 8),
        2 | 3 => (8, 4),
        4 | 5 => (16, 2),
        6 | 7 => (32, 1),
        8 => (64, 1),
        _ => (96, 1),
    }
}

fn write_lz_extension(out: &mut Vec<u8>, value: usize, lz55x: bool) {
    if lz55x {
        write_uleb128(out, value);
    } else {
        write_varlen(out, value);
    }
}

fn read_lz_extension(data: &[u8], pos: &mut usize, lz55x: bool, what: &str) -> Result<usize> {
    if lz55x {
        read_uleb128(data, pos, what)
    } else {
        read_varlen(data, pos)
    }
}

fn emit_lz_sequence_with(
    out: &mut Vec<u8>,
    literals: &[u8],
    offset: Option<usize>,
    match_length: usize,
    lz55x: bool,
) -> Result<()> {
    let lit_len = literals.len();
    match offset {
        None => {
            out.push((cmp::min(lit_len, 15) as u8) << 4);
            if lit_len >= 15 {
                write_lz_extension(out, lit_len - 15, lz55x);
            }
            out.extend_from_slice(literals);
        }
        Some(off) => {
            if !(1..=MAX_OFFSET).contains(&off) {
                return err("LZ55 offset out of range");
            }
            if match_length < MIN_MATCH {
                return err("LZ55 match too short");
            }
            let ml_code = match_length - MIN_MATCH;
            let token = ((cmp::min(lit_len, 15) as u8) << 4) | (cmp::min(ml_code, 15) as u8);
            out.push(token);
            if lit_len >= 15 {
                write_lz_extension(out, lit_len - 15, lz55x);
            }
            out.extend_from_slice(literals);
            put_u16_le(out, off as u16);
            if ml_code >= 15 {
                write_lz_extension(out, ml_code - 15, lz55x);
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
enum Bucket {
    One(usize),
    Many(Vec<usize>),
}

impl Bucket {
    fn trim(&mut self, cutoff: usize) {
        match self {
            Bucket::One(pos) if *pos < cutoff => *self = Bucket::Many(Vec::new()),
            Bucket::One(_) => {}
            Bucket::Many(positions) => {
                if positions.first().is_some_and(|&first| first < cutoff) {
                    let drop = positions.partition_point(|&candidate| candidate < cutoff);
                    if drop > 0 {
                        positions.drain(..drop);
                    }
                }
            }
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Bucket::Many(positions) if positions.is_empty())
    }

    fn push(&mut self, pos: usize) {
        match self {
            Bucket::One(first) => *self = Bucket::Many(vec![*first, pos]),
            Bucket::Many(positions) => positions.push(pos),
        }
    }

    fn trim_tail(&mut self) {
        if let Bucket::Many(positions) = self {
            if positions.len() > 256 {
                let keep_from = positions.len() - 128;
                positions.drain(..keep_from);
            }
        }
    }
}

fn lz_walk<F>(data: &[u8], level: u8, mut emit: F) -> Result<()>
where
    F: FnMut(&[u8], Option<usize>, usize) -> Result<()>,
{
    let level = validate_level(level)?;
    let n = data.len();
    if n == 0 {
        return Ok(());
    }
    if n < MIN_MATCH {
        emit(data, None, 0)?;
        return Ok(());
    }

    let (depth, insertion_stride) = level_params(level);
    if depth == 0 {
        emit(data, None, 0)?;
        return Ok(());
    }

    let mut table: HashMap<u32, Bucket> = HashMap::new();
    let mut anchor = 0usize;
    let mut pos = 0usize;
    let last_match_pos = n - MIN_MATCH;
    let mut emitted = false;

    while pos <= last_match_pos {
        let h = hash4(data, pos);
        let mut best_len = 0usize;
        let mut best_off = 0usize;
        if let Some(bucket) = table.get_mut(&h) {
            let cutoff = pos.saturating_sub(MAX_OFFSET);
            bucket.trim(cutoff);
            match bucket {
                Bucket::One(candidate) => {
                    let off = pos - *candidate;
                    if off != 0 && off <= MAX_OFFSET {
                        best_len = match_len(data, *candidate, pos, n - pos);
                        best_off = off;
                    }
                }
                Bucket::Many(candidates) => {
                    let start = candidates.len().saturating_sub(depth);
                    for &candidate in candidates[start..].iter().rev() {
                        let off = pos - candidate;
                        if off == 0 || off > MAX_OFFSET {
                            continue;
                        }
                        let current = match_len(data, candidate, pos, n - pos);
                        if current > best_len {
                            best_len = current;
                            best_off = off;
                            if current == n - pos {
                                break;
                            }
                        }
                    }
                }
            }
        }

        if best_len >= MIN_MATCH {
            emit(&data[anchor..pos], Some(best_off), best_len)?;
            emitted = true;
            let new_pos = pos + best_len;
            if new_pos <= last_match_pos {
                let future_cutoff = new_pos.saturating_sub(MAX_OFFSET);
                let insert_start = cmp::max(pos, future_cutoff);
                let skipped = insert_start.saturating_sub(pos);
                let stride_steps = skipped.div_ceil(insertion_stride);
                let mut i = pos + stride_steps * insertion_stride;
                while i < new_pos {
                    add_lz_position(&mut table, data, i, last_match_pos);
                    i += insertion_stride;
                }
            }
            pos = new_pos;
            anchor = pos;
        } else {
            add_lz_position(&mut table, data, pos, last_match_pos);
            pos += 1;
        }
    }

    if anchor < n {
        emit(&data[anchor..], None, 0)?;
    } else if !emitted {
        emit(&[], None, 0)?;
    }
    Ok(())
}

fn lz_compress(data: &[u8], level: u8, lz55x: bool) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    lz_walk(data, level, |literals, offset, match_length| {
        emit_lz_sequence_with(&mut out, literals, offset, match_length, lz55x)
    })?;
    Ok(out)
}

fn lz_compress_dual(data: &[u8], level: u8) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut lz55 = Vec::with_capacity(data.len());
    let mut lz55x = Vec::with_capacity(data.len());
    lz_walk(data, level, |literals, offset, match_length| {
        emit_lz_sequence_with(&mut lz55, literals, offset, match_length, false)?;
        emit_lz_sequence_with(&mut lz55x, literals, offset, match_length, true)
    })?;
    Ok((lz55, lz55x))
}

pub fn lz55_compress(data: &[u8], level: u8) -> Result<Vec<u8>> {
    lz_compress(data, level, false)
}

pub fn lz55x_compress(data: &[u8], level: u8) -> Result<Vec<u8>> {
    lz_compress(data, level, true)
}

fn add_lz_position(
    table: &mut HashMap<u32, Bucket>,
    data: &[u8],
    pos: usize,
    last_match_pos: usize,
) {
    if pos > last_match_pos {
        return;
    }
    let h = hash4(data, pos);
    match table.entry(h) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(Bucket::One(pos));
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            let bucket = entry.get_mut();
            bucket.trim(pos.saturating_sub(MAX_OFFSET));
            if bucket.is_empty() {
                *bucket = Bucket::One(pos);
            } else {
                bucket.push(pos);
                bucket.trim_tail();
            }
        }
    }
}

fn copy_lz_match(out: &mut Vec<u8>, offset: usize, length: usize) -> Result<()> {
    if offset == 0 || offset > out.len() {
        return err("invalid LZ55 match offset");
    }
    let target_len = out
        .len()
        .checked_add(length)
        .ok_or_else(|| Pro55Error::new("LZ55 match length overflow"))?;
    if offset == 1 {
        let b = *out
            .last()
            .ok_or_else(|| Pro55Error::new("invalid LZ55 match offset"))?;
        out.resize(target_len, b);
        return Ok(());
    }

    let source_start = out.len() - offset;
    while out.len() < target_len {
        let available = out.len() - source_start;
        let take = (target_len - out.len()).min(available);
        out.extend_from_within(source_start..source_start + take);
    }
    Ok(())
}

fn lz_decompress(packet: &[u8], expected_size: usize, lz55x: bool) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_size);
    let mut pos = 0usize;
    while pos < packet.len() {
        let token = packet[pos];
        pos += 1;

        let mut lit_len = (token >> 4) as usize;
        if lit_len == 15 {
            lit_len += read_lz_extension(packet, &mut pos, lz55x, "literal length")?;
        }
        if pos + lit_len > packet.len() {
            return err("LZ55 literal section exceeds packet size");
        }
        out.extend_from_slice(&packet[pos..pos + lit_len]);
        pos += lit_len;

        if pos >= packet.len() {
            break;
        }

        let offset = read_u16_at(packet, &mut pos, "LZ55 match offset")? as usize;

        let mut match_length = ((token & 0x0F) as usize) + MIN_MATCH;
        if (token & 0x0F) == 15 {
            match_length += read_lz_extension(packet, &mut pos, lz55x, "match length")?;
        }

        copy_lz_match(&mut out, offset, match_length)?;
        if out.len() > expected_size {
            return err("LZ55 block expanded beyond expected size");
        }
    }

    if out.len() != expected_size {
        return err(format!(
            "LZ55 block produced {} bytes, expected {expected_size}",
            out.len()
        ));
    }
    Ok(out)
}

pub fn lz55_decompress(packet: &[u8], expected_size: usize) -> Result<Vec<u8>> {
    lz_decompress(packet, expected_size, false)
}

pub fn lz55x_decompress(packet: &[u8], expected_size: usize) -> Result<Vec<u8>> {
    lz_decompress(packet, expected_size, true)
}

pub fn rle_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut pos = 0usize;
    let mut literal_start = 0usize;

    fn flush_literals(out: &mut Vec<u8>, data: &[u8], literal_start: &mut usize, end: usize) {
        while *literal_start < end {
            let take = (end - *literal_start).min(128);
            out.push((take - 1) as u8);
            out.extend_from_slice(&data[*literal_start..*literal_start + take]);
            *literal_start += take;
        }
    }

    while pos < data.len() {
        let mut run = 1usize;
        let max_run = (data.len() - pos).min(128);
        while run < max_run && data[pos + run] == data[pos] {
            run += 1;
        }
        if run >= 4 {
            flush_literals(&mut out, data, &mut literal_start, pos);
            out.push(0x80 | ((run - 1) as u8));
            out.push(data[pos]);
            pos += run;
            literal_start = pos;
        } else {
            pos += 1;
            if pos - literal_start == 128 {
                flush_literals(&mut out, data, &mut literal_start, pos);
            }
        }
    }
    flush_literals(&mut out, data, &mut literal_start, data.len());
    out
}

pub fn rle_decompress(packet: &[u8], expected_size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(expected_size);
    let mut pos = 0usize;
    while pos < packet.len() {
        let header = packet[pos];
        pos += 1;
        let count = ((header & 0x7F) as usize) + 1;
        if header & 0x80 != 0 {
            if pos >= packet.len() {
                return err("truncated RLE run");
            }
            let value = packet[pos];
            pos += 1;
            out.resize(out.len() + count, value);
        } else {
            if pos + count > packet.len() {
                return err("truncated RLE literal");
            }
            out.extend_from_slice(&packet[pos..pos + count]);
            pos += count;
        }
        if out.len() > expected_size {
            return err("RLE block expanded beyond expected size");
        }
    }
    if out.len() != expected_size {
        return err(format!(
            "RLE block produced {} bytes, expected {expected_size}",
            out.len()
        ));
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum HuffNode {
    Leaf(u8),
    Internal(usize, usize),
}

fn huffman_lengths_and_freqs(data: &[u8]) -> (Vec<u8>, [u64; 256]) {
    let mut freqs = [0u64; 256];
    for &b in data {
        freqs[b as usize] += 1;
    }
    let mut lengths = vec![0u8; 256];
    if data.is_empty() {
        return (lengths, freqs);
    }

    let symbols: Vec<(u8, u64)> = freqs
        .iter()
        .enumerate()
        .filter_map(|(sym, &freq)| (freq > 0).then_some((sym as u8, freq)))
        .collect();
    if symbols.len() == 1 {
        lengths[symbols[0].0 as usize] = 1;
        return (lengths, freqs);
    }

    let mut nodes = Vec::<HuffNode>::new();
    let mut heap = BinaryHeap::<Reverse<(u64, usize, usize)>>::new();
    let mut counter = 0usize;
    for (sym, freq) in symbols {
        let idx = nodes.len();
        nodes.push(HuffNode::Leaf(sym));
        heap.push(Reverse((freq, counter, idx)));
        counter += 1;
    }

    while heap.len() > 1 {
        let Reverse((f1, _c1, n1)) = heap.pop().unwrap();
        let Reverse((f2, _c2, n2)) = heap.pop().unwrap();
        let idx = nodes.len();
        nodes.push(HuffNode::Internal(n1, n2));
        heap.push(Reverse((f1 + f2, counter, idx)));
        counter += 1;
    }

    let root = heap.pop().unwrap().0 .2;
    fill_huffman_lengths(&nodes, root, 0, &mut lengths);
    (lengths, freqs)
}

fn fill_huffman_lengths(nodes: &[HuffNode], index: usize, depth: u8, lengths: &mut [u8]) {
    match nodes[index] {
        HuffNode::Leaf(sym) => lengths[sym as usize] = depth.max(1),
        HuffNode::Internal(left, right) => {
            let next_depth = depth.saturating_add(1);
            fill_huffman_lengths(nodes, left, next_depth, lengths);
            fill_huffman_lengths(nodes, right, next_depth, lengths);
        }
    }
}

fn canonical_codes(lengths: &[u8]) -> Result<Vec<Option<(u64, u8)>>> {
    let mut pairs: Vec<(u8, u16)> = lengths
        .iter()
        .enumerate()
        .filter_map(|(sym, &len)| (len > 0).then_some((len, sym as u16)))
        .collect();
    pairs.sort_unstable();

    let mut codes = vec![None; 256];
    let mut code = 0u64;
    let mut prev_len = 0u8;
    for (len, sym) in pairs {
        if len > 63 {
            return err(format!(
                "unsupported Huffman code length {len}; use a smaller block size"
            ));
        }
        code <<= (len - prev_len) as usize;
        codes[sym as usize] = Some((code, len));
        code = code
            .checked_add(1)
            .ok_or_else(|| Pro55Error::new("Huffman canonical code overflow"))?;
        prev_len = len;
    }
    Ok(codes)
}

struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    used: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            cur: 0,
            used: 0,
        }
    }

    fn write(&mut self, code: u64, length: u8) {
        for shift in (0..length).rev() {
            let bit = ((code >> shift) & 1) as u8;
            self.cur = (self.cur << 1) | bit;
            self.used += 1;
            if self.used == 8 {
                self.out.push(self.cur);
                self.cur = 0;
                self.used = 0;
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.used != 0 {
            self.out.push(self.cur << (8 - self.used));
        }
        self.out
    }
}

fn huff_encoded_size(freqs: &[u64; 256], lengths: &[u8]) -> Result<usize> {
    let bits = freqs
        .iter()
        .zip(lengths.iter())
        .try_fold(0u128, |total, (&freq, &len)| {
            total
                .checked_add(u128::from(freq) * u128::from(len))
                .ok_or_else(|| Pro55Error::new("Huffman encoded size overflow"))
        })?;
    let bytes = bits.div_ceil(8);
    let total = 260u128
        .checked_add(bytes)
        .ok_or_else(|| Pro55Error::new("Huffman encoded size overflow"))?;
    usize::try_from(total).map_err(|_| Pro55Error::new("Huffman encoded size overflow"))
}

fn huff_encode_if_smaller(data: &[u8], limit: usize) -> Result<Option<Vec<u8>>> {
    if data.len() > u32::MAX as usize {
        return err("Huffman input exceeds u32 length limit");
    }
    let (lengths, freqs) = huffman_lengths_and_freqs(data);
    let encoded_size = huff_encoded_size(&freqs, &lengths)?;
    if encoded_size >= limit {
        return Ok(None);
    }
    let codes = canonical_codes(&lengths)?;
    let mut writer = BitWriter::new();
    for &b in data {
        let (code, nbits) =
            codes[b as usize].ok_or_else(|| Pro55Error::new("missing Huffman code for byte"))?;
        writer.write(code, nbits);
    }
    let mut out = Vec::with_capacity(encoded_size);
    put_u32_le(&mut out, data.len() as u32);
    out.extend_from_slice(&lengths);
    out.extend_from_slice(&writer.finish());
    Ok(Some(out))
}

#[derive(Debug, Clone)]
struct DecNode {
    child: [Option<usize>; 2],
    symbol: Option<u8>,
}

fn build_huffman_decoder(lengths: &[u8]) -> Result<Vec<DecNode>> {
    if lengths.len() != 256 {
        return err("invalid Huffman length table");
    }
    let codes = canonical_codes(lengths)?;
    let mut nodes = vec![DecNode {
        child: [None, None],
        symbol: None,
    }];

    for (sym, code_info) in codes.iter().enumerate() {
        let Some((code, length)) = code_info else {
            continue;
        };
        let mut index = 0usize;
        for shift in (0..*length).rev() {
            if nodes[index].symbol.is_some() {
                return err("ambiguous Huffman code table");
            }
            let bit = ((code >> shift) & 1) as usize;
            let next = match nodes[index].child[bit] {
                Some(existing) => existing,
                None => {
                    let new_index = nodes.len();
                    nodes.push(DecNode {
                        child: [None, None],
                        symbol: None,
                    });
                    nodes[index].child[bit] = Some(new_index);
                    new_index
                }
            };
            index = next;
        }
        if nodes[index].symbol.is_some()
            || nodes[index].child[0].is_some()
            || nodes[index].child[1].is_some()
        {
            return err("duplicate or ambiguous Huffman code");
        }
        nodes[index].symbol = Some(sym as u8);
    }
    Ok(nodes)
}

fn huff_decode(packet: &[u8], expected_size: Option<usize>) -> Result<Vec<u8>> {
    if packet.len() < 260 {
        return err("truncated Huffman packet");
    }
    let mut pos = 0usize;
    let output_size = read_u32_at(packet, &mut pos, "Huffman output size")? as usize;
    if let Some(expected) = expected_size {
        if output_size != expected {
            return err(format!(
                "Huffman packet size {output_size}, expected {expected}"
            ));
        }
    }
    if output_size == 0 {
        return Ok(Vec::new());
    }

    let lengths = read_bytes(packet, &mut pos, 256, "Huffman length table")?;
    let decoder = build_huffman_decoder(lengths)?;
    if decoder.len() == 1 && decoder[0].child == [None, None] && decoder[0].symbol.is_none() {
        return err("empty Huffman tree for non-empty output");
    }

    let mut out = Vec::with_capacity(output_size);
    let mut node_index = 0usize;
    for &byte in &packet[pos..] {
        for shift in (0..8).rev() {
            let bit = ((byte >> shift) & 1) as usize;
            let next = decoder[node_index].child[bit]
                .ok_or_else(|| Pro55Error::new("invalid Huffman bitstream"))?;
            node_index = next;
            if let Some(sym) = decoder[node_index].symbol {
                out.push(sym);
                if out.len() == output_size {
                    return Ok(out);
                }
                node_index = 0;
            }
        }
    }
    err("Huffman bitstream ended before output was complete")
}

fn compress_block(data: &[u8], level: u8) -> Result<(u8, Payload<'_>)> {
    let level = validate_level(level)?;
    let mut best_method = METHOD_RAW;
    let mut best_size = data.len();
    let mut best_payload = None::<Vec<u8>>;

    fn keep_if_smaller(
        method: u8,
        payload: Vec<u8>,
        best_method: &mut u8,
        best_size: &mut usize,
        best_payload: &mut Option<Vec<u8>>,
    ) {
        if payload.len() < *best_size {
            *best_method = method;
            *best_size = payload.len();
            *best_payload = Some(payload);
        }
    }

    if level >= 1 {
        let rle = rle_compress(data);
        keep_if_smaller(
            METHOD_RLE,
            rle,
            &mut best_method,
            &mut best_size,
            &mut best_payload,
        );
    }

    let mut lz = Vec::new();
    let mut lz55x = Vec::new();
    if level >= 5 {
        (lz, lz55x) = lz_compress_dual(data, level)?;
        if lz.len() < best_size {
            best_method = METHOD_LZ55;
            best_size = lz.len();
            best_payload = Some(lz.clone());
        }
    } else if level >= 2 {
        lz = lz55_compress(data, level)?;
        if lz.len() < best_size {
            best_method = METHOD_LZ55;
            best_size = lz.len();
            best_payload = Some(lz.clone());
        }
    }

    if level >= 4 && data.len() >= 512 {
        if let Some(hraw) = huff_encode_if_smaller(data, best_size)? {
            keep_if_smaller(
                METHOD_HUFRAW,
                hraw,
                &mut best_method,
                &mut best_size,
                &mut best_payload,
            );
        }
    }

    if level >= 5 && lz.len() >= 512 {
        if let Some(hlz) = huff_encode_if_smaller(&lz, best_size)? {
            keep_if_smaller(
                METHOD_HUF_LZ55,
                hlz,
                &mut best_method,
                &mut best_size,
                &mut best_payload,
            );
        }
    }

    if level >= 5 {
        if lz55x.len() < best_size {
            best_method = METHOD_LZ55X;
            best_size = lz55x.len();
            best_payload = Some(lz55x.clone());
        }
        if lz55x.len() >= 512 {
            if let Some(hlz55x) = huff_encode_if_smaller(&lz55x, best_size)? {
                keep_if_smaller(
                    METHOD_HUF_LZ55X,
                    hlz55x,
                    &mut best_method,
                    &mut best_size,
                    &mut best_payload,
                );
            }
        }
    }

    Ok((
        best_method,
        best_payload
            .map(Payload::Owned)
            .unwrap_or(Payload::Borrowed(data)),
    ))
}

fn decompress_block(method: u8, payload: &[u8], expected_size: usize) -> Result<Vec<u8>> {
    match method {
        METHOD_RAW => {
            if payload.len() != expected_size {
                return err("raw block size mismatch");
            }
            Ok(payload.to_vec())
        }
        METHOD_RLE => rle_decompress(payload, expected_size),
        METHOD_LZ55 => lz55_decompress(payload, expected_size),
        METHOD_HUFRAW => huff_decode(payload, Some(expected_size)),
        METHOD_HUF_LZ55 => {
            let lz_packet = huff_decode(payload, None)?;
            lz55_decompress(&lz_packet, expected_size)
        }
        METHOD_LZ55X => lz55x_decompress(payload, expected_size),
        METHOD_HUF_LZ55X => {
            let lz_packet = huff_decode(payload, None)?;
            lz55x_decompress(&lz_packet, expected_size)
        }
        _ => err(format!("unknown block method {method}")),
    }
}

fn compress_indexed_block<'a>(
    index: usize,
    block: &'a [u8],
    level: u8,
) -> Result<EncodedBlock<'a>> {
    let (method, payload) = compress_block(block, level)?;
    Ok(EncodedBlock {
        index,
        method,
        raw_size: block.len(),
        payload,
        crc32: crc32(block),
    })
}

fn decompress_record(record: &BlockRecord<'_>, verify: bool) -> Result<DecodedBlock> {
    let block = decompress_block(record.method, record.payload, record.uncompressed_size)?;
    if verify && crc32(&block) != record.crc32 {
        return err(format!("block {} CRC check failed", record.index));
    }
    Ok(DecodedBlock {
        index: record.index,
        data: block,
    })
}

fn block_count(data_len: usize, block_size: usize) -> usize {
    if data_len == 0 {
        0
    } else {
        data_len.div_ceil(block_size)
    }
}

fn block_at(data: &[u8], block_size: usize, index: usize) -> &[u8] {
    let start = index * block_size;
    let end = cmp::min(start + block_size, data.len());
    &data[start..end]
}

fn write_encoded_block(out: &mut Vec<u8>, block: &EncodedBlock<'_>) -> Result<()> {
    if block.raw_size > u32::MAX as usize || block.payload.len() > u32::MAX as usize {
        return err("block size exceeds u32 limit");
    }
    out.push(block.method);
    put_u32_le(out, block.raw_size as u32);
    put_u32_le(out, block.payload.len() as u32);
    put_u32_le(out, block.crc32);
    out.extend_from_slice(block.payload.as_slice());
    Ok(())
}

fn compress_batch<'a>(
    data: &'a [u8],
    block_size: usize,
    batch_start: usize,
    batch_jobs: usize,
    level: u8,
    workers: usize,
) -> Result<Vec<EncodedBlock<'a>>> {
    if workers == 1 {
        let mut out = Vec::with_capacity(batch_jobs);
        for local_index in 0..batch_jobs {
            let index = batch_start + local_index;
            out.push(compress_indexed_block(
                index,
                block_at(data, block_size, index),
                level,
            )?);
        }
        return Ok(out);
    }

    let next = AtomicUsize::new(0);
    thread::scope(|scope| -> Result<Vec<EncodedBlock<'a>>> {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let next = &next;
            handles.push(scope.spawn(move || -> Result<Vec<EncodedBlock<'a>>> {
                let mut local = Vec::new();
                loop {
                    let local_index = next.fetch_add(1, Ordering::Relaxed);
                    if local_index >= batch_jobs {
                        break;
                    }
                    let index = batch_start + local_index;
                    local.push(compress_indexed_block(
                        index,
                        block_at(data, block_size, index),
                        level,
                    )?);
                }
                Ok(local)
            }));
        }

        let mut out = Vec::with_capacity(batch_jobs);
        for handle in handles {
            let mut part = handle
                .join()
                .map_err(|_| Pro55Error::new("compression worker thread panicked"))??;
            out.append(&mut part);
        }
        out.sort_by_key(|block| block.index);
        Ok(out)
    })
}

fn compress_blocks_into(
    data: &[u8],
    block_size: usize,
    level: u8,
    threads: usize,
    out: &mut Vec<u8>,
) -> Result<bool> {
    let jobs = block_count(data.len(), block_size);
    if jobs == 0 {
        return Ok(false);
    }
    let workers = worker_count(threads, jobs);
    let mut uses_current_format = false;

    if workers == 1 {
        for (index, block) in data.chunks(block_size).enumerate() {
            let encoded = compress_indexed_block(index, block, level)?;
            uses_current_format |= encoded.method >= METHOD_LZ55X;
            write_encoded_block(out, &encoded)?;
        }
        return Ok(uses_current_format);
    }

    let mut batch_start = 0usize;
    while batch_start < jobs {
        let batch_jobs = cmp::min(workers * WORK_BATCH_FACTOR, jobs - batch_start);
        let mut batch = compress_batch(data, block_size, batch_start, batch_jobs, level, workers)?;
        batch.sort_by_key(|block| block.index);
        for block in batch {
            uses_current_format |= block.method >= METHOD_LZ55X;
            write_encoded_block(out, &block)?;
        }
        batch_start += batch_jobs;
    }
    Ok(uses_current_format)
}

fn decode_batch(
    records: &[BlockRecord<'_>],
    verify: bool,
    workers: usize,
) -> Result<Vec<DecodedBlock>> {
    if workers == 1 {
        let mut out = Vec::with_capacity(records.len());
        for record in records {
            out.push(decompress_record(record, verify)?);
        }
        return Ok(out);
    }

    let next = AtomicUsize::new(0);
    thread::scope(|scope| -> Result<Vec<DecodedBlock>> {
        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let next = &next;
            handles.push(scope.spawn(move || -> Result<Vec<DecodedBlock>> {
                let mut local = Vec::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= records.len() {
                        break;
                    }
                    local.push(decompress_record(&records[index], verify)?);
                }
                Ok(local)
            }));
        }

        let mut out = Vec::with_capacity(records.len());
        for handle in handles {
            let mut part = handle
                .join()
                .map_err(|_| Pro55Error::new("decompression worker thread panicked"))??;
            out.append(&mut part);
        }
        out.sort_by_key(|block| block.index);
        Ok(out)
    })
}

fn append_decoded_block(
    out: &mut Vec<u8>,
    running_crc: &mut u32,
    block: DecodedBlock,
    original_size: usize,
    verify: bool,
) -> Result<()> {
    if block.data.len() > original_size.saturating_sub(out.len()) {
        return err("archive produced too much data");
    }
    if verify {
        *running_crc = crc32_with_seed(&block.data, *running_crc);
    }
    out.extend_from_slice(&block.data);
    Ok(())
}

fn decompress_records_to_output(
    records: &[BlockRecord<'_>],
    verify: bool,
    threads: usize,
    original_size: usize,
    expected_crc: u32,
) -> Result<Vec<u8>> {
    let jobs = records.len();
    let mut out = Vec::with_capacity(original_size);
    let mut running_crc = 0u32;
    if jobs == 0 {
        if verify && running_crc != expected_crc {
            return err("archive CRC check failed");
        }
        return Ok(out);
    }

    let workers = worker_count(threads, jobs);
    if workers == 1 {
        for record in records {
            let block = decompress_record(record, verify)?;
            append_decoded_block(&mut out, &mut running_crc, block, original_size, verify)?;
        }
    } else {
        for batch in records.chunks(workers * WORK_BATCH_FACTOR) {
            let batch_workers = worker_count(workers, batch.len());
            let decoded = decode_batch(batch, verify, batch_workers)?;
            for block in decoded {
                append_decoded_block(&mut out, &mut running_crc, block, original_size, verify)?;
            }
        }
    }

    if out.len() != original_size {
        return err(format!(
            "archive produced {} bytes, expected {original_size}",
            out.len()
        ));
    }
    if verify && running_crc != expected_crc {
        return err("archive CRC check failed");
    }
    Ok(out)
}

pub fn compress_bytes(
    data: &[u8],
    level: u8,
    block_size: usize,
    threads: usize,
) -> Result<Vec<u8>> {
    let level = validate_level(level)?;
    let block_size = validate_block_size(block_size)?;
    let threads = normalize_threads(threads)?;
    if data.len() > u64::MAX as usize {
        return err("input too large");
    }
    if block_size > u32::MAX as usize {
        return err("block size exceeds u32 limit");
    }

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    let version_pos = out.len();
    out.push(LEGACY_FORMAT_VERSION);
    out.push(level);
    put_u64_le(&mut out, data.len() as u64);
    put_u32_le(&mut out, block_size as u32);
    put_u32_le(&mut out, crc32(data));

    if compress_blocks_into(data, block_size, level, threads, &mut out)? {
        out[version_pos] = FORMAT_VERSION;
    }
    Ok(out)
}

pub fn compress_reader<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
    level: u8,
    block_size: usize,
    threads: usize,
) -> Result<()> {
    let mut data = Vec::new();
    reader.read_to_end(&mut data)?;
    let archive = compress_bytes(&data, level, block_size, threads)?;
    writer.write_all(&archive)?;
    Ok(())
}

fn parse_header(archive: &[u8]) -> Result<(u8, u8, u64, u32, u32, usize)> {
    let mut pos = 0usize;
    if archive.len() < HEADER_SIZE {
        return err("unexpected end of file while reading file header");
    }
    let magic = read_bytes(archive, &mut pos, MAGIC.len(), "file magic")?;
    if magic != MAGIC.as_slice() {
        return err("not a .55pro file: bad magic");
    }
    let version = *read_bytes(archive, &mut pos, 1, "version")?
        .first()
        .unwrap();
    if !matches!(version, LEGACY_FORMAT_VERSION | FORMAT_VERSION) {
        return err(format!("unsupported .55pro version {version}"));
    }
    let level = *read_bytes(archive, &mut pos, 1, "level")?.first().unwrap();
    let original_size = read_u64_at(archive, &mut pos, "original size")?;
    let block_size = read_u32_at(archive, &mut pos, "block size")?;
    let crc = read_u32_at(archive, &mut pos, "archive crc32")?;
    if !(MIN_BLOCK_SIZE as u32..=MAX_BLOCK_SIZE as u32).contains(&block_size) {
        return err("invalid block size in header");
    }
    Ok((version, level, original_size, block_size, crc, pos))
}

fn read_block_records<'a>(
    archive: &'a [u8],
    mut pos: usize,
    original_size: u64,
    block_size: usize,
) -> Result<(Vec<BlockRecord<'a>>, usize)> {
    let original_usize = usize::try_from(original_size)
        .map_err(|_| Pro55Error::new("archive too large for this platform"))?;
    let mut produced = 0usize;
    let mut records = Vec::new();
    while produced < original_usize {
        if pos + BLOCK_HEADER_SIZE > archive.len() {
            return err("unexpected end of file while reading block header");
        }
        let method = archive[pos];
        pos += 1;
        if method_name(method).is_none() {
            return err(format!("unknown block method {method}"));
        }
        let usize_raw = read_u32_at(archive, &mut pos, "block uncompressed size")? as usize;
        let csize = read_u32_at(archive, &mut pos, "block compressed size")? as usize;
        let block_crc = read_u32_at(archive, &mut pos, "block crc32")?;
        if usize_raw == 0 || usize_raw > block_size {
            return err("invalid block uncompressed size");
        }
        if csize
            > cmp::max(
                usize_raw + 1024 + 260,
                block_size.saturating_mul(2).saturating_add(1024),
            )
        {
            return err("invalid block compressed size");
        }
        if csize > archive.len().saturating_sub(pos) {
            return err("unexpected end of file while reading block payload");
        }
        let end_payload = pos + csize;
        let payload = &archive[pos..end_payload];
        pos = end_payload;
        records.push(BlockRecord {
            index: records.len(),
            method,
            uncompressed_size: usize_raw,
            crc32: block_crc,
            payload,
        });
        produced += usize_raw;
        if produced > original_usize {
            return err("archive block table exceeds original size");
        }
    }
    Ok((records, pos))
}

pub fn decompress_bytes(archive: &[u8], verify: bool, threads: usize) -> Result<Vec<u8>> {
    let threads = normalize_threads(threads)?;
    let (_version, _level, original_size, block_size, crc, pos) = parse_header(archive)?;
    let (records, end_pos) = read_block_records(archive, pos, original_size, block_size as usize)?;
    if end_pos != archive.len() {
        return err("trailing data after final .55pro block");
    }
    let original_usize = usize::try_from(original_size)
        .map_err(|_| Pro55Error::new("archive too large for this platform"))?;
    decompress_records_to_output(&records, verify, threads, original_usize, crc)
}

pub fn decompress_reader<R: Read, W: Write>(
    reader: &mut R,
    writer: Option<&mut W>,
    verify: bool,
    threads: usize,
) -> Result<Vec<u8>> {
    let mut archive = Vec::new();
    reader.read_to_end(&mut archive)?;
    let data = decompress_bytes(&archive, verify, threads)?;
    if let Some(out) = writer {
        out.write_all(&data)?;
    }
    Ok(data)
}

pub fn inspect_archive_bytes(archive: &[u8]) -> Result<ArchiveInfo> {
    let (version, level, original_size, block_size, crc, mut pos) = parse_header(archive)?;
    let original_usize = usize::try_from(original_size)
        .map_err(|_| Pro55Error::new("archive too large for this platform"))?;
    let mut produced = 0usize;
    let mut block_count = 0usize;
    let mut methods = BTreeMap::new();
    while produced < original_usize {
        if pos + BLOCK_HEADER_SIZE > archive.len() {
            return err("unexpected end of file while reading block header");
        }
        let method = archive[pos];
        pos += 1;
        let usize_raw = read_u32_at(archive, &mut pos, "block uncompressed size")? as usize;
        let csize = read_u32_at(archive, &mut pos, "block compressed size")? as usize;
        let _block_crc = read_u32_at(archive, &mut pos, "block crc32")?;
        let name = method_name(method)
            .ok_or_else(|| Pro55Error::new(format!("unknown block method {method}")))?;
        if usize_raw == 0 || usize_raw > block_size as usize {
            return err("invalid block uncompressed size");
        }
        if csize > archive.len().saturating_sub(pos) {
            return err("unexpected end of file while reading block payload");
        }
        *methods.entry(name.to_string()).or_insert(0) += 1;
        pos += csize;
        produced += usize_raw;
        if produced > original_usize {
            return err("archive block table exceeds original size");
        }
        block_count += 1;
    }
    if pos != archive.len() {
        return err("trailing data after final .55pro block");
    }
    Ok(ArchiveInfo {
        version,
        level,
        original_size,
        block_size,
        crc32: crc,
        block_count,
        compressed_size: archive.len() as u64,
        methods,
    })
}

pub fn inspect_archive_path(path: impl AsRef<Path>) -> Result<ArchiveInfo> {
    let data = fs::read(path.as_ref())?;
    inspect_archive_bytes(&data)
}

pub fn recommended_output_for_compress(input_path: &Path) -> PathBuf {
    input_path.with_file_name(format!(
        "{}.55pro",
        input_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("out")
    ))
}

pub fn recommended_output_for_decompress(input_path: &Path) -> PathBuf {
    if input_path.extension().and_then(|s| s.to_str()) == Some("55pro") {
        input_path.with_extension("")
    } else {
        input_path.with_file_name(format!(
            "{}.out",
            input_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("out")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(data: &[u8], level: u8, threads: usize) {
        let archive = compress_bytes(data, level, MIN_BLOCK_SIZE, threads).unwrap();
        let restored = decompress_bytes(&archive, true, threads).unwrap();
        assert_eq!(restored, data);
    }

    #[test]
    fn roundtrips_empty_and_small() {
        roundtrip(b"", 5, 1);
        roundtrip(b"hello 55pro", 5, 1);
    }

    #[test]
    fn roundtrips_all_levels() {
        let mut data = Vec::new();
        for _ in 0..800 {
            data.extend_from_slice(b"level behavior test data\n");
        }
        for i in 0..4096 {
            data.push((i % 251) as u8);
        }
        for level in 0..=9 {
            roundtrip(&data, level, 2);
        }
    }

    #[test]
    fn lz55_direct() {
        let data = b"mississippi ".repeat(1000);
        let packet = lz55_compress(&data, 9).unwrap();
        assert_eq!(lz55_decompress(&packet, data.len()).unwrap(), data);
    }

    #[test]
    fn lz55x_direct_long_match() {
        let data = b"A".repeat(1_000_000);
        let old_packet = lz55_compress(&data, 9).unwrap();
        let packet = lz55x_compress(&data, 9).unwrap();
        assert!(packet.len() < old_packet.len());
        assert_eq!(lz55x_decompress(&packet, data.len()).unwrap(), data);
    }

    #[test]
    fn huf_lz55x_direct() {
        let data = b"long huffman lz55x data ".repeat(20_000);
        let lz_packet = lz55x_compress(&data, 9).unwrap();
        let packet = huff_encode_if_smaller(&lz_packet, usize::MAX)
            .unwrap()
            .unwrap();
        assert_eq!(
            decompress_block(METHOD_HUF_LZ55X, &packet, data.len()).unwrap(),
            data
        );
    }

    #[test]
    fn rle_direct() {
        let mut data = b"A".repeat(300);
        data.extend_from_slice(b"BCDE");
        data.extend_from_slice(&b"Z".repeat(129));
        let packet = rle_compress(&data);
        assert_eq!(rle_decompress(&packet, data.len()).unwrap(), data);
    }

    #[test]
    fn threaded_limit() {
        assert_eq!(normalize_threads(MAX_THREADS).unwrap(), MAX_THREADS);
        assert!(normalize_threads(MAX_THREADS + 1).is_err());
    }
}
