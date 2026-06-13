use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use serde_json::Value;
use walkdir::WalkDir;
use zip::ZipArchive;

#[derive(Parser, Debug)]
#[command(
    name = "app-rename",
    version,
    about = "Rename APK/XAPK files to package_version.ext"
)]
struct Cli {
    /// APK/XAPK file or directory path(s)
    #[arg(required = true)]
    paths: Vec<PathBuf>,

    /// Preview actions without changing files
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Copy to the new name instead of renaming in place
    #[arg(short = 'c', long)]
    copy: bool,

    /// Recursively scan directories
    #[arg(short = 'r', long)]
    recursive: bool,

    /// Replace an existing destination file. Default creates name__N.ext on collision.
    #[arg(long)]
    overwrite: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppMeta {
    package: String,
    version: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppKind {
    Apk,
    Xapk,
}

impl AppKind {
    fn from_path(path: &Path) -> Option<Self> {
        match path
            .extension()?
            .to_string_lossy()
            .to_ascii_lowercase()
            .as_str()
        {
            "apk" => Some(Self::Apk),
            "xapk" => Some(Self::Xapk),
            _ => None,
        }
    }

    fn ext(self) -> &'static str {
        match self {
            Self::Apk => "apk",
            Self::Xapk => "xapk",
        }
    }
}

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let mut processed = 0usize;
    let mut failed = 0usize;

    for input in collect_inputs(&cli)? {
        match process_one(&input, &cli) {
            Ok(ProcessOutcome::Changed { from, to, copied }) => {
                processed += 1;
                let action = if cli.dry_run {
                    if copied { "would copy" } else { "would rename" }
                } else if copied {
                    "copied"
                } else {
                    "renamed"
                };
                println!("{action}: {} -> {}", from.display(), to.display());
            }
            Ok(ProcessOutcome::Skipped { path, reason }) => {
                println!("skip: {} ({reason})", path.display());
            }
            Err(err) => {
                failed += 1;
                eprintln!("fail: {}: {err:#}", input.display());
            }
        }
    }

    if failed > 0 {
        bail!("{failed} file(s) failed, {processed} file(s) processed");
    }
    Ok(())
}

fn collect_inputs(cli: &Cli) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for path in &cli.paths {
        if path.is_file() {
            if AppKind::from_path(path).is_some() {
                files.push(path.clone());
            }
            continue;
        }
        if path.is_dir() {
            if cli.recursive {
                for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
                    let p = entry.path();
                    if p.is_file() && AppKind::from_path(p).is_some() {
                        files.push(p.to_path_buf());
                    }
                }
            } else {
                for entry in
                    fs::read_dir(path).with_context(|| format!("read dir {}", path.display()))?
                {
                    let p = entry?.path();
                    if p.is_file() && AppKind::from_path(&p).is_some() {
                        files.push(p);
                    }
                }
            }
            continue;
        }
        bail!("path does not exist: {}", path.display());
    }
    Ok(files)
}

#[derive(Debug, PartialEq, Eq)]
enum ProcessOutcome {
    Changed {
        from: PathBuf,
        to: PathBuf,
        copied: bool,
    },
    Skipped {
        path: PathBuf,
        reason: String,
    },
}

fn process_one(path: &Path, cli: &Cli) -> Result<ProcessOutcome> {
    let kind = AppKind::from_path(path).ok_or_else(|| anyhow!("unsupported extension"))?;
    let meta = match kind {
        AppKind::Apk => read_apk_meta(path),
        AppKind::Xapk => read_xapk_meta(path),
    }?;

    let file_name = build_file_name(&meta, kind);
    let desired = path.with_file_name(file_name);
    let dest = if cli.overwrite {
        desired
    } else {
        unique_path(&desired)
    };

    if same_path(path, &dest) {
        return Ok(ProcessOutcome::Skipped {
            path: path.to_path_buf(),
            reason: "already named".to_string(),
        });
    }

    if cli.dry_run {
        return Ok(ProcessOutcome::Changed {
            from: path.to_path_buf(),
            to: dest,
            copied: cli.copy,
        });
    }

    if cli.copy {
        fs::copy(path, &dest).with_context(|| format!("copy to {}", dest.display()))?;
    } else {
        if cli.overwrite && dest.exists() {
            fs::remove_file(&dest)
                .with_context(|| format!("remove existing {}", dest.display()))?;
        }
        fs::rename(path, &dest).with_context(|| format!("rename to {}", dest.display()))?;
    }

    Ok(ProcessOutcome::Changed {
        from: path.to_path_buf(),
        to: dest,
        copied: cli.copy,
    })
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.file_name() == b.file_name() && a.parent() == b.parent()
}

fn unique_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("app");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");

    for n in 1.. {
        let name = if ext.is_empty() {
            format!("{stem}__{n}")
        } else {
            format!("{stem}__{n}.{ext}")
        };
        let candidate = parent.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn build_file_name(meta: &AppMeta, kind: AppKind) -> String {
    format!(
        "{}_{}.{}",
        sanitize_component(&meta.package),
        sanitize_component(&meta.version),
        kind.ext()
    )
}

fn sanitize_component(input: &str) -> String {
    let mut out = String::new();
    for ch in input.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(['.', '_', '-']).to_string();
    if trimmed.is_empty() {
        "unknown".to_string()
    } else {
        trimmed
    }
}

fn read_apk_meta(path: &Path) -> Result<AppMeta> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = ZipArchive::new(file).with_context(|| format!("open zip {}", path.display()))?;
    let mut manifest = zip
        .by_name("AndroidManifest.xml")
        .with_context(|| format!("AndroidManifest.xml not found in {}", path.display()))?;
    let mut data = Vec::new();
    manifest.read_to_end(&mut data)?;
    parse_manifest_meta(&data)
        .with_context(|| format!("parse AndroidManifest.xml in {}", path.display()))
}

fn read_xapk_meta(path: &Path) -> Result<AppMeta> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut zip = ZipArchive::new(file).with_context(|| format!("open xapk {}", path.display()))?;

    if let Ok(mut manifest) = zip.by_name("manifest.json") {
        let mut data = String::new();
        manifest.read_to_string(&mut data)?;
        if let Some(meta) = parse_xapk_manifest_json(&data)? {
            return Ok(meta);
        }
    }

    for i in 0..zip.len() {
        let mut entry = zip.by_index(i)?;
        if !entry.name().to_ascii_lowercase().ends_with(".apk") {
            continue;
        }
        let mut apk_data = Vec::new();
        entry.read_to_end(&mut apk_data)?;
        if let Ok(meta) = read_apk_meta_from_bytes(&apk_data) {
            return Ok(meta);
        }
    }

    bail!("no usable manifest.json or embedded APK found")
}

fn read_apk_meta_from_bytes(apk_data: &[u8]) -> Result<AppMeta> {
    let cursor = std::io::Cursor::new(apk_data);
    let mut zip = ZipArchive::new(cursor)?;
    let mut manifest = zip.by_name("AndroidManifest.xml")?;
    let mut data = Vec::new();
    manifest.read_to_end(&mut data)?;
    parse_manifest_meta(&data)
}

fn parse_xapk_manifest_json(data: &str) -> Result<Option<AppMeta>> {
    let value: Value = serde_json::from_str(data).context("invalid manifest.json")?;
    let package = first_string(&value, &["package_name", "package", "application_id", "id"]);
    let version = first_string(&value, &["version_name", "versionName", "version"])
        .or_else(|| first_number_or_string(&value, &["version_code", "versionCode"]));

    Ok(match (package, version) {
        (Some(package), Some(version)) => Some(AppMeta { package, version }),
        _ => None,
    })
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str().map(ToOwned::to_owned))
        .filter(|s| !s.trim().is_empty())
}

fn first_number_or_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(v) = value.get(*key) {
            if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
                return Some(s.to_string());
            }
            if let Some(n) = v.as_i64() {
                return Some(n.to_string());
            }
            if let Some(n) = v.as_u64() {
                return Some(n.to_string());
            }
        }
    }
    None
}

fn parse_manifest_meta(data: &[u8]) -> Result<AppMeta> {
    if data.first().copied() == Some(b'<') || data.iter().take(16).any(|b| *b == b'<') {
        parse_text_manifest_meta(data)
    } else {
        parse_binary_manifest_meta(data)
    }
}

fn parse_text_manifest_meta(data: &[u8]) -> Result<AppMeta> {
    let text = std::str::from_utf8(data).context("text manifest is not utf-8")?;
    let start = text.find("<manifest").context("manifest tag not found")?;
    let end = text[start..].find('>').context("manifest tag not closed")? + start;
    let tag = &text[start..=end];
    let package = extract_text_attr(tag, "package").context("package not found")?;
    let version = extract_text_attr(tag, "android:versionName")
        .or_else(|| extract_text_attr(tag, "versionName"))
        .or_else(|| extract_text_attr(tag, "android:versionCode"))
        .or_else(|| extract_text_attr(tag, "versionCode"))
        .context("versionName/versionCode not found")?;
    Ok(AppMeta { package, version })
}

fn extract_text_attr(tag: &str, attr: &str) -> Option<String> {
    let needle = format!("{attr}=");
    let idx = tag.find(&needle)? + needle.len();
    let quote = tag[idx..].chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &tag[idx + quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

const RES_STRING_POOL_TYPE: u16 = 0x0001;
const RES_XML_TYPE: u16 = 0x0003;
const RES_XML_START_ELEMENT_TYPE: u16 = 0x0102;
const UTF8_FLAG: u32 = 0x0000_0100;
const NO_INDEX: u32 = 0xffff_ffff;
const TYPE_STRING: u8 = 0x03;
const TYPE_INT_DEC: u8 = 0x10;
const TYPE_INT_HEX: u8 = 0x11;
const TYPE_INT_BOOLEAN: u8 = 0x12;

#[derive(Debug)]
struct StringPool {
    strings: Vec<String>,
}

impl StringPool {
    fn get(&self, idx: u32) -> Option<&str> {
        if idx == NO_INDEX {
            return None;
        }
        self.strings.get(idx as usize).map(String::as_str)
    }
}

fn parse_binary_manifest_meta(data: &[u8]) -> Result<AppMeta> {
    ensure_chunk(data, 0, RES_XML_TYPE)?;
    let xml_size = read_u32(data, 4)? as usize;
    if xml_size > data.len() {
        bail!("xml chunk size exceeds input");
    }

    let mut offset = read_u16(data, 2)? as usize;
    let mut strings: Option<StringPool> = None;
    while offset + 8 <= xml_size {
        let chunk_type = read_u16(data, offset)?;
        let chunk_size = read_u32(data, offset + 4)? as usize;
        if chunk_size < 8 || offset + chunk_size > xml_size {
            bail!("invalid chunk at offset {offset}");
        }

        match chunk_type {
            RES_STRING_POOL_TYPE => strings = Some(parse_string_pool(data, offset)?),
            RES_XML_START_ELEMENT_TYPE => {
                if let Some(pool) = strings.as_ref() {
                    if let Some(meta) = parse_start_element_for_manifest(data, offset, pool)? {
                        return Ok(meta);
                    }
                }
            }
            _ => {}
        }
        offset += chunk_size;
    }

    bail!("manifest element with package/version not found")
}

fn parse_start_element_for_manifest(
    data: &[u8],
    offset: usize,
    strings: &StringPool,
) -> Result<Option<AppMeta>> {
    let chunk_size = read_u32(data, offset + 4)? as usize;
    let ext = offset + 16;
    if offset + chunk_size < ext + 20 {
        bail!("truncated start element");
    }

    let tag_name_idx = read_u32(data, ext + 4)?;
    if strings.get(tag_name_idx) != Some("manifest") {
        return Ok(None);
    }

    let attr_start = read_u16(data, ext + 8)? as usize;
    let attr_size = read_u16(data, ext + 10)? as usize;
    let attr_count = read_u16(data, ext + 12)? as usize;
    if attr_size < 20 {
        bail!("invalid attribute size {attr_size}");
    }

    let attrs_offset = ext + attr_start;
    let mut package = None;
    let mut version_name = None;
    let mut version_code = None;

    for i in 0..attr_count {
        let attr = attrs_offset + i * attr_size;
        if attr + 20 > offset + chunk_size {
            bail!("attribute outside start element");
        }
        let name_idx = read_u32(data, attr + 4)?;
        let name = match strings.get(name_idx) {
            Some(name) => name,
            None => continue,
        };
        let value = read_binary_attr_value(data, attr, strings)?;
        match name {
            "package" => package = Some(value),
            "versionName" => version_name = Some(value),
            "versionCode" => version_code = Some(value),
            _ => {}
        }
    }

    let package = package.context("package attr not found")?;
    let version = version_name
        .or(version_code)
        .context("versionName/versionCode attr not found")?;
    Ok(Some(AppMeta { package, version }))
}

fn read_binary_attr_value(data: &[u8], attr: usize, strings: &StringPool) -> Result<String> {
    let raw_value = read_u32(data, attr + 8)?;
    if let Some(s) = strings.get(raw_value) {
        return Ok(s.to_string());
    }

    let data_type = *data
        .get(attr + 15)
        .context("missing typed value dataType")?;
    let value = read_u32(data, attr + 16)?;
    match data_type {
        TYPE_STRING => strings
            .get(value)
            .map(ToOwned::to_owned)
            .ok_or_else(|| anyhow!("string value index {value} out of range")),
        TYPE_INT_DEC => Ok(value.to_string()),
        TYPE_INT_HEX => Ok(format!("0x{value:x}")),
        TYPE_INT_BOOLEAN => Ok(if value == 0 { "false" } else { "true" }.to_string()),
        _ => Ok(value.to_string()),
    }
}

fn parse_string_pool(data: &[u8], offset: usize) -> Result<StringPool> {
    ensure_chunk(data, offset, RES_STRING_POOL_TYPE)?;
    let chunk_size = read_u32(data, offset + 4)? as usize;
    let string_count = read_u32(data, offset + 8)? as usize;
    let flags = read_u32(data, offset + 16)?;
    let strings_start = read_u32(data, offset + 20)? as usize;
    let is_utf8 = flags & UTF8_FLAG != 0;

    let offsets_start = offset + 28;
    let strings_base = offset + strings_start;
    if offsets_start + string_count * 4 > offset + chunk_size || strings_base > offset + chunk_size
    {
        bail!("invalid string pool bounds");
    }

    let mut strings = Vec::with_capacity(string_count);
    for i in 0..string_count {
        let rel = read_u32(data, offsets_start + i * 4)? as usize;
        let pos = strings_base + rel;
        if pos >= offset + chunk_size {
            bail!("string offset outside string pool");
        }
        let s = if is_utf8 {
            read_utf8_string(data, pos, offset + chunk_size)?
        } else {
            read_utf16_string(data, pos, offset + chunk_size)?
        };
        strings.push(s);
    }

    Ok(StringPool { strings })
}

fn read_utf8_string(data: &[u8], pos: usize, limit: usize) -> Result<String> {
    let (_, p) = read_len8(data, pos, limit)?;
    let (byte_len, p) = read_len8(data, p, limit)?;
    let end = p + byte_len;
    if end > limit || end >= data.len() {
        bail!("utf8 string outside pool");
    }
    Ok(std::str::from_utf8(&data[p..end])?.to_string())
}

fn read_len8(data: &[u8], pos: usize, limit: usize) -> Result<(usize, usize)> {
    let first = *data
        .get(pos)
        .filter(|_| pos < limit)
        .context("missing utf8 length")?;
    if first & 0x80 == 0 {
        Ok((first as usize, pos + 1))
    } else {
        let second = *data
            .get(pos + 1)
            .filter(|_| pos + 1 < limit)
            .context("missing utf8 length byte")?;
        Ok(((((first & 0x7f) as usize) << 8) | second as usize, pos + 2))
    }
}

fn read_utf16_string(data: &[u8], pos: usize, limit: usize) -> Result<String> {
    let (char_len, mut p) = read_len16(data, pos, limit)?;
    let byte_len = char_len * 2;
    if p + byte_len > limit || p + byte_len > data.len() {
        bail!("utf16 string outside pool");
    }
    let mut units = Vec::with_capacity(char_len);
    for _ in 0..char_len {
        units.push(read_u16(data, p)?);
        p += 2;
    }
    Ok(String::from_utf16(&units)?)
}

fn read_len16(data: &[u8], pos: usize, limit: usize) -> Result<(usize, usize)> {
    let first = read_u16(data, pos)?;
    if pos + 2 > limit {
        bail!("missing utf16 length");
    }
    if first & 0x8000 == 0 {
        Ok((first as usize, pos + 2))
    } else {
        let second = read_u16(data, pos + 2)?;
        if pos + 4 > limit {
            bail!("missing utf16 length word");
        }
        Ok((
            (((first & 0x7fff) as usize) << 16) | second as usize,
            pos + 4,
        ))
    }
}

fn ensure_chunk(data: &[u8], offset: usize, expected: u16) -> Result<()> {
    let actual = read_u16(data, offset)?;
    if actual != expected {
        bail!("unexpected chunk type 0x{actual:04x}, expected 0x{expected:04x}");
    }
    Ok(())
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16> {
    let bytes: [u8; 2] = data
        .get(offset..offset + 2)
        .context("unexpected end of data")?
        .try_into()
        .unwrap();
    Ok(u16::from_le_bytes(bytes))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32> {
    let bytes: [u8; 4] = data
        .get(offset..offset + 4)
        .context("unexpected end of data")?
        .try_into()
        .unwrap();
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_safe_apk_name() {
        let meta = AppMeta {
            package: "com.demo.app".into(),
            version: "1.2/3 beta".into(),
        };
        assert_eq!(
            build_file_name(&meta, AppKind::Apk),
            "com.demo.app_1.2_3_beta.apk"
        );
    }

    #[test]
    fn parses_text_manifest() {
        let xml = br#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="com.demo.app" android:versionName="1.2.3" />"#;
        assert_eq!(
            parse_manifest_meta(xml).unwrap(),
            AppMeta {
                package: "com.demo.app".into(),
                version: "1.2.3".into()
            }
        );
    }

    #[test]
    fn parses_xapk_manifest_json() {
        let json = r#"{"package_name":"com.demo.xapk","version_name":"2.0.1"}"#;
        assert_eq!(
            parse_xapk_manifest_json(json).unwrap().unwrap(),
            AppMeta {
                package: "com.demo.xapk".into(),
                version: "2.0.1".into()
            }
        );
    }

    #[test]
    fn parses_minimal_binary_manifest() {
        let data = make_binary_manifest_fixture();
        assert_eq!(
            parse_manifest_meta(&data).unwrap(),
            AppMeta {
                package: "com.example.app".into(),
                version: "1.2.3".into()
            }
        );
    }

    fn make_binary_manifest_fixture() -> Vec<u8> {
        let strings = [
            "manifest",
            "package",
            "versionName",
            "versionCode",
            "com.example.app",
            "1.2.3",
        ];
        let string_pool = make_utf8_string_pool(&strings);
        let start = make_manifest_start_element();
        let total_size = 8 + string_pool.len() + start.len();
        let mut out = Vec::new();
        push_u16(&mut out, RES_XML_TYPE);
        push_u16(&mut out, 8);
        push_u32(&mut out, total_size as u32);
        out.extend(string_pool);
        out.extend(start);
        out
    }

    fn make_utf8_string_pool(strings: &[&str]) -> Vec<u8> {
        let header_size = 28usize;
        let offsets_start = header_size;
        let strings_start = offsets_start + strings.len() * 4;
        let mut string_data = Vec::new();
        let mut offsets = Vec::new();
        for s in strings {
            offsets.push(string_data.len() as u32);
            push_len8(&mut string_data, s.chars().count());
            push_len8(&mut string_data, s.len());
            string_data.extend_from_slice(s.as_bytes());
            string_data.push(0);
        }
        while string_data.len() % 4 != 0 {
            string_data.push(0);
        }
        let chunk_size = strings_start + string_data.len();
        let mut out = Vec::new();
        push_u16(&mut out, RES_STRING_POOL_TYPE);
        push_u16(&mut out, header_size as u16);
        push_u32(&mut out, chunk_size as u32);
        push_u32(&mut out, strings.len() as u32);
        push_u32(&mut out, 0);
        push_u32(&mut out, UTF8_FLAG);
        push_u32(&mut out, strings_start as u32);
        push_u32(&mut out, 0);
        for off in offsets {
            push_u32(&mut out, off);
        }
        out.extend(string_data);
        out
    }

    fn make_manifest_start_element() -> Vec<u8> {
        let attr_count = 3usize;
        let chunk_size = 36 + attr_count * 20;
        let mut out = Vec::new();
        push_u16(&mut out, RES_XML_START_ELEMENT_TYPE);
        push_u16(&mut out, 16);
        push_u32(&mut out, chunk_size as u32);
        push_u32(&mut out, 1); // line number
        push_u32(&mut out, NO_INDEX); // comment
        push_u32(&mut out, NO_INDEX); // ns
        push_u32(&mut out, 0); // name: manifest
        push_u16(&mut out, 20); // attrStart, relative to ext
        push_u16(&mut out, 20); // attrSize
        push_u16(&mut out, attr_count as u16);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_attr(&mut out, 1, 4); // package=com.example.app
        push_attr(&mut out, 2, 5); // versionName=1.2.3
        push_int_attr(&mut out, 3, 123); // versionCode=123
        out
    }

    fn push_attr(out: &mut Vec<u8>, name_idx: u32, value_idx: u32) {
        push_u32(out, NO_INDEX); // ns
        push_u32(out, name_idx);
        push_u32(out, value_idx); // raw value
        push_u16(out, 8); // typed value size
        out.push(0); // res0
        out.push(TYPE_STRING);
        push_u32(out, value_idx);
    }

    fn push_int_attr(out: &mut Vec<u8>, name_idx: u32, value: u32) {
        push_u32(out, NO_INDEX);
        push_u32(out, name_idx);
        push_u32(out, NO_INDEX);
        push_u16(out, 8);
        out.push(0);
        out.push(TYPE_INT_DEC);
        push_u32(out, value);
    }

    fn push_len8(out: &mut Vec<u8>, len: usize) {
        assert!(len < 0x80);
        out.push(len as u8);
    }

    fn push_u16(out: &mut Vec<u8>, value: u16) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
}
