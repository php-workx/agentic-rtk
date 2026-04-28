//! Claude Code session transcript compaction.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

use crate::core::constants::RTK_DATA_DIR;
use crate::core::tracking::Tracker;

const MIN_AUTO_BYTES: usize = 64 * 1024;
const MIN_AUTO_SAVINGS_PCT: f64 = 1.0;
const HOOK_STDIN_CAP: usize = 1_048_576;
const LOCK_STALE_SECS: u64 = 600;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompactStats {
    pub records_read: usize,
    pub records_written: usize,
    pub bytes_in: usize,
    pub bytes_out: usize,
    pub read_results_deduped: usize,
    pub bash_results_recompressed: usize,
}

impl CompactStats {
    pub fn percent_saved(&self) -> f64 {
        if self.bytes_in == 0 {
            return 0.0;
        }
        ((self.bytes_in - self.bytes_out.min(self.bytes_in)) as f64 / self.bytes_in as f64) * 100.0
    }

    pub fn changed(&self) -> bool {
        self.read_results_deduped > 0 || self.bash_results_recompressed > 0
    }
}

#[derive(Debug, Clone)]
pub struct CompactOutcome {
    pub session_path: PathBuf,
    pub sidecar_path: Option<PathBuf>,
    pub backup_path: Option<PathBuf>,
    pub stats: CompactStats,
    pub applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    Manual,
    Auto,
}

pub fn run_overview(verbose: u8) -> Result<()> {
    crate::analytics::session_cmd::run(verbose)
}

pub fn run_compact(
    target: Option<&str>,
    all: bool,
    dry_run: bool,
    apply: bool,
    older_than: Option<&str>,
    verbose: u8,
) -> Result<()> {
    if all {
        let min_age = older_than.map(parse_duration).transpose()?;
        let mut done = 0usize;
        let mut skipped = 0usize;
        let mut total_in = 0usize;
        let mut total_out = 0usize;
        for path in discover_session_files()? {
            if let Some(age) = min_age {
                if !is_older_than(&path, age) {
                    skipped += 1;
                    continue;
                }
            }
            match compact_path(&path, dry_run, apply, ApplyMode::Manual, verbose) {
                Ok(outcome) => {
                    done += 1;
                    total_in += outcome.stats.bytes_in;
                    total_out += outcome.stats.bytes_out;
                    print_outcome(&outcome, dry_run);
                }
                Err(e) => {
                    skipped += 1;
                    eprintln!("[rtk session] skip {}: {}", path.display(), e);
                }
            }
        }
        let pct = if total_in == 0 {
            0.0
        } else {
            ((total_in - total_out.min(total_in)) as f64 / total_in as f64) * 100.0
        };
        println!(
            "session compact --all: {} compacted, {} skipped, {} -> {} bytes ({:.1}% saved)",
            done, skipped, total_in, total_out, pct
        );
        return Ok(());
    }

    let target = target.context("session target required unless --all is set")?;
    let path = resolve_session_path(target)?;
    let outcome = compact_path(&path, dry_run, apply, ApplyMode::Manual, verbose)?;
    print_outcome(&outcome, dry_run);
    Ok(())
}

pub fn run_apply(target: &str, verbose: u8) -> Result<()> {
    let session_path = resolve_session_path(target)?;
    let sidecar = sidecar_path(&session_path);
    if !sidecar.is_file() {
        bail!(
            "No sidecar at {}. Run `rtk session compact {target}` first.",
            sidecar.display()
        );
    }
    let compressed = fs::read_to_string(&sidecar)
        .with_context(|| format!("Failed to read sidecar {}", sidecar.display()))?;
    let original = fs::read_to_string(&session_path)
        .with_context(|| format!("Failed to read session {}", session_path.display()))?;
    let stats = validate_compacted_pair(&original, &compressed)?;
    let backup = apply_compacted(
        &session_path,
        compressed.as_bytes(),
        &stats,
        ApplyMode::Manual,
    )?;
    if verbose > 0 {
        eprintln!("rtk session apply: {}", session_path.display());
    }
    println!(
        "session apply: {} active; backup {}",
        session_path.display(),
        backup.display()
    );
    Ok(())
}

pub fn run_expand(target: &str, backup: Option<&str>, latest: bool, _verbose: u8) -> Result<()> {
    let session_path = resolve_session_path(target)?;
    let backup_path = match backup {
        Some(path) => PathBuf::from(path),
        None => latest_backup_for(&session_path)?,
    };
    if !latest && backup.is_none() {
        // Default to latest for ergonomics; the flag is accepted for explicit scripts.
    }
    if !backup_path.is_file() {
        bail!("Backup not found: {}", backup_path.display());
    }
    let current = fs::read(&session_path).unwrap_or_default();
    if !current.is_empty() {
        fs::write(sidecar_path(&session_path), current)?;
    }
    fs::copy(&backup_path, &session_path).with_context(|| {
        format!(
            "Failed to restore {} to {}",
            backup_path.display(),
            session_path.display()
        )
    })?;
    println!(
        "session expand: restored {} from {}",
        session_path.display(),
        backup_path.display()
    );
    Ok(())
}

pub fn run_status(target: &str) -> Result<()> {
    let session_path = resolve_session_path(target)?;
    let raw = fs::read_to_string(&session_path)
        .with_context(|| format!("Failed to read {}", session_path.display()))?;
    let (_out, stats) = compact_session_str(&raw);
    println!("session: {}", session_path.display());
    println!(
        "records: {}, bytes: {} -> {} ({:.1}% saved)",
        stats.records_read,
        stats.bytes_in,
        stats.bytes_out,
        stats.percent_saved()
    );
    println!(
        "axes: ReadDedup={}, BashHistoryCompact={}",
        stats.read_results_deduped, stats.bash_results_recompressed
    );
    if let Ok(backup) = latest_backup_for(&session_path) {
        println!("latest backup: {}", backup.display());
    }
    Ok(())
}

pub fn run_hook() -> Result<()> {
    let input = read_stdin_limited()?;
    let Ok(v) = serde_json::from_str::<Value>(input.trim()) else {
        return Ok(());
    };
    if v.get("hook_event_name").and_then(Value::as_str) != Some("SessionEnd") {
        return Ok(());
    }
    let Some(path) = v.get("transcript_path").and_then(Value::as_str) else {
        return Ok(());
    };
    let path = expand_tilde(path);
    if !path.is_file() {
        return Ok(());
    }

    let Some(_guard) = LockGuard::acquire(&path)? else {
        return Ok(());
    };
    if !wait_for_stable_file(&path, Duration::from_millis(250), 8) {
        return Ok(());
    }

    match compact_path(&path, false, true, ApplyMode::Auto, 0) {
        Ok(_) => {}
        Err(e) => {
            let _ = writeln_no_panic(format_args!("[rtk session] auto-compaction skipped: {e}"));
        }
    }
    Ok(())
}

fn compact_path(
    session_path: &Path,
    dry_run: bool,
    apply: bool,
    mode: ApplyMode,
    verbose: u8,
) -> Result<CompactOutcome> {
    let raw = fs::read_to_string(session_path)
        .with_context(|| format!("Failed to read session {}", session_path.display()))?;
    let (compressed, stats) = compact_session_str(&raw);

    if dry_run {
        return Ok(CompactOutcome {
            session_path: session_path.to_path_buf(),
            sidecar_path: None,
            backup_path: None,
            stats,
            applied: false,
        });
    }

    if mode == ApplyMode::Auto
        && (stats.bytes_in < MIN_AUTO_BYTES
            || stats.percent_saved() < MIN_AUTO_SAVINGS_PCT
            || !stats.changed())
    {
        return Ok(CompactOutcome {
            session_path: session_path.to_path_buf(),
            sidecar_path: None,
            backup_path: None,
            stats,
            applied: false,
        });
    }

    validate_compacted_pair(&raw, &compressed)?;

    if apply {
        let backup = apply_compacted(session_path, compressed.as_bytes(), &stats, mode)?;
        if verbose > 0 {
            eprintln!("rtk session compact --apply: {}", session_path.display());
        }
        return Ok(CompactOutcome {
            session_path: session_path.to_path_buf(),
            sidecar_path: None,
            backup_path: Some(backup),
            stats,
            applied: true,
        });
    }

    let sidecar = sidecar_path(session_path);
    fs::write(&sidecar, compressed)
        .with_context(|| format!("Failed to write sidecar {}", sidecar.display()))?;
    Ok(CompactOutcome {
        session_path: session_path.to_path_buf(),
        sidecar_path: Some(sidecar),
        backup_path: None,
        stats,
        applied: false,
    })
}

fn print_outcome(outcome: &CompactOutcome, dry_run: bool) {
    let action = if dry_run {
        "session compact (dry-run)"
    } else if outcome.applied {
        "session compact --apply"
    } else {
        "session compact"
    };
    println!(
        "{}: {}\n  records: {}, bytes: {} -> {} ({:.1}% saved)\n  axes: ReadDedup={}, BashHistoryCompact={}",
        action,
        outcome.session_path.display(),
        outcome.stats.records_written,
        outcome.stats.bytes_in,
        outcome.stats.bytes_out,
        outcome.stats.percent_saved(),
        outcome.stats.read_results_deduped,
        outcome.stats.bash_results_recompressed
    );
    if let Some(path) = &outcome.sidecar_path {
        println!("  sidecar: {}", path.display());
    }
    if let Some(path) = &outcome.backup_path {
        println!("  backup: {}", path.display());
    }
}

pub fn compact_session_str(input: &str) -> (String, CompactStats) {
    let mut stats = CompactStats {
        bytes_in: input.len(),
        ..Default::default()
    };
    let mut tool_use_index = HashMap::new();
    let mut first_read_for = HashMap::new();

    for line in input.lines() {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            index_record(&record, &mut tool_use_index, &mut first_read_for);
        }
    }

    let mut out = String::with_capacity(input.len());
    for line in input.lines() {
        stats.records_read += 1;
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }

        let mut record: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                out.push_str(line);
                out.push('\n');
                stats.records_written += 1;
                continue;
            }
        };

        rewrite_record(&mut record, &tool_use_index, &first_read_for, &mut stats);
        let written = serde_json::to_string(&record).unwrap_or_else(|_| line.to_string());
        out.push_str(&written);
        out.push('\n');
        stats.records_written += 1;
    }

    stats.bytes_out = out.len();
    (out, stats)
}

#[derive(Debug, Clone)]
struct ToolUseInfo {
    name: String,
    file_path: Option<String>,
}

#[derive(Debug, Clone)]
struct FirstRead {
    tool_use_id: String,
}

fn index_record(
    record: &Value,
    tool_use_index: &mut HashMap<String, ToolUseInfo>,
    first_read_for: &mut HashMap<String, FirstRead>,
) {
    let Some(content) = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
    else {
        return;
    };

    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            continue;
        };
        let name = block
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let file_path = block
            .pointer("/input/file_path")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        if name == "Read" {
            if let Some(path) = &file_path {
                first_read_for
                    .entry(path.clone())
                    .or_insert_with(|| FirstRead {
                        tool_use_id: id.to_string(),
                    });
            }
        }

        tool_use_index.insert(id.to_string(), ToolUseInfo { name, file_path });
    }
}

fn rewrite_record(
    record: &mut Value,
    tool_use_index: &HashMap<String, ToolUseInfo>,
    first_read_for: &HashMap<String, FirstRead>,
    stats: &mut CompactStats,
) {
    let Some(content) = record
        .get_mut("message")
        .and_then(|m| m.get_mut("content"))
        .and_then(Value::as_array_mut)
    else {
        return;
    };

    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(use_id) = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let Some(info) = tool_use_index.get(&use_id) else {
            continue;
        };

        match info.name.as_str() {
            "Read" => {
                if let Some(path) = info.file_path.as_deref() {
                    if let Some(first) = first_read_for.get(path) {
                        if first.tool_use_id != use_id {
                            let original_len = block_text_len(block);
                            replace_with_read_ref(block, path, &first.tool_use_id, original_len);
                            stats.read_results_deduped += 1;
                        }
                    }
                }
            }
            "Bash" => {
                if recompress_bash_block(block) {
                    stats.bash_results_recompressed += 1;
                }
            }
            _ => {}
        }
    }
}

fn block_text_len(block: &Value) -> usize {
    match block.get("content") {
        Some(Value::String(s)) => s.len(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .map(str::len)
            .sum(),
        _ => 0,
    }
}

fn replace_with_read_ref(block: &mut Value, path: &str, first_id: &str, original_len: usize) {
    let marker = format!(
        "[rtk: dedup same as Read tool_use {} ({} chars) at {}]",
        first_id, original_len, path
    );
    block["content"] = json!([{ "type": "text", "text": marker }]);
    block["rtk_compressed"] = json!({
        "axis": "ReadDedup",
        "first_tool_use_id": first_id,
        "file_path": path,
        "original_chars": original_len,
    });
}

fn recompress_bash_block(block: &mut Value) -> bool {
    if block.get("rtk_compressed").is_some() || block.get("contextzip_compressed").is_some() {
        return false;
    }
    let original = match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|c| c.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return false,
    };
    if original.is_empty() {
        return false;
    }
    let filtered = compress_bash_text(&original);
    if filtered.len() >= original.len() {
        return false;
    }
    let new_text = format!(
        "{}\n[rtk: BashHistoryCompact saved {} chars]",
        filtered,
        original.len() - filtered.len()
    );
    block["content"] = json!([{ "type": "text", "text": new_text }]);
    block["rtk_compressed"] = json!({
        "axis": "BashHistoryCompact",
        "original_chars": original.len(),
        "compressed_chars": new_text.len(),
        "content_sha256": sha256_hex(original.as_bytes()),
    });
    true
}

fn compress_bash_text(input: &str) -> String {
    let stripped = crate::core::noise::clean_text_noise(input);
    let mut out = Vec::new();
    let mut blank_run = 0usize;
    let mut last_line: Option<String> = None;
    let mut last_count = 0usize;

    for raw in stripped.lines() {
        let line = raw.trim_end();
        if line.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                if let Some(prev) = flush_repeat(&mut last_line, &mut last_count) {
                    out.push(prev);
                }
                out.push(String::new());
            }
            continue;
        }
        blank_run = 0;
        if last_line.as_deref() == Some(line) {
            last_count += 1;
            continue;
        }
        if let Some(prev) = flush_repeat(&mut last_line, &mut last_count) {
            out.push(prev);
        }
        last_line = Some(line.to_string());
        last_count = 1;
    }
    if let Some(prev) = flush_repeat(&mut last_line, &mut last_count) {
        out.push(prev);
    }
    if out.len() > 200 {
        let dropped = out.len() - 200;
        out.truncate(200);
        out.push(format!("(... {} more lines dropped by rtk)", dropped));
    }
    out.join("\n")
}

fn flush_repeat(last_line: &mut Option<String>, count: &mut usize) -> Option<String> {
    let line = last_line.take()?;
    let n = *count;
    *count = 0;
    if n > 1 {
        Some(format!("{} (x{})", line, n))
    } else {
        Some(line)
    }
}

fn validate_compacted_pair(original: &str, compressed: &str) -> Result<CompactStats> {
    let (expected, stats) = compact_session_str(original);
    if expected != compressed {
        bail!("compacted output does not match RTK's deterministic rewrite");
    }
    if compressed.len() >= original.len() {
        bail!("compacted output is not smaller");
    }
    if !stats.changed() {
        bail!("no compaction axes matched");
    }
    let original_lines = original.lines().count();
    let compressed_lines = compressed.lines().count();
    if original_lines != compressed_lines {
        bail!("compaction changed record count");
    }
    for (idx, (before, after)) in original.lines().zip(compressed.lines()).enumerate() {
        if serde_json::from_str::<Value>(before).is_ok()
            && serde_json::from_str::<Value>(after).is_err()
        {
            bail!("compaction produced invalid JSON on line {}", idx + 1);
        }
    }
    Ok(stats)
}

fn apply_compacted(
    session_path: &Path,
    compressed: &[u8],
    stats: &CompactStats,
    mode: ApplyMode,
) -> Result<PathBuf> {
    let backup = backup_path_for(session_path)?;
    if let Some(parent) = backup.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(session_path, &backup).with_context(|| {
        format!(
            "Failed to back up {} to {}",
            session_path.display(),
            backup.display()
        )
    })?;
    let tmp = session_path.with_extension("jsonl.rtk-tmp");
    fs::write(&tmp, compressed)?;
    if let Err(e) = fs::rename(&tmp, session_path) {
        let _ = fs::copy(&backup, session_path);
        return Err(e).context("Failed to promote compacted transcript; backup restored");
    }
    record_session_compaction(session_path, &backup, stats, mode, "applied");
    Ok(backup)
}

fn record_session_compaction(
    session_path: &Path,
    backup_path: &Path,
    stats: &CompactStats,
    mode: ApplyMode,
    status: &str,
) {
    if let Ok(tracker) = Tracker::new() {
        let _ = tracker.record_with_feature(
            &format!("session compact {}", session_path.display()),
            "rtk session compact --apply",
            stats.bytes_in / 4,
            stats.bytes_out / 4,
            0,
            "session",
        );
        let _ = tracker.record_session_compaction(
            session_path,
            backup_path,
            stats.bytes_in,
            stats.bytes_out,
            stats.percent_saved(),
            stats.read_results_deduped,
            stats.bash_results_recompressed,
            match mode {
                ApplyMode::Manual => "manual",
                ApplyMode::Auto => "auto",
            },
            status,
        );
    }
}

fn resolve_session_path(target: &str) -> Result<PathBuf> {
    let direct = expand_tilde(target);
    if direct.is_file() {
        return Ok(direct);
    }
    let root = claude_projects_root()?;
    for entry in WalkDir::new(&root).into_iter().filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == target || stem.starts_with(target) {
            return Ok(path.to_path_buf());
        }
    }
    bail!(
        "Could not resolve Claude session `{target}` under {}",
        root.display()
    )
}

fn discover_session_files() -> Result<Vec<PathBuf>> {
    let root = claude_projects_root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl")
            && !entry.path().to_string_lossy().contains("/subagents/")
        {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

fn claude_projects_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home.join(".claude").join("projects"))
}

fn sidecar_path(session: &Path) -> PathBuf {
    let mut p = session.to_path_buf();
    let name = session
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("session.jsonl");
    p.set_file_name(format!("{}.compressed", name));
    p
}

fn backup_path_for(session: &Path) -> Result<PathBuf> {
    let dir = session_backup_dir(session)?;
    let ts = Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
    Ok(dir.join(format!("{ts}.jsonl")))
}

fn latest_backup_for(session: &Path) -> Result<PathBuf> {
    let dir = session_backup_dir(session)?;
    let mut backups = fs::read_dir(&dir)
        .with_context(|| format!("No backups found for {}", session.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("jsonl"))
        .collect::<Vec<_>>();
    backups.sort();
    backups
        .pop()
        .with_context(|| format!("No backups found for {}", session.display()))
}

fn session_backup_dir(session: &Path) -> Result<PathBuf> {
    let data = rtk_data_dir()?.join("session-compaction").join("backups");
    Ok(data.join(sha256_hex(session.to_string_lossy().as_bytes())))
}

fn lock_path(session: &Path) -> Result<PathBuf> {
    Ok(rtk_data_dir()?
        .join("session-compaction")
        .join("locks")
        .join(format!(
            "{}.lock",
            sha256_hex(session.to_string_lossy().as_bytes())
        )))
}

fn rtk_data_dir() -> Result<PathBuf> {
    Ok(dirs::data_local_dir()
        .context("Cannot determine local data directory")?
        .join(RTK_DATA_DIR))
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

fn parse_duration(value: &str) -> Result<Duration> {
    let trimmed = value.trim();
    if trimmed.len() < 2 {
        bail!("Invalid duration `{value}`");
    }
    let (num, unit) = trimmed.split_at(trimmed.len() - 1);
    let n: u64 = num
        .parse()
        .with_context(|| format!("Invalid duration `{value}`"))?;
    let secs = match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => bail!("Invalid duration `{value}`; use s, m, h, or d"),
    };
    Ok(Duration::from_secs(secs))
}

fn is_older_than(path: &Path, age: Duration) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .map(|actual| actual >= age)
        .unwrap_or(false)
}

fn wait_for_stable_file(path: &Path, interval: Duration, attempts: usize) -> bool {
    let mut last = file_fingerprint(path);
    for _ in 0..attempts {
        thread::sleep(interval);
        let current = file_fingerprint(path);
        if current.is_some() && current == last {
            return true;
        }
        last = current;
    }
    false
}

fn file_fingerprint(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn acquire(session: &Path) -> Result<Option<Self>> {
        let path = lock_path(session)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Ok(meta) = fs::metadata(&path) {
            if meta
                .modified()
                .ok()
                .and_then(|m| m.elapsed().ok())
                .map(|e| e.as_secs() > LOCK_STALE_SECS)
                .unwrap_or(false)
            {
                let _ = fs::remove_file(&path);
            }
        }
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => Ok(Some(Self { path })),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e).context("Failed to create session compaction lock"),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn read_stdin_limited() -> Result<String> {
    let mut input = String::new();
    io::stdin()
        .take((HOOK_STDIN_CAP + 1) as u64)
        .read_to_string(&mut input)
        .context("Failed to read stdin")?;
    if input.len() > HOOK_STDIN_CAP {
        bail!("hook stdin exceeds {} byte limit", HOOK_STDIN_CAP);
    }
    Ok(input)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn writeln_no_panic(args: std::fmt::Arguments<'_>) -> io::Result<()> {
    use std::io::Write;
    let mut stderr = io::stderr();
    stderr.write_fmt(args)?;
    stderr.write_all(b"\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_read(id: &str, path: &str) -> String {
        json!({
            "type":"assistant",
            "message":{"content":[{"type":"tool_use","id":id,"name":"Read","input":{"file_path":path}}]}
        })
        .to_string()
    }

    fn assistant_bash(id: &str) -> String {
        json!({
            "type":"assistant",
            "message":{"content":[{"type":"tool_use","id":id,"name":"Bash","input":{"command":"cargo test"}}]}
        })
        .to_string()
    }

    fn tool_result(id: &str, text: &str) -> String {
        json!({
            "type":"user",
            "message":{"content":[{"type":"tool_result","tool_use_id":id,"content":[{"type":"text","text":text}]}]}
        })
        .to_string()
    }

    #[test]
    fn repeated_reads_become_references() {
        let input = format!(
            "{}\n{}\n{}\n{}\n",
            assistant_read("a", "/tmp/a.rs"),
            tool_result("a", "same content"),
            assistant_read("b", "/tmp/a.rs"),
            tool_result("b", "same content")
        );
        let (out, stats) = compact_session_str(&input);
        assert_eq!(stats.read_results_deduped, 1);
        assert!(out.contains("rtk: dedup same as Read tool_use a"));
    }

    #[test]
    fn unique_reads_are_unchanged() {
        let input = format!(
            "{}\n{}\n{}\n{}\n",
            assistant_read("a", "/tmp/a.rs"),
            tool_result("a", "a"),
            assistant_read("b", "/tmp/b.rs"),
            tool_result("b", "b")
        );
        let (_out, stats) = compact_session_str(&input);
        assert_eq!(stats.read_results_deduped, 0);
    }

    #[test]
    fn bash_results_are_recompressed_and_idempotent() {
        let noisy = "line\nline\nline\n\x1b[31merror\x1b[0m\n⠋ loading";
        let input = format!(
            "{}\n{}\n",
            assistant_bash("bash1"),
            tool_result("bash1", noisy)
        );
        let (out, stats) = compact_session_str(&input);
        assert_eq!(stats.bash_results_recompressed, 1);
        assert!(out.contains("line (x3)"));
        let (_out2, stats2) = compact_session_str(&out);
        assert_eq!(stats2.bash_results_recompressed, 0);
    }

    #[test]
    fn malformed_jsonl_passes_through() {
        let input = "not-json\n";
        let (out, stats) = compact_session_str(input);
        assert_eq!(out, input);
        assert_eq!(stats.records_written, 1);
    }
}
