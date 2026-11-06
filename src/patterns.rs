use crate::Result;
use globset::{GlobBuilder, GlobMatcher};

pub(crate) fn is_pattern(text: &str) -> bool {
    text.contains(['*', '?', '[', '{'])
}

pub(crate) fn compile(pattern: &str) -> Result<GlobMatcher> {
    Ok(GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(false)
        .build()?
        .compile_matcher())
}
