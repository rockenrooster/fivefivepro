use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::codec::{
    compress_bytes, decompress_bytes, inspect_archive_path, normalize_threads,
    recommended_output_for_compress, recommended_output_for_decompress, DEFAULT_BLOCK_SIZE,
    MAX_BLOCK_SIZE, MAX_THREADS, MIN_BLOCK_SIZE,
};
use crate::error::{Pro55Error, Result};
use crate::path_archive::{
    extract_path_archive, inspect_path_archive_payload, is_path_archive, pack_directory,
};
use crate::VERSION;

#[derive(Debug, Clone)]
struct CompressArgs {
    input: String,
    output: Option<String>,
    level: u8,
    block_size: usize,
    threads: usize,
    no_overwrite: bool,
}

#[derive(Debug, Clone)]
struct DecompressArgs {
    input: String,
    output: Option<String>,
    threads: usize,
    no_overwrite: bool,
    no_verify: bool,
}

#[derive(Debug, Clone)]
struct TestArgs {
    input: String,
    threads: usize,
}

#[derive(Debug, Clone)]
struct InfoArgs {
    input: String,
    deep: bool,
    threads: usize,
}

fn usage() -> &'static str {
    "55pro Rust compressor\n\nUsage:\n  55pro compress|c <input> [output.55pro] [options]\n  55pro decompress|d|x <input.55pro> [output] [options]\n  55pro test|t <input.55pro> [options]\n  55pro info|i <input.55pro> [--deep] [options]\n\nCommon options:\n  -T, --threads <0-1024|auto>   Worker threads; default 0/auto uses CPU count\n\nCompress options:\n  -l, --level <0-9>             Compression level, default 5\n  -b, --block-size <size>       Block size, default 1m; try 4m for repetitive data\n  -f, --force                   Accepted for compatibility; overwrite is default\n      --no-overwrite            Refuse to replace an existing output\n\nDecompress options:\n  -f, --force                   Accepted for compatibility; overwrite is default\n      --no-overwrite            Refuse to replace existing output/merge directory\n      --no-verify               Skip outer CRC verification\n\nOther:\n  -h, --help                    Show this help\n  -V, --version                 Show version"
}

fn command_help(command: &str) -> &'static str {
    match command {
        "compress" | "c" => "Usage: 55pro compress <input> [output.55pro] [-l 0-9] [-b 1m] [-T 0-1024|auto] [--no-overwrite]",
        "decompress" | "d" | "x" => "Usage: 55pro decompress <input.55pro> [output] [-T 0-1024|auto] [--no-verify] [--no-overwrite]",
        "test" | "t" => "Usage: 55pro test <input.55pro> [-T 0-1024|auto]",
        "info" | "i" => "Usage: 55pro info <input.55pro> [--deep] [-T 0-1024|auto]",
        _ => usage(),
    }
}

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Pro55Error::new(message))
}

fn take_value(args: &[String], i: &mut usize, opt: &str) -> Result<String> {
    *i += 1;
    if *i >= args.len() {
        return err(format!("missing value for {opt}"));
    }
    Ok(args[*i].clone())
}

fn split_long_value(arg: &str, name: &str) -> Option<String> {
    arg.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('='))
        .map(ToOwned::to_owned)
}

fn parse_level(text: &str) -> Result<u8> {
    let level: u8 = text.parse()?;
    if level <= 9 {
        Ok(level)
    } else {
        err("compression level must be 0..9")
    }
}

fn parse_threads(text: &str) -> Result<usize> {
    let lower = text.trim().to_ascii_lowercase();
    let value = if matches!(lower.as_str(), "auto" | "cpu" | "cpus") {
        0
    } else {
        lower.parse::<usize>()?
    };
    if value <= MAX_THREADS {
        Ok(value)
    } else {
        err(format!("threads must be 0..{MAX_THREADS}; 0 means auto"))
    }
}

fn parse_size(text: &str) -> Result<usize> {
    let lower = text.trim().to_ascii_lowercase().replace('_', "");
    let suffixes = [
        ("kib", 1024.0),
        ("ki", 1024.0),
        ("kb", 1024.0),
        ("k", 1024.0),
        ("mib", 1024.0 * 1024.0),
        ("mi", 1024.0 * 1024.0),
        ("mb", 1024.0 * 1024.0),
        ("m", 1024.0 * 1024.0),
    ];
    let mut parsed = None;
    for (suffix, multiplier) in suffixes {
        if let Some(number) = lower.strip_suffix(suffix) {
            let value: f64 = number
                .parse()
                .map_err(|_| Pro55Error::new(format!("invalid size: {text}")))?;
            parsed = Some((value * multiplier) as usize);
            break;
        }
    }
    let size = match parsed {
        Some(size) => size,
        None => lower.parse::<usize>()?,
    };
    if (MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&size) {
        Ok(size)
    } else {
        err(format!(
            "block size must be between {} and {}",
            human_size(MIN_BLOCK_SIZE as u64),
            human_size(MAX_BLOCK_SIZE as u64)
        ))
    }
}

fn human_size(num: u64) -> String {
    let mut value = num as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB"] {
        if value < 1024.0 || unit == "TiB" {
            if unit == "B" {
                return format!("{num} B");
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{num} B")
}

fn human_rate(bytes: u64, elapsed: Duration) -> String {
    let seconds = elapsed.as_secs_f64().max(0.000_001);
    format!("{}/s", human_size((bytes as f64 / seconds) as u64))
}

fn parse_compress(args: &[String]) -> Result<CompressArgs> {
    let mut level = 5u8;
    let mut block_size = DEFAULT_BLOCK_SIZE;
    let mut threads = 0usize;
    let mut no_overwrite = false;
    let mut positional = Vec::<String>::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if matches!(arg.as_str(), "-h" | "--help") {
            println!("{}", command_help("compress"));
            return err("help requested");
        } else if arg == "-l" || arg == "--level" {
            level = parse_level(&take_value(args, &mut i, arg)?)?;
        } else if let Some(value) = split_long_value(arg, "--level") {
            level = parse_level(&value)?;
        } else if arg == "-b" || arg == "--block-size" {
            block_size = parse_size(&take_value(args, &mut i, arg)?)?;
        } else if let Some(value) = split_long_value(arg, "--block-size") {
            block_size = parse_size(&value)?;
        } else if arg == "-T" || arg == "--threads" {
            threads = parse_threads(&take_value(args, &mut i, arg)?)?;
        } else if let Some(value) = split_long_value(arg, "--threads") {
            threads = parse_threads(&value)?;
        } else if arg == "-f" || arg == "--force" {
            // Compatibility flag. Overwriting regular files is already default.
        } else if arg == "--no-overwrite" {
            no_overwrite = true;
        } else if arg.starts_with('-') && arg != "-" {
            return err(format!("unknown compress option: {arg}"));
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }
    if positional.is_empty() || positional.len() > 2 {
        return err(command_help("compress"));
    }
    Ok(CompressArgs {
        input: positional[0].clone(),
        output: positional.get(1).cloned(),
        level,
        block_size,
        threads,
        no_overwrite,
    })
}

fn parse_decompress(args: &[String]) -> Result<DecompressArgs> {
    let mut threads = 0usize;
    let mut no_overwrite = false;
    let mut no_verify = false;
    let mut positional = Vec::<String>::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if matches!(arg.as_str(), "-h" | "--help") {
            println!("{}", command_help("decompress"));
            return err("help requested");
        } else if arg == "-T" || arg == "--threads" {
            threads = parse_threads(&take_value(args, &mut i, arg)?)?;
        } else if let Some(value) = split_long_value(arg, "--threads") {
            threads = parse_threads(&value)?;
        } else if arg == "--no-verify" {
            no_verify = true;
        } else if arg == "-f" || arg == "--force" {
            // Compatibility flag. Overwriting regular files is already default.
        } else if arg == "--no-overwrite" {
            no_overwrite = true;
        } else if arg.starts_with('-') && arg != "-" {
            return err(format!("unknown decompress option: {arg}"));
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }
    if positional.is_empty() || positional.len() > 2 {
        return err(command_help("decompress"));
    }
    Ok(DecompressArgs {
        input: positional[0].clone(),
        output: positional.get(1).cloned(),
        threads,
        no_overwrite,
        no_verify,
    })
}

fn parse_test(args: &[String]) -> Result<TestArgs> {
    let mut threads = 0usize;
    let mut positional = Vec::<String>::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if matches!(arg.as_str(), "-h" | "--help") {
            println!("{}", command_help("test"));
            return err("help requested");
        } else if arg == "-T" || arg == "--threads" {
            threads = parse_threads(&take_value(args, &mut i, arg)?)?;
        } else if let Some(value) = split_long_value(arg, "--threads") {
            threads = parse_threads(&value)?;
        } else if arg.starts_with('-') && arg != "-" {
            return err(format!("unknown test option: {arg}"));
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }
    if positional.len() != 1 {
        return err(command_help("test"));
    }
    Ok(TestArgs {
        input: positional[0].clone(),
        threads,
    })
}

fn parse_info(args: &[String]) -> Result<InfoArgs> {
    let mut threads = 0usize;
    let mut deep = false;
    let mut positional = Vec::<String>::new();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        if matches!(arg.as_str(), "-h" | "--help") {
            println!("{}", command_help("info"));
            return err("help requested");
        } else if arg == "--deep" {
            deep = true;
        } else if arg == "-T" || arg == "--threads" {
            threads = parse_threads(&take_value(args, &mut i, arg)?)?;
        } else if let Some(value) = split_long_value(arg, "--threads") {
            threads = parse_threads(&value)?;
        } else if arg.starts_with('-') && arg != "-" {
            return err(format!("unknown info option: {arg}"));
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }
    if positional.len() != 1 {
        return err(command_help("info"));
    }
    Ok(InfoArgs {
        input: positional[0].clone(),
        deep,
        threads,
    })
}

fn read_input(path: &str) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    if path == "-" {
        io::stdin().lock().read_to_end(&mut data)?;
    } else {
        data = fs::read(path)?;
    }
    Ok(data)
}

fn parent_or_current(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn atomic_write_file(path: &Path, data: &[u8], overwrite: bool) -> Result<()> {
    if path.exists() {
        let meta = fs::symlink_metadata(path)?;
        if meta.file_type().is_symlink() {
            return err(format!("refusing to overwrite symlink: {}", path.display()));
        }
        if meta.is_dir() {
            return err(format!("output path is a directory: {}", path.display()));
        }
        if !overwrite {
            return err(format!("output exists: {}", path.display()));
        }
    }
    let parent = parent_or_current(path);
    if !parent.exists() {
        return err(format!(
            "output directory does not exist: {}",
            parent.display()
        ));
    }
    let stem = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000u32 {
        let tmp = parent.join(format!(".{stem}.{pid}.{nanos}.{attempt}.tmp"));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(data).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(&tmp);
                    return Err(Pro55Error::from(err));
                }
                match fs::rename(&tmp, path) {
                    Ok(()) => return Ok(()),
                    Err(_err) if overwrite && path.exists() => {
                        let _ = fs::remove_file(path);
                        fs::rename(&tmp, path).map_err(|rename_err| {
                            let _ = fs::remove_file(&tmp);
                            Pro55Error::from(rename_err)
                        })?;
                        return Ok(());
                    }
                    Err(err) => {
                        let _ = fs::remove_file(&tmp);
                        return Err(Pro55Error::from(err));
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(Pro55Error::from(err)),
        }
    }
    err("could not allocate temporary output file")
}

fn write_output(path: &str, data: &[u8], overwrite: bool) -> Result<()> {
    if path == "-" {
        io::stdout().lock().write_all(data)?;
        Ok(())
    } else {
        atomic_write_file(Path::new(path), data, overwrite)
    }
}

fn assert_not_same_file(input: &str, output: &str) -> Result<()> {
    if input == "-" || output == "-" {
        return Ok(());
    }
    let input_path = Path::new(input);
    let output_path = Path::new(output);
    if input_path.exists()
        && output_path.exists()
        && input_path.canonicalize()? == output_path.canonicalize()?
    {
        return err("input and output paths are the same; refusing to overwrite the input");
    }
    Ok(())
}

fn assert_archive_not_inside_input_dir(input_dir: &Path, output: &str) -> Result<()> {
    if output == "-" {
        return Ok(());
    }
    let root = input_dir.canonicalize()?;
    let output_path = Path::new(output);
    let parent = parent_or_current(output_path).canonicalize()?;
    let candidate = parent.join(output_path.file_name().unwrap_or_default());
    if candidate.starts_with(&root) {
        err("refusing to create the .55pro archive inside the directory being compressed")
    } else {
        Ok(())
    }
}

fn output_for_compress(input: &str, output: Option<String>) -> String {
    output.unwrap_or_else(|| {
        if input == "-" {
            "-".to_string()
        } else {
            recommended_output_for_compress(Path::new(input))
                .to_string_lossy()
                .into_owned()
        }
    })
}

fn output_for_decompress(input: &str, output: Option<String>) -> String {
    output.unwrap_or_else(|| {
        if input == "-" {
            "-".to_string()
        } else {
            recommended_output_for_decompress(Path::new(input))
                .to_string_lossy()
                .into_owned()
        }
    })
}

fn cmd_compress(args: CompressArgs) -> Result<()> {
    let started = Instant::now();
    let threads = normalize_threads(args.threads)?;
    let output = output_for_compress(&args.input, args.output.clone());
    let overwrite = !args.no_overwrite;
    assert_not_same_file(&args.input, &output)?;

    if args.input != "-" && Path::new(&args.input).is_dir() {
        assert_archive_not_inside_input_dir(Path::new(&args.input), &output)?;
        let payload = pack_directory(&args.input)?;
        let archive = compress_bytes(&payload, args.level, args.block_size, threads)?;
        write_output(&output, &archive, overwrite)?;
        if output != "-" {
            let info = inspect_path_archive_payload(&payload)?;
            let dst = fs::metadata(&output)?.len();
            let elapsed = started.elapsed();
            println!(
                "compressed directory {} -> {} ({} files, {} dirs, {} file data -> {}, level {}, threads {}, {:.2}s, {})",
                args.input,
                output,
                info.files,
                info.directories,
                human_size(info.total_size),
                human_size(dst),
                args.level,
                threads,
                elapsed.as_secs_f64(),
                human_rate(info.total_size, elapsed)
            );
        }
        return Ok(());
    }

    let data = read_input(&args.input)?;
    let archive = compress_bytes(&data, args.level, args.block_size, threads)?;
    write_output(&output, &archive, overwrite)?;
    if output != "-" && args.input != "-" {
        let src = fs::metadata(&args.input)?.len();
        let dst = fs::metadata(&output)?.len();
        let ratio = if src == 0 {
            0.0
        } else {
            dst as f64 / src as f64
        };
        let elapsed = started.elapsed();
        println!(
            "compressed {} -> {} ({} -> {}, ratio {:.3}, level {}, threads {}, {:.2}s, {})",
            args.input,
            output,
            human_size(src),
            human_size(dst),
            ratio,
            args.level,
            threads,
            elapsed.as_secs_f64(),
            human_rate(src, elapsed)
        );
    }
    Ok(())
}

fn cmd_decompress(args: DecompressArgs) -> Result<()> {
    let started = Instant::now();
    let threads = normalize_threads(args.threads)?;
    let overwrite = !args.no_overwrite;
    let archive = read_input(&args.input)?;
    let payload = decompress_bytes(&archive, !args.no_verify, threads)?;

    if is_path_archive(&payload) {
        inspect_path_archive_payload(&payload)?;
        let Some(output) = args.output.clone().or_else(|| {
            if args.input == "-" {
                None
            } else {
                Some(
                    recommended_output_for_decompress(Path::new(&args.input))
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        }) else {
            return err("directory archive from stdin requires an explicit output directory");
        };
        if output == "-" {
            return err("directory archives cannot be extracted to stdout");
        }
        let info = extract_path_archive(&payload, &output, overwrite)?;
        let elapsed = started.elapsed();
        println!(
            "extracted directory archive {} -> {} ({} files, {} dirs, {} file data, threads {}, {:.2}s, {})",
            args.input,
            output,
            info.files,
            info.directories,
            human_size(info.total_size),
            threads,
            elapsed.as_secs_f64(),
            human_rate(info.total_size, elapsed)
        );
        return Ok(());
    }

    let output = output_for_decompress(&args.input, args.output.clone());
    assert_not_same_file(&args.input, &output)?;
    write_output(&output, &payload, overwrite)?;
    if output != "-" && args.input != "-" {
        let elapsed = started.elapsed();
        println!(
            "decompressed {} -> {} ({} -> {}, threads {}, {:.2}s, {})",
            args.input,
            output,
            human_size(archive.len() as u64),
            human_size(payload.len() as u64),
            threads,
            elapsed.as_secs_f64(),
            human_rate(payload.len() as u64, elapsed)
        );
    }
    Ok(())
}

fn cmd_test(args: TestArgs) -> Result<()> {
    let started = Instant::now();
    let threads = normalize_threads(args.threads)?;
    let archive = fs::read(&args.input)?;
    let payload = decompress_bytes(&archive, true, threads)?;
    let elapsed = started.elapsed();
    if is_path_archive(&payload) {
        let info = inspect_path_archive_payload(&payload)?;
        println!(
            "ok: {} (directory archive: {} files, {} dirs, threads {}, {:.2}s, {})",
            args.input,
            info.files,
            info.directories,
            threads,
            elapsed.as_secs_f64(),
            human_rate(info.total_size, elapsed)
        );
    } else {
        println!(
            "ok: {} ({} payload, threads {}, {:.2}s, {})",
            args.input,
            human_size(payload.len() as u64),
            threads,
            elapsed.as_secs_f64(),
            human_rate(payload.len() as u64, elapsed)
        );
    }
    Ok(())
}

fn cmd_info(args: InfoArgs) -> Result<()> {
    let info = inspect_archive_path(&args.input)?;
    let method_text = if info.methods.is_empty() {
        "none".to_string()
    } else {
        info.methods
            .iter()
            .map(|(name, count)| format!("{name}:{count}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let ratio = if info.original_size == 0 {
        0.0
    } else {
        info.compressed_size as f64 / info.original_size as f64
    };
    println!("file:            {}", args.input);
    println!("format version:  {}", info.version);
    println!("level:           {}", info.level);
    println!(
        "original size:   {} ({})",
        info.original_size,
        human_size(info.original_size)
    );
    println!(
        "archive size:    {} ({})",
        info.compressed_size,
        human_size(info.compressed_size)
    );
    println!("ratio:           {:.3}", ratio);
    println!(
        "block size:      {} ({})",
        info.block_size,
        human_size(u64::from(info.block_size))
    );
    println!("blocks:          {}", info.block_count);
    println!("methods:         {}", method_text);
    println!("crc32:           {:08x}", info.crc32);
    if args.deep {
        let threads = normalize_threads(args.threads)?;
        let payload = decompress_bytes(&fs::read(&args.input)?, true, threads)?;
        if is_path_archive(&payload) {
            let pinfo = inspect_path_archive_payload(&payload)?;
            println!("payload:         directory archive");
            println!("payload files:   {}", pinfo.files);
            println!("payload dirs:    {}", pinfo.directories);
            println!(
                "payload data:    {} ({})",
                pinfo.total_size,
                human_size(pinfo.total_size)
            );
        } else {
            println!("payload:         file byte stream");
        }
        println!("deep threads:    {}", threads);
    }
    Ok(())
}

fn run_result(argv: Vec<String>) -> Result<i32> {
    let mut args = argv.into_iter().collect::<Vec<_>>();
    if !args.is_empty() {
        args.remove(0);
    }
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help") {
        println!("{}", usage());
        return Ok(0);
    }
    if matches!(args[0].as_str(), "-V" | "--version" | "version") {
        println!("55pro {VERSION}");
        return Ok(0);
    }

    let command = args.remove(0);
    match command.as_str() {
        "compress" | "c" => cmd_compress(parse_compress(&args)?)?,
        "decompress" | "d" | "x" | "extract" => cmd_decompress(parse_decompress(&args)?)?,
        "test" | "t" => cmd_test(parse_test(&args)?)?,
        "info" | "i" => cmd_info(parse_info(&args)?)?,
        other => return err(format!("unknown command: {other}\n\n{}", usage())),
    }
    Ok(0)
}

pub fn run(argv: Vec<String>) -> i32 {
    match run_result(argv) {
        Ok(code) => code,
        Err(err) if err.message() == "help requested" => 0,
        Err(err) => {
            eprintln!("55pro: error: {err}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    #[test]
    fn thread_values_accept_auto_aliases_and_limit() {
        assert_eq!(parse_threads("0").unwrap(), 0);
        assert_eq!(parse_threads("auto").unwrap(), 0);
        assert_eq!(parse_threads("cpu").unwrap(), 0);
        assert_eq!(parse_threads("cpus").unwrap(), 0);
        assert_eq!(parse_threads("1024").unwrap(), MAX_THREADS);
        assert!(parse_threads("1025").is_err());
    }

    #[test]
    fn command_parsers_default_to_auto_threads() {
        assert_eq!(parse_compress(&args(&["input.bin"])).unwrap().threads, 0);
        assert_eq!(
            parse_decompress(&args(&["input.bin.55pro"]))
                .unwrap()
                .threads,
            0
        );
        assert_eq!(parse_test(&args(&["input.bin.55pro"])).unwrap().threads, 0);
        assert_eq!(parse_info(&args(&["input.bin.55pro"])).unwrap().threads, 0);
    }
}
