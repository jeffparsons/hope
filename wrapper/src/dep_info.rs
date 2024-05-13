use std::{collections::HashMap, fs::read_to_string, path::Path};

use anyhow::Context;

pub struct DepInfo {
    pub files: HashMap<String, Vec<String>>,
}

impl DepInfo {
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let lines = s.lines();
        let mut files = HashMap::new();
        for line in lines {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // TODO: Handle escaping of file names.
            let (file_name, rest) = line.split_once(':').context("Couldn't find ':'")?;
            // There will be a space after the ':' if there are actually any deps.
            let deps = rest
                .trim()
                .split(' ')
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            files.insert(file_name.to_owned(), deps.collect());
        }

        Ok(Self { files })
    }

    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let content = read_to_string(path)?;
        Self::parse(&content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_dep_info_file() {
        let dep_info = r#"
            # Some comments
            /something/target/debug/deps/something-152c1f4ab9b42169: src/main.rs

            /something/target/debug/deps/something-152c1f4ab9b42169.d: src/main.rs

            src/main.rs:
        "#;
        let dep_info = DepInfo::parse(dep_info).unwrap();
        assert_eq!(
            dep_info
                .files
                .get("/something/target/debug/deps/something-152c1f4ab9b42169"),
            Some(&vec!["src/main.rs".to_string()])
        );
        // Shouldn't have put any deps for the file with no deps.
        assert!(dep_info.files.get("src/main.rs").unwrap().is_empty())
    }
}
