//! Claude Code session transcript compaction.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};
use walkdir::WalkDir;

use crate::core::constants::RTK_DATA_DIR;
use crate::core::tracking::Tracker;

const HOOK_STDIN_CAP: usize = 1_048_576;
const LOCK_STALE_SECS: u64 = 600;
const RTK_COMPACTOR_VERSION: u32 = 2;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompactStats {
    pub records_read: usize,
    pub records_written: usize,
    pub bytes_in: usize,
    pub bytes_out: usize,
    pub read_results_deduped: usize,
    pub bash_results_recompressed: usize,
    pub newly_compacted_blocks: usize,
    pub already_compacted_blocks: usize,
    pub manifest_skipped_blocks: usize,
    pub recompacted_blocks: usize,
    pub last_compacted_record_index: Option<usize>,
}

impl CompactStats {
    pub fn percent_saved(&self) -> f64 {
        if self.bytes_in == 0 {
            return 0.0;
        }
        ((self.bytes_in - self.bytes_out.min(self.bytes_in)) as f64 / self.bytes_in as f64) * 100.0
    }

    pub fn changed(&self) -> bool {
        self.newly_compacted_blocks > 0
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionAgent {
    Auto,
    Claude,
    Codex,
}

impl SessionAgent {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Stop,
    SessionEnd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactionManifest {
    version: u32,
    session_path: String,
    session_path_sha256: String,
    blocks: Vec<ManifestEntry>,
}

impl CompactionManifest {
    fn new(session_path: &Path) -> Self {
        let path = session_path.to_string_lossy().to_string();
        Self {
            version: 1,
            session_path: path.clone(),
            session_path_sha256: sha256_hex(path.as_bytes()),
            blocks: Vec::new(),
        }
    }

    fn raw_hashes(&self) -> HashSet<String> {
        self.blocks
            .iter()
            .map(|entry| entry.raw_sha256.clone())
            .collect()
    }

    fn has_raw_hash(&self, hash: &str) -> bool {
        self.blocks.iter().any(|entry| entry.raw_sha256 == hash)
    }

    fn add(&mut self, entry: ManifestEntry) {
        if !self.has_raw_hash(&entry.raw_sha256) {
            self.blocks.push(entry);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestEntry {
    raw_sha256: String,
    compact_sha256: String,
    axis: String,
    agent: String,
    record_index: usize,
    tool_use_id: Option<String>,
    original_chars: usize,
    compressed_chars: usize,
    rtk_compactor_version: u32,
}

#[derive(Debug, Default, Clone)]
struct CacheExplain {
    first_changed_line: Option<usize>,
    stable_prefix_bytes: usize,
    stable_prefix_sha256: String,
}

pub fn run_overview(verbose: u8) -> Result<()> {
    crate::analytics::session_cmd::run(verbose)
}

#[allow(clippy::too_many_arguments)]
pub fn run_compact(
    target: Option<&str>,
    all: bool,
    dry_run: bool,
    apply: bool,
    older_than: Option<&str>,
    agent: SessionAgent,
    explain_cache: bool,
    verbose: u8,
) -> Result<()> {
    if all {
        let min_age = older_than.map(parse_duration).transpose()?;
        let mut done = 0usize;
        let mut skipped = 0usize;
        let mut total_in = 0usize;
        let mut total_out = 0usize;
        for path in discover_session_files(agent)? {
            if let Some(age) = min_age {
                if !is_older_than(&path, age) {
                    skipped += 1;
                    continue;
                }
            }
            match compact_path(
                &path,
                dry_run,
                apply,
                ApplyMode::Manual,
                agent,
                explain_cache,
                verbose,
            ) {
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
    let path = resolve_session_path(target, agent)?;
    let outcome = compact_path(
        &path,
        dry_run,
        apply,
        ApplyMode::Manual,
        agent,
        explain_cache,
        verbose,
    )?;
    print_outcome(&outcome, dry_run);
    Ok(())
}

pub fn run_apply(target: &str, verbose: u8) -> Result<()> {
    let session_path = resolve_session_path(target, SessionAgent::Auto)?;
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
    let session_path = resolve_session_path(target, SessionAgent::Auto)?;
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

pub fn run_status(target: &str, agent: SessionAgent) -> Result<()> {
    let session_path = resolve_session_path(target, agent)?;
    let raw = fs::read_to_string(&session_path)
        .with_context(|| format!("Failed to read {}", session_path.display()))?;
    let detected = detect_agent(&raw, agent);
    let manifest = load_manifest(&session_path)?;
    let mut preview_manifest = manifest.clone();
    let (_out, stats) = compact_session_str_with_manifest(&raw, detected, &mut preview_manifest);
    let manifest_path = manifest_path(&session_path)?;
    let manifest_saved = manifest
        .blocks
        .iter()
        .map(|entry| entry.original_chars.saturating_sub(entry.compressed_chars))
        .sum::<usize>();
    let last_record = manifest.blocks.iter().map(|entry| entry.record_index).max();
    println!("session: {}", session_path.display());
    println!("agent: {}", detected.as_str());
    println!(
        "records: {}, bytes: {} -> {} ({:.1}% saved)",
        stats.records_read,
        stats.bytes_in,
        stats.bytes_out,
        stats.percent_saved()
    );
    println!(
        "blocks: compacted={}, already-skipped={}, manifest-skipped={}, newly-compactable={}, recompacted={}",
        manifest.blocks.len(),
        stats.already_compacted_blocks,
        stats.manifest_skipped_blocks,
        stats.newly_compacted_blocks,
        stats.recompacted_blocks
    );
    println!(
        "saved: manifest={} bytes, preview={} bytes",
        manifest_saved,
        stats.bytes_in.saturating_sub(stats.bytes_out)
    );
    println!(
        "last compacted record index: {}",
        last_record
            .map(|idx| idx.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!("manifest: {}", manifest_path.display());
    if let Ok(backup) = latest_backup_for(&session_path) {
        println!("latest backup: {}", backup.display());
    }
    Ok(())
}

pub fn run_hook(agent: SessionAgent, event: HookEvent) -> Result<()> {
    let input = read_stdin_limited()?;
    let Ok(v) = serde_json::from_str::<Value>(input.trim()) else {
        return Ok(());
    };
    if !hook_event_matches(&v, event) {
        return Ok(());
    }
    let Some(path) = resolve_hook_transcript_path(&v, agent)? else {
        return Ok(());
    };
    if !path.is_file() {
        return Ok(());
    }

    let Some(_guard) = LockGuard::acquire(&path)? else {
        return Ok(());
    };
    if !wait_for_stable_file(&path, Duration::from_millis(250), 8) {
        return Ok(());
    }

    match compact_path(&path, false, true, ApplyMode::Auto, agent, false, 0) {
        Ok(_) => {}
        Err(e) => {
            let _ = writeln_no_panic(format_args!("[rtk session] auto-compaction skipped: {e}"));
        }
    }
    Ok(())
}

pub fn run_cache_bench(provider: &str, mode: &str) -> Result<()> {
    if provider != "openai" {
        bail!("unsupported cache-bench provider `{provider}`");
    }
    if mode == "live" {
        return run_openai_live_cache_bench();
    }
    if mode != "offline" {
        bail!("unsupported cache-bench mode `{mode}`");
    }

    let mut transcript = synthetic_claude_turn(0);
    let mut manifest = CompactionManifest::new(Path::new("<cache-bench>"));
    let (mut compacted, _) =
        compact_session_str_with_manifest(&transcript, SessionAgent::Claude, &mut manifest);
    let mut preserved = 0usize;
    for turn in 1..=5 {
        let prefix_len = compacted.len();
        let prefix_hash = sha256_hex(compacted.as_bytes());
        transcript = compacted.clone();
        transcript.push_str(&synthetic_claude_turn(turn));
        let mut next_manifest = manifest.clone();
        let (next, _stats) = compact_session_str_with_manifest(
            &transcript,
            SessionAgent::Claude,
            &mut next_manifest,
        );
        if next.as_bytes().get(..prefix_len) == Some(compacted.as_bytes())
            && sha256_hex(&next.as_bytes()[..prefix_len]) == prefix_hash
        {
            preserved += 1;
        }
        compacted = next;
        manifest = next_manifest;
    }

    let delayed_raw = (0..=5).map(synthetic_claude_turn).collect::<String>();
    let mut delayed_manifest = CompactionManifest::new(Path::new("<cache-bench-delayed>"));
    let (delayed_compacted, delayed_stats) = compact_session_str_with_manifest(
        &delayed_raw,
        SessionAgent::Claude,
        &mut delayed_manifest,
    );
    let delayed_explain = cache_explain(&delayed_raw, &delayed_compacted);

    println!("cache-bench offline provider=openai");
    println!("prefix preserved after each appended turn: {preserved}/5");
    println!(
        "delayed whole-session first_changed_line: {}",
        delayed_explain
            .first_changed_line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "none".to_string())
    );
    println!(
        "synthetic delayed compaction: blocks={}, bytes {} -> {}",
        delayed_stats.newly_compacted_blocks, delayed_stats.bytes_in, delayed_stats.bytes_out
    );
    Ok(())
}

fn run_openai_live_cache_bench() -> Result<()> {
    let api_key =
        std::env::var("OPENAI_API_KEY").context("live cache-bench requires OPENAI_API_KEY")?;
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-5".to_string());
    let stable_prefix = "RTK prompt-cache benchmark stable prefix.\n".repeat(1400);
    let prompt = format!("{stable_prefix}\nReturn the word ok.");

    let first = openai_cached_tokens(&api_key, &model, &prompt)?;
    thread::sleep(Duration::from_secs(2));
    let second = openai_cached_tokens(&api_key, &model, &prompt)?;

    println!("cache-bench live provider=openai model={model}");
    println!(
        "request 1: input_tokens={}, cached_tokens={}",
        first.input_tokens, first.cached_tokens
    );
    println!(
        "request 2: input_tokens={}, cached_tokens={}",
        second.input_tokens, second.cached_tokens
    );
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct OpenAiUsage {
    input_tokens: u64,
    cached_tokens: u64,
}

fn openai_cached_tokens(api_key: &str, model: &str, prompt: &str) -> Result<OpenAiUsage> {
    let body = json!({
        "model": model,
        "input": prompt,
        "max_output_tokens": 16,
        "store": false
    });
    let response = ureq::post("https://api.openai.com/v1/responses")
        .set("Authorization", &format!("Bearer {api_key}"))
        .set("Content-Type", "application/json")
        .send_string(&body.to_string());
    let raw = match response {
        Ok(resp) => resp
            .into_string()
            .context("Failed to read OpenAI response")?,
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            bail!("OpenAI API returned HTTP {code}: {body}");
        }
        Err(e) => return Err(e).context("OpenAI API request failed"),
    };
    let parsed: Value = serde_json::from_str(&raw).context("Failed to parse OpenAI response")?;
    let input_tokens = parsed
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            parsed
                .pointer("/usage/prompt_tokens")
                .and_then(Value::as_u64)
        })
        .unwrap_or_default();
    let cached_tokens = parsed
        .pointer("/usage/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            parsed
                .pointer("/usage/prompt_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
        })
        .unwrap_or_default();
    Ok(OpenAiUsage {
        input_tokens,
        cached_tokens,
    })
}

fn compact_path(
    session_path: &Path,
    dry_run: bool,
    apply: bool,
    mode: ApplyMode,
    agent: SessionAgent,
    explain_cache: bool,
    verbose: u8,
) -> Result<CompactOutcome> {
    let raw = fs::read_to_string(session_path)
        .with_context(|| format!("Failed to read session {}", session_path.display()))?;
    let detected = detect_agent(&raw, agent);
    let mut manifest = load_manifest(session_path)?;
    let (compressed, stats) = compact_session_str_with_manifest(&raw, detected, &mut manifest);

    if explain_cache {
        print_cache_explain(&raw, &compressed, &stats);
    }

    if dry_run {
        return Ok(CompactOutcome {
            session_path: session_path.to_path_buf(),
            sidecar_path: None,
            backup_path: None,
            stats,
            applied: false,
        });
    }

    if mode == ApplyMode::Auto && !stats.changed() {
        return Ok(CompactOutcome {
            session_path: session_path.to_path_buf(),
            sidecar_path: None,
            backup_path: None,
            stats,
            applied: false,
        });
    }

    if compressed == raw || compressed.len() >= raw.len() || !stats.changed() {
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
        save_manifest(session_path, &manifest)?;
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
    save_manifest(session_path, &manifest)?;
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

#[allow(dead_code)]
pub fn compact_session_str(input: &str) -> (String, CompactStats) {
    let mut manifest = CompactionManifest::new(Path::new("<memory>"));
    compact_session_str_with_manifest(input, SessionAgent::Claude, &mut manifest)
}

fn compact_session_str_with_manifest(
    input: &str,
    agent: SessionAgent,
    manifest: &mut CompactionManifest,
) -> (String, CompactStats) {
    let mut stats = CompactStats {
        bytes_in: input.len(),
        ..Default::default()
    };
    let mut tool_use_index = HashMap::new();
    let mut first_read_for = HashMap::new();

    for line in input.lines() {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            index_record(&record, agent, &mut tool_use_index, &mut first_read_for);
        }
    }

    let mut out = String::with_capacity(input.len());
    let mut compacted_hashes = manifest.raw_hashes();
    for (record_index, line) in input.lines().enumerate() {
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

        let before = record.clone();
        rewrite_record(
            &mut record,
            agent,
            record_index,
            &tool_use_index,
            &first_read_for,
            &mut compacted_hashes,
            manifest,
            &mut stats,
        );
        if record == before {
            out.push_str(line);
        } else {
            let written = serde_json::to_string(&record).unwrap_or_else(|_| line.to_string());
            out.push_str(&written);
        }
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
    content_hash: Option<String>,
}

fn index_record(
    record: &Value,
    agent: SessionAgent,
    tool_use_index: &mut HashMap<String, ToolUseInfo>,
    first_read_for: &mut HashMap<String, FirstRead>,
) {
    match agent {
        SessionAgent::Claude | SessionAgent::Auto => {
            index_claude_record(record, tool_use_index, first_read_for);
        }
        SessionAgent::Codex => index_codex_record(record, tool_use_index),
    }
}

fn index_claude_record(
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
                        content_hash: None,
                    });
            }
        }

        tool_use_index.insert(id.to_string(), ToolUseInfo { name, file_path });
    }

    // Also index tool_result blocks to capture Read content hashes for safe dedup
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let info = match tool_use_index.get(tool_use_id) {
            Some(i) if i.name == "Read" && i.file_path.is_some() => i,
            _ => continue,
        };
        let path = info.file_path.as_deref().unwrap();
        let content_text = block
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .or_else(|| block.get("content").and_then(|c| c.as_str()))
            .unwrap_or("");
        let hash = format!("{:x}", Sha256::digest(content_text.as_bytes()));
        first_read_for
            .entry(path.to_string())
            .and_modify(|fr| fr.content_hash = Some(hash.clone()))
            .or_insert_with(|| FirstRead {
                tool_use_id: tool_use_id.to_string(),
                content_hash: Some(hash),
            });
    }
}

fn index_codex_record(record: &Value, tool_use_index: &mut HashMap<String, ToolUseInfo>) {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return;
    }
    let Some(payload) = record.get("payload") else {
        return;
    };
    if payload.get("type").and_then(Value::as_str) != Some("function_call") {
        return;
    }
    let Some(id) = payload.get("call_id").and_then(Value::as_str) else {
        return;
    };
    let name = payload
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    tool_use_index.insert(
        id.to_string(),
        ToolUseInfo {
            name,
            file_path: None,
        },
    );
}

#[allow(clippy::too_many_arguments)]
fn rewrite_record(
    record: &mut Value,
    agent: SessionAgent,
    record_index: usize,
    tool_use_index: &HashMap<String, ToolUseInfo>,
    first_read_for: &HashMap<String, FirstRead>,
    compacted_hashes: &mut HashSet<String>,
    manifest: &mut CompactionManifest,
    stats: &mut CompactStats,
) {
    match agent {
        SessionAgent::Claude | SessionAgent::Auto => rewrite_claude_record(
            record,
            record_index,
            tool_use_index,
            first_read_for,
            compacted_hashes,
            manifest,
            stats,
        ),
        SessionAgent::Codex => rewrite_codex_record(
            record,
            record_index,
            tool_use_index,
            compacted_hashes,
            manifest,
            stats,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn rewrite_claude_record(
    record: &mut Value,
    record_index: usize,
    tool_use_index: &HashMap<String, ToolUseInfo>,
    first_read_for: &HashMap<String, FirstRead>,
    compacted_hashes: &mut HashSet<String>,
    manifest: &mut CompactionManifest,
    stats: &mut CompactStats,
) {
    let mut bash_result_ids = Vec::new();

    if let Some(content) = record
        .get_mut("message")
        .and_then(|m| m.get_mut("content"))
        .and_then(Value::as_array_mut)
    {
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let use_id = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if use_id
                .as_deref()
                .and_then(|id| tool_use_index.get(id))
                .is_some_and(|info| info.name == "Bash")
            {
                if let Some(id) = &use_id {
                    bash_result_ids.push(id.clone());
                }
            }
            if block.get("rtk_compressed").is_some() || block.get("contextzip_compressed").is_some()
            {
                stats.already_compacted_blocks += 1;
                continue;
            }
            let raw_sha256 = sha256_json(block);
            if compacted_hashes.contains(&raw_sha256) {
                stats.manifest_skipped_blocks += 1;
                continue;
            }
            let Some(use_id) = use_id else {
                continue;
            };
            let Some(info) = tool_use_index.get(&use_id) else {
                continue;
            };

            let original_chars = block_text_len(block);
            match info.name.as_str() {
                "Read" => {
                    if let Some(path) = info.file_path.as_deref() {
                        if let Some(first) = first_read_for.get(path) {
                            if first.tool_use_id != use_id {
                                // Only dedup if content hash matches (or no hash available for back-compat)
                                let current_hash = block
                                    .pointer("/content/0/text")
                                    .and_then(Value::as_str)
                                    .or_else(|| block.get("content").and_then(|c| c.as_str()))
                                    .map(|text| format!("{:x}", Sha256::digest(text.as_bytes())));
                                let should_dedup = match (&first.content_hash, current_hash) {
                                    (Some(first_hash), Some(current_hash)) => {
                                        first_hash == &current_hash
                                    }
                                    _ => true, // Fallback to path-only dedup when hashes unavailable
                                };
                                if !should_dedup {
                                    continue;
                                }
                                replace_with_read_ref(
                                    block,
                                    path,
                                    &first.tool_use_id,
                                    original_chars,
                                );
                                record_applied_block(
                                    block,
                                    manifest,
                                    compacted_hashes,
                                    stats,
                                    raw_sha256,
                                    "ReadDedup",
                                    SessionAgent::Claude,
                                    record_index,
                                    Some(use_id),
                                    original_chars,
                                );
                            }
                        }
                    }
                }
                "Bash" => {
                    if recompress_bash_block(block) {
                        record_applied_block(
                            block,
                            manifest,
                            compacted_hashes,
                            stats,
                            raw_sha256,
                            "BashHistoryCompact",
                            SessionAgent::Claude,
                            record_index,
                            Some(use_id),
                            original_chars,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    if bash_result_ids.is_empty() {
        return;
    }
    let Some(tool_use_result) = record.get_mut("toolUseResult") else {
        return;
    };
    if tool_use_result.get("rtk_compressed").is_some()
        || tool_use_result.get("contextzip_compressed").is_some()
    {
        stats.already_compacted_blocks += 1;
        return;
    }
    let raw_sha256 = sha256_json(tool_use_result);
    if compacted_hashes.contains(&raw_sha256) {
        stats.manifest_skipped_blocks += 1;
        return;
    }
    let original_chars = tool_use_result
        .get("stdout")
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or_default()
        + tool_use_result
            .get("stderr")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or_default();
    if recompress_claude_tool_use_result(tool_use_result) {
        record_applied_block(
            tool_use_result,
            manifest,
            compacted_hashes,
            stats,
            raw_sha256,
            "BashHistoryCompact",
            SessionAgent::Claude,
            record_index,
            bash_result_ids.first().cloned(),
            original_chars,
        );
    }
}

fn recompress_claude_tool_use_result(result: &mut Value) -> bool {
    let mut original_chars = 0usize;
    let mut compressed_chars = 0usize;
    let mut changed = false;
    let mut hasher_input = String::new();

    for field in ["stdout", "stderr"] {
        let Some(original) = result
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        if original.is_empty() {
            continue;
        }
        original_chars += original.len();
        hasher_input.push_str(field);
        hasher_input.push('\0');
        hasher_input.push_str(&original);
        hasher_input.push('\0');
        let filtered = compress_bash_text(&original);
        if filtered.len() >= original.len() {
            compressed_chars += original.len();
            continue;
        }
        let new_text = format!(
            "{}\n[rtk: BashHistoryCompact saved {} chars]",
            filtered,
            original.len() - filtered.len()
        );
        compressed_chars += new_text.len();
        result[field] = json!(new_text);
        changed = true;
    }

    if changed {
        result["rtk_compressed"] = json!({
            "axis": "BashHistoryCompact",
            "original_chars": original_chars,
            "compressed_chars": compressed_chars,
            "content_sha256": sha256_hex(hasher_input.as_bytes()),
            "rtk_compactor_version": RTK_COMPACTOR_VERSION,
        });
    }
    changed
}

fn rewrite_codex_record(
    record: &mut Value,
    record_index: usize,
    tool_use_index: &HashMap<String, ToolUseInfo>,
    compacted_hashes: &mut HashSet<String>,
    manifest: &mut CompactionManifest,
    stats: &mut CompactStats,
) {
    let record_type = record
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let Some(payload) = record.get_mut("payload") else {
        return;
    };
    let Some(output_field) = codex_output_field(record_type.as_deref(), payload) else {
        return;
    };
    if payload.get("rtk_compressed").is_some() || payload.get("contextzip_compressed").is_some() {
        stats.already_compacted_blocks += 1;
        return;
    }
    let raw_sha256 = sha256_json(payload);
    if compacted_hashes.contains(&raw_sha256) {
        stats.manifest_skipped_blocks += 1;
        return;
    }
    let Some(call_id) = payload
        .get("call_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return;
    };
    let Some(info) = tool_use_index.get(&call_id) else {
        return;
    };
    if !matches!(info.name.as_str(), "exec_command" | "write_stdin") {
        return;
    }
    let original_chars = payload
        .get(output_field)
        .and_then(Value::as_str)
        .map(str::len)
        .unwrap_or_default();
    if recompress_codex_output_field(payload, output_field) {
        record_applied_block(
            payload,
            manifest,
            compacted_hashes,
            stats,
            raw_sha256,
            "BashHistoryCompact",
            SessionAgent::Codex,
            record_index,
            Some(call_id),
            original_chars,
        );
    }
}

fn codex_output_field<'a>(record_type: Option<&str>, payload: &Value) -> Option<&'a str> {
    match (record_type, payload.get("type").and_then(Value::as_str)) {
        (Some("response_item"), Some("function_call_output")) => Some("output"),
        (Some("event_msg"), Some("exec_command_end")) => Some("aggregated_output"),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn record_applied_block(
    block: &Value,
    manifest: &mut CompactionManifest,
    compacted_hashes: &mut HashSet<String>,
    stats: &mut CompactStats,
    raw_sha256: String,
    axis: &str,
    agent: SessionAgent,
    record_index: usize,
    tool_use_id: Option<String>,
    original_chars: usize,
) {
    let compressed_chars = block_text_len(block).max(
        block
            .get("output")
            .and_then(Value::as_str)
            .map(str::len)
            .unwrap_or_default(),
    );
    manifest.add(ManifestEntry {
        raw_sha256: raw_sha256.clone(),
        compact_sha256: sha256_json(block),
        axis: axis.to_string(),
        agent: agent.as_str().to_string(),
        record_index,
        tool_use_id,
        original_chars,
        compressed_chars,
        rtk_compactor_version: RTK_COMPACTOR_VERSION,
    });
    compacted_hashes.insert(raw_sha256);
    stats.newly_compacted_blocks += 1;
    stats.last_compacted_record_index = Some(record_index);
    match axis {
        "ReadDedup" => stats.read_results_deduped += 1,
        "BashHistoryCompact" => stats.bash_results_recompressed += 1,
        _ => {}
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
        "rtk_compactor_version": RTK_COMPACTOR_VERSION,
    });
    true
}

fn recompress_codex_output_field(payload: &mut Value, field: &str) -> bool {
    let Some(original) = payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return false;
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
    payload[field] = json!(new_text);
    payload["rtk_compressed"] = json!({
        "axis": "BashHistoryCompact",
        "original_chars": original.len(),
        "compressed_chars": new_text.len(),
        "content_sha256": sha256_hex(original.as_bytes()),
        "rtk_compactor_version": RTK_COMPACTOR_VERSION,
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
    let stats = CompactStats {
        records_read: original.lines().count(),
        records_written: compressed.lines().count(),
        bytes_in: original.len(),
        bytes_out: compressed.len(),
        ..Default::default()
    };
    if compressed.len() >= original.len() {
        bail!("compacted output is not smaller");
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

fn detect_agent(input: &str, requested: SessionAgent) -> SessionAgent {
    if requested != SessionAgent::Auto {
        return requested;
    }
    for line in input.lines().take(20) {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) == Some("session_meta")
            || record
                .get("payload")
                .and_then(|p| p.get("type"))
                .and_then(Value::as_str)
                .is_some()
        {
            return SessionAgent::Codex;
        }
        if record.get("message").is_some() {
            return SessionAgent::Claude;
        }
    }
    SessionAgent::Claude
}

fn resolve_session_path(target: &str, agent: SessionAgent) -> Result<PathBuf> {
    let direct = expand_tilde(target);
    if direct.is_file() {
        return Ok(direct);
    }
    if matches!(agent, SessionAgent::Codex) {
        return resolve_codex_session_path(target);
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

fn resolve_codex_session_path(target: &str) -> Result<PathBuf> {
    for path in discover_codex_session_files()? {
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == target || stem.contains(target) || codex_session_id_matches(&path, target) {
            return Ok(path);
        }
    }
    let root = codex_archived_sessions_root()?;
    bail!(
        "Could not resolve Codex session `{target}` under {}",
        root.display()
    )
}

fn discover_session_files(agent: SessionAgent) -> Result<Vec<PathBuf>> {
    match agent {
        SessionAgent::Codex => discover_codex_session_files(),
        SessionAgent::Auto | SessionAgent::Claude => discover_claude_session_files(),
    }
}

fn discover_claude_session_files() -> Result<Vec<PathBuf>> {
    let root = claude_projects_root()?;
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|e| e.to_str()) == Some("jsonl")
            && !entry
                .path()
                .components()
                .any(|c| c.as_os_str() == "subagents")
        {
            files.push(entry.path().to_path_buf());
        }
    }
    Ok(files)
}

fn discover_codex_session_files() -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for root in codex_session_roots()? {
        if !root.is_dir() {
            continue;
        }
        files.extend(
            WalkDir::new(root)
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| entry.path().to_path_buf())
                .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("jsonl")),
        );
    }
    files.sort_by(|a, b| {
        let a_modified = fs::metadata(a).and_then(|m| m.modified()).ok();
        let b_modified = fs::metadata(b).and_then(|m| m.modified()).ok();
        b_modified
            .cmp(&a_modified)
            .then_with(|| b.to_string_lossy().cmp(&a.to_string_lossy()))
    });
    Ok(files)
}

fn claude_projects_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home.join(".claude").join("projects"))
}

fn codex_archived_sessions_root() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(home.join(".codex").join("archived_sessions"))
}

fn codex_session_roots() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("Cannot determine home directory")?;
    Ok(vec![
        home.join(".codex").join("sessions"),
        home.join(".codex").join("archived_sessions"),
    ])
}

fn codex_session_id_matches(path: &Path, target: &str) -> bool {
    let Ok(raw) = fs::read_to_string(path) else {
        return false;
    };
    raw.lines().take(5).any(|line| {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|record| {
                record
                    .pointer("/payload/id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .is_some_and(|id| id == target || id.starts_with(target))
    })
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

fn manifest_path(session: &Path) -> Result<PathBuf> {
    Ok(rtk_data_dir()?
        .join("session-compaction")
        .join("manifests")
        .join(format!(
            "{}.json",
            sha256_hex(session.to_string_lossy().as_bytes())
        )))
}

fn load_manifest(session: &Path) -> Result<CompactionManifest> {
    let path = manifest_path(session)?;
    if !path.is_file() {
        return Ok(CompactionManifest::new(session));
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("Failed to read manifest {}", path.display()))?;
    let mut manifest: CompactionManifest = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse manifest {}", path.display()))?;
    manifest.session_path = session.to_string_lossy().to_string();
    manifest.session_path_sha256 = sha256_hex(manifest.session_path.as_bytes());
    Ok(manifest)
}

fn save_manifest(session: &Path, manifest: &CompactionManifest) -> Result<()> {
    let path = manifest_path(session)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let serialized = serde_json::to_string_pretty(manifest)
        .context("Failed to serialize compaction manifest")?;
    fs::write(&path, serialized)
        .with_context(|| format!("Failed to write manifest {}", path.display()))
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
        "m" => n
            .checked_mul(60)
            .with_context(|| format!("Duration `{value}` overflows u64"))?,
        "h" => n
            .checked_mul(3600)
            .with_context(|| format!("Duration `{value}` overflows u64"))?,
        "d" => n
            .checked_mul(86400)
            .with_context(|| format!("Duration `{value}` overflows u64"))?,
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

fn hook_event_matches(payload: &Value, expected: HookEvent) -> bool {
    let Some(name) = payload
        .get("hook_event_name")
        .or_else(|| payload.get("event"))
        .or_else(|| payload.get("event_name"))
        .and_then(Value::as_str)
    else {
        return true;
    };
    match expected {
        HookEvent::Stop => matches!(name, "Stop" | "stop"),
        HookEvent::SessionEnd => matches!(name, "SessionEnd" | "session_end" | "session-end"),
    }
}

fn resolve_hook_transcript_path(payload: &Value, agent: SessionAgent) -> Result<Option<PathBuf>> {
    if let Some(path) = payload
        .get("transcript_path")
        .or_else(|| payload.get("transcriptPath"))
        .or_else(|| payload.get("session_path"))
        .and_then(Value::as_str)
    {
        return Ok(Some(expand_tilde(path)));
    }
    if !matches!(agent, SessionAgent::Codex | SessionAgent::Auto) {
        return Ok(None);
    }
    let Some(id) = payload
        .get("conversation_id")
        .or_else(|| payload.get("session_id"))
        .or_else(|| payload.get("id"))
        .or_else(|| payload.pointer("/payload/id"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    Ok(resolve_codex_session_path(id).ok())
}

fn print_cache_explain(raw: &str, compressed: &str, stats: &CompactStats) {
    let explain = cache_explain(raw, compressed);
    println!(
        "cache: first_changed_line={}, stable_prefix_bytes={}, stable_prefix_sha256={}",
        explain
            .first_changed_line
            .map(|line| line.to_string())
            .unwrap_or_else(|| "none".to_string()),
        explain.stable_prefix_bytes,
        explain.stable_prefix_sha256
    );
    println!(
        "cache: newly_compacted_blocks={}, recompacted_blocks={}, manifest_skipped_blocks={}",
        stats.newly_compacted_blocks, stats.recompacted_blocks, stats.manifest_skipped_blocks
    );
}

fn cache_explain(raw: &str, compressed: &str) -> CacheExplain {
    let raw_lines = raw.split_inclusive('\n').collect::<Vec<_>>();
    let compressed_lines = compressed.split_inclusive('\n').collect::<Vec<_>>();
    let mut stable_prefix_bytes = 0usize;
    let mut first_changed_line = None;
    let max = raw_lines.len().max(compressed_lines.len());
    for idx in 0..max {
        match (raw_lines.get(idx), compressed_lines.get(idx)) {
            (Some(before), Some(after)) if before == after => {
                stable_prefix_bytes += before.len();
            }
            _ => {
                first_changed_line = Some(idx + 1);
                break;
            }
        }
    }
    CacheExplain {
        first_changed_line,
        stable_prefix_bytes,
        stable_prefix_sha256: sha256_hex(&raw.as_bytes()[..stable_prefix_bytes]),
    }
}

fn synthetic_claude_turn(turn: usize) -> String {
    let read_id = format!("read-{turn}");
    let bash_id = format!("bash-{turn}");
    let repeated = (0..250)
        .map(|_| format!("turn {turn} repeated output line"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}\n{}\n{}\n{}\n",
        json!({
            "type":"assistant",
            "message":{"content":[{"type":"tool_use","id":read_id,"name":"Read","input":{"file_path":"/tmp/stable.txt"}}]}
        }),
        json!({
            "type":"user",
            "message":{"content":[{"type":"tool_result","tool_use_id":read_id,"content":[{"type":"text","text":"stable file contents"}]}]}
        }),
        json!({
            "type":"assistant",
            "message":{"content":[{"type":"tool_use","id":bash_id,"name":"Bash","input":{"command":"printf noise"}}]}
        }),
        json!({
            "type":"user",
            "message":{"content":[{"type":"tool_result","tool_use_id":bash_id,"content":[{"type":"text","text":repeated}]}]}
        })
    )
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

fn sha256_json(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    sha256_hex(&bytes)
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
    fn claude_tool_use_result_stdout_is_recompressed() {
        let noisy = (1..=800)
            .map(|i| {
                if i == 790 {
                    "CTXZIP_CANARY=hidden".to_string()
                } else {
                    format!("line {i}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let input = format!(
            "{}\n{}\n",
            assistant_bash("bash1"),
            json!({
                "type": "user",
                "message": {
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "bash1",
                        "content": [{"type": "text", "text": noisy}]
                    }]
                },
                "toolUseResult": {
                    "stdout": noisy,
                    "stderr": "",
                    "interrupted": false
                }
            })
        );
        let (out, stats) = compact_session_str(&input);
        assert_eq!(stats.bash_results_recompressed, 2);
        assert_eq!(out.matches("rtk_compressed").count(), 2);
        assert!(!out.contains("CTXZIP_CANARY=hidden"));
    }

    #[test]
    fn repeated_reads_are_idempotent_after_compression() {
        let input = format!(
            "{}\n{}\n{}\n{}\n",
            assistant_read("a", "/tmp/a.rs"),
            tool_result("a", "same content"),
            assistant_read("b", "/tmp/a.rs"),
            tool_result("b", "same content")
        );
        let (out, stats) = compact_session_str(&input);
        assert_eq!(stats.read_results_deduped, 1);
        let (out2, stats2) = compact_session_str(&out);
        assert_eq!(out2, out);
        assert_eq!(stats2.read_results_deduped, 0);
        assert_eq!(stats2.already_compacted_blocks, 1);
    }

    #[test]
    fn manifest_preserves_compacted_prefix_after_append() {
        let mut manifest = CompactionManifest::new(Path::new("/tmp/session.jsonl"));
        let first = format!(
            "{}\n{}\n",
            assistant_bash("bash1"),
            tool_result("bash1", "line\nline\nline\nline")
        );
        let (compacted, stats) =
            compact_session_str_with_manifest(&first, SessionAgent::Claude, &mut manifest);
        assert_eq!(stats.newly_compacted_blocks, 1);
        let prefix_hash = sha256_hex(compacted.as_bytes());

        let appended = format!(
            "{}{}\n{}\n",
            compacted,
            assistant_bash("bash2"),
            tool_result("bash2", "new\nnew\nnew\nnew")
        );
        let (next, stats2) =
            compact_session_str_with_manifest(&appended, SessionAgent::Claude, &mut manifest);
        assert_eq!(stats2.newly_compacted_blocks, 1);
        assert_eq!(&next[..compacted.len()], compacted);
        assert_eq!(sha256_hex(&next.as_bytes()[..compacted.len()]), prefix_hash);
    }

    #[test]
    fn codex_adapter_compacts_function_call_and_event_outputs() {
        let input = format!(
            "{}\n{}\n{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":"abc","base_instructions":{"text":"keep me"}}}),
            json!({"type":"response_item","payload":{"type":"function_call","name":"exec_command","arguments":"{\"cmd\":\"cargo test\"}","call_id":"call_1"}}),
            json!({"type":"event_msg","payload":{"type":"exec_command_end","call_id":"call_1","aggregated_output":"same\nsame\nsame\nsame","status":"completed"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"same\nsame\nsame\nsame"}})
        );
        let mut manifest = CompactionManifest::new(Path::new("/tmp/codex.jsonl"));
        let (out, stats) =
            compact_session_str_with_manifest(&input, SessionAgent::Codex, &mut manifest);
        assert_eq!(stats.bash_results_recompressed, 2);
        assert!(out.contains("base_instructions"));
        assert!(out.contains("rtk_compressed"));
        assert_eq!(out.matches("rtk_compressed").count(), 2);
    }

    #[test]
    fn malformed_jsonl_passes_through() {
        let input = "not-json\n";
        let (out, stats) = compact_session_str(input);
        assert_eq!(out, input);
        assert_eq!(stats.records_written, 1);
    }
}
