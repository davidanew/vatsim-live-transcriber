//! Loading of user-editable keyword and prompt files.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;

use crate::DEFAULT_PROMPT;

pub fn load_keywords(path: &Path) -> io::Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path)?;
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();
    for raw_line in content.lines() {
        let keyword = raw_line.trim();
        if keyword.is_empty() || keyword.starts_with('#') {
            continue;
        }
        if keyword.contains(['<', '>']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid keyword: {keyword:?}"),
            ));
        }
        let key = keyword.to_lowercase();
        if seen.insert(key) {
            keywords.push(keyword.to_owned());
        }
    }
    Ok(keywords)
}

pub fn load_prompt(path: Option<&Path>) -> io::Result<String> {
    let Some(path) = path else {
        return Ok(DEFAULT_PROMPT.to_owned());
    };
    let content = fs::read_to_string(path)?;
    let prompt = content.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    let prompt = prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the prompt file is empty",
        ));
    }
    Ok(prompt)
}
