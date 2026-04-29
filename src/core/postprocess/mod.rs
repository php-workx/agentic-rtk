//! Shared conservative postprocessors for outputs that cross tool ecosystems.

pub mod build_group;
pub mod package_install;
pub mod stacktrace;
pub mod web_extract;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostprocessKind {
    Stacktrace,
    PackageInstall,
    BuildGroup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostprocessResult {
    pub output: String,
    pub feature: Option<&'static str>,
}

pub fn apply_postprocessors(input: &str, processors: &[PostprocessKind]) -> PostprocessResult {
    let mut output = input.to_string();
    let mut feature = None;

    for processor in processors {
        let before = output.clone();
        let after = match processor {
            PostprocessKind::Stacktrace => stacktrace::compress_errors(&output),
            PostprocessKind::PackageInstall => package_install::compress_pkg_log(&output),
            PostprocessKind::BuildGroup => build_group::group_build_errors(&output),
        };

        if after != before && after.len() <= before.len() {
            output = after;
            if feature.is_none() {
                feature = Some(processor.feature_name());
            }
        }
    }

    PostprocessResult { output, feature }
}

impl PostprocessKind {
    pub fn feature_name(self) -> &'static str {
        match self {
            Self::Stacktrace => "stacktrace",
            Self::PackageInstall => "pkg-install",
            Self::BuildGroup => "build-group",
        }
    }
}
