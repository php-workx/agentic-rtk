//! Fork gate module — pure env-var functions, no upstream deps.
//!
//! Every fork feature should have a gate function here.
//! Upstream files call these gates via single-line sentinel-marked hooks.
//! See FORK.md for the full registry and rebase workflow.

/// Context Zip postprocessing (stacktrace, package-install, build-group, web-extract).
/// Default: enabled (set RTK_POSTPROCESS=0 to disable).
#[allow(dead_code)]
pub fn postprocess_enabled() -> bool {
    std::env::var("RTK_POSTPROCESS").unwrap_or_default() != "0"
}

/// Session compaction (Claude/Codex transcript Read-dedup + BashHistory recompress).
/// Default: enabled (set RTK_SESSION_COMPACT=0 to disable).
#[allow(dead_code)]
pub fn session_compaction_enabled() -> bool {
    std::env::var("RTK_SESSION_COMPACT").unwrap_or_default() != "0"
}

/// Web command (rtk web <URL>).
/// Default: enabled (set RTK_WEB_CMD=0 to disable).
#[allow(dead_code)]
pub fn web_command_enabled() -> bool {
    std::env::var("RTK_WEB_CMD").unwrap_or_default() != "0"
}

/// Command rewrite enhancements (head/tail/cat → rtk read, whitespace-safe extensions).
/// Default: enabled (set RTK_COMMAND_REWRITE=0 to disable).
#[allow(dead_code)]
pub fn command_rewrite_enabled() -> bool {
    std::env::var("RTK_COMMAND_REWRITE").unwrap_or_default() != "0"
}
