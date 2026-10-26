//! Resolve instructions once on the host, rather than making the model discover them.
use anyhow::{Context, Result, ensure};
use pulldown_cmark::{Event, Parser, Tag};
use std::{
    collections::HashSet,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

const MAX_BYTES: usize = 65_536;
const MAX_FILES: usize = 32;

pub(crate) fn build(cwd: &Path, task: &str, report_chars: u64) -> Result<String> {
    let home = directories::BaseDirs::new().context("Cannot find home directory")?;
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.home_dir().join(".codex"));
    build_with_instructions(cwd, task, report_chars, home.home_dir(), &codex_home)
}

fn build_with_instructions(
    cwd: &Path,
    task: &str,
    report_chars: u64,
    home: &Path,
    codex_home: &Path,
) -> Result<String> {
    let mut instructions = Instructions::default();
    let mut files = Vec::new();
    for directory in
        std::iter::once(codex_home).chain(cwd.ancestors().collect::<Vec<_>>().into_iter().rev())
    {
        if let Some(path) = Instructions::directory(directory)? {
            files.push(path);
        }
    }
    // Reserve directly scoped files for their last (highest-precedence) position.
    files.reverse();
    files.retain(|path| instructions.pending.insert(path.clone()));
    files.reverse();
    for path in files {
        instructions.pending.remove(&path);
        instructions.file(&path, home)?;
    }
    Ok(format!(
        "You are a read-only repository investigator delegated by a parent coding agent.\n\
The starting working directory is: {}\n\
The host has already checked global and ancestor AGENTS.md / AGENTS.override.md files\n\
through this directory and expanded their standalone @file references.\n\
The following snapshot lists loaded instructions with their source paths, in precedence order.\n\
<loaded_instructions>\n{}\n</loaded_instructions>\n\
Follow the loaded instructions for this read-only task. You are already the delegated investigator:\n\
parent-agent delegation/workflow rules do not require another delegation. Do not spawn subagents,\n\
modify files, run builds or tests, install dependencies, or start services.\n\
Do not reread loaded instruction files or search home/configuration directories for more rules.\n\
When entering a new subdirectory, check only the exact local filenames AGENTS.override.md and\n\
AGENTS.md, preferring the override. Do not recursively search for instruction files.\n\
Stay within the supplied investigation. Read named task files directly unless already loaded above.\n\
Read outside the starting directory only for a concrete dependency needed to answer the task;\n\
do not search for parent instructions. Batch independent checks into as few tool calls as possible.\n\
Combine independent file reads and checks in one run_command shell batch instead of a separate\n\
read_file call per file. Reserve separate reads for follow-ups that depend on earlier results.\n\
Avoid repeating successful directory listings. If a required shell wrapper filters filenames,\n\
use its unfiltered/proxy mode for discovery so hidden files and empty results remain unambiguous.\n\
For usage questions, start with the README and give the documented quick start; inspect source\n\
or local tool versions only if needed to resolve a specific gap or explicitly requested. Once the\n\
documented startup steps are found, report them without tracing their implementation to reconfirm.\n\
Aim for at most four tool round trips for a basic usage question. Skip unrelated Git metadata,\n\
skills, and configuration. Stop as soon as the requested answer has sufficient evidence.\n\
Return: Conclusion, Essential evidence with precise file paths and line numbers,\n\
and Uncertainty or the smallest useful next check. Omit command transcripts and unrelated inventories.\n\
State clearly if permissions or missing information prevent completion.\n\
If the task asks to read instructions already loaded above, use the snapshot instead of searching again.\n\
Keep the final report within {report_chars} characters. Task follows:\n\n{task}",
        cwd.display(),
        if instructions.text.is_empty() {
            "(No instruction files found.)"
        } else {
            &instructions.text
        }
    ))
}

#[derive(Default)]
struct Instructions {
    seen: HashSet<PathBuf>,
    pending: HashSet<PathBuf>,
    bytes: usize,
    text: String,
}

impl Instructions {
    fn directory(directory: &Path) -> Result<Option<PathBuf>> {
        for name in ["AGENTS.override.md", "AGENTS.md"] {
            let path = directory.join(name);
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    return path.canonicalize().map(Some).with_context(|| {
                        format!("Cannot resolve instruction file {}", path.display())
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("Cannot inspect {}", path.display()));
                }
            }
        }
        Ok(None)
    }

    fn file(&mut self, path: &Path, home: &Path) -> Result<()> {
        let path = path
            .canonicalize()
            .with_context(|| format!("Cannot resolve instruction file {}", path.display()))?;
        if self.seen.contains(&path) || self.pending.contains(&path) {
            return Ok(());
        }
        ensure!(
            self.seen.len() < MAX_FILES,
            "Instruction files exceed {MAX_FILES} files"
        );
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("Cannot read instruction file {}", path.display()))?;
        ensure!(
            file.metadata()?.is_file(),
            "Instruction path is not a regular file: {}",
            path.display()
        );
        let mut bytes = Vec::new();
        file.take((MAX_BYTES - self.bytes + 1) as u64)
            .read_to_end(&mut bytes)?;
        self.bytes += bytes.len();
        ensure!(
            self.bytes <= MAX_BYTES,
            "Instruction files exceed {MAX_BYTES} bytes"
        );
        let text = String::from_utf8(bytes)
            .with_context(|| format!("Instruction file is not UTF-8: {}", path.display()))?;
        self.seen.insert(path.clone());
        self.text
            .push_str(&format!("\n### Source: {}\n", path.display()));
        let literals = Parser::new(&text)
            .into_offset_iter()
            .filter_map(|(event, range)| {
                matches!(
                    event,
                    Event::Start(Tag::CodeBlock(_))
                        | Event::Code(_)
                        | Event::Html(_)
                        | Event::InlineHtml(_)
                )
                .then_some(range)
            })
            .collect::<Vec<_>>();
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let position = offset + line.len() - line.trim_start().len();
            offset += line.len();
            let reference = if literals.iter().any(|range| range.contains(&position)) {
                None
            } else {
                import_reference(line)
            };
            if let Some(reference) = reference {
                let imported = match reference.strip_prefix("~/") {
                    Some(relative) => home.join(relative),
                    None => path.parent().unwrap().join(reference),
                };
                self.file(&imported, home)?;
                // Restore the source label after an expanded reference.
                self.text
                    .push_str(&format!("\n### Source: {} (continued)\n", path.display()));
            } else {
                self.text.push_str(line);
            }
        }
        self.text.push('\n');
        Ok(())
    }
}

fn import_reference(line: &str) -> Option<&str> {
    let reference = line.trim().strip_prefix('@')?;
    if let Some(quote @ ('\'' | '"')) = reference.chars().next() {
        return reference
            .strip_prefix(quote)?
            .strip_suffix(quote)
            .filter(|s| !s.is_empty());
    }
    (!reference.is_empty() && !reference.chars().any(char::is_whitespace)).then_some(reference)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn preserves_fenced_indented_and_prose_references() {
        let root = tempfile::tempdir().unwrap();
        let literal = "```markdown\n@./missing.md\n````\n~~~\n@./missing.md\n~~~\n    @./missing.md\n \t@missing.md\n \t```text\n \t@missing.md\n \t```\n- ```text\n  @missing.md\n  ```\n<!--\n@missing.md\n-->\n@maintainers please review\n";
        fs::write(root.path().join("rules with spaces.md"), "QUOTED_RULE").unwrap();
        fs::write(
            root.path().join("AGENTS.md"),
            format!("{literal}@\"./rules with spaces.md\"\n"),
        )
        .unwrap();
        let prompt =
            build_with_instructions(root.path(), "usage", 4000, root.path(), root.path()).unwrap();
        assert!(prompt.contains(literal));
        assert!(prompt.contains("QUOTED_RULE"));
    }

    #[test]
    fn imported_local_instructions_keep_their_directory_precedence() {
        let root = tempfile::tempdir().unwrap();
        let global = root.path().join("global");
        let cwd = root.path().join("repo/child");
        fs::create_dir(&global).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(
            global.join("AGENTS.md"),
            format!("@{}\nGLOBAL_RULE", cwd.join("AGENTS.md").display()),
        )
        .unwrap();
        fs::write(cwd.parent().unwrap().join("AGENTS.md"), "PARENT_RULE").unwrap();
        fs::write(cwd.join("AGENTS.md"), "LOCAL_RULE").unwrap();
        let prompt = build_with_instructions(&cwd, "usage", 4000, root.path(), &global).unwrap();
        assert!(prompt.find("PARENT_RULE").unwrap() < prompt.find("LOCAL_RULE").unwrap());
        assert_eq!(prompt.matches("LOCAL_RULE").count(), 1);
    }

    #[test]
    fn injects_instructions_in_order_with_imports_and_overrides() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let codex_home = home.join(".codex");
        let cwd = root.path().join("repo/./child");
        fs::create_dir_all(&codex_home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        fs::write(codex_home.join("AGENTS.md"), "@./RTK.md\nGLOBAL_RULE").unwrap();
        fs::write(codex_home.join("RTK.md"), "COMMAND_RULE").unwrap();
        fs::write(root.path().join("AGENTS.md"), "ANCESTOR_RULE").unwrap();
        fs::write(cwd.parent().unwrap().join("AGENTS.md"), "REPO_RULE").unwrap();
        fs::write(cwd.join("AGENTS.md"), "SHADOWED_RULE").unwrap();
        fs::write(cwd.join("AGENTS.override.md"), "LOCAL_RULE").unwrap();

        let prompt =
            build_with_instructions(&cwd, "Explain usage", 4000, &home, &codex_home).unwrap();
        let mut previous = 0;
        for rule in [
            "COMMAND_RULE",
            "GLOBAL_RULE",
            "ANCESTOR_RULE",
            "REPO_RULE",
            "LOCAL_RULE",
        ] {
            let position = prompt
                .find(rule)
                .unwrap_or_else(|| panic!("Missing {rule}"));
            assert!(position >= previous, "Incorrect precedence for {rule}");
            previous = position;
        }
        assert!(!prompt.contains("SHADOWED_RULE"));
        let source = cwd.join("AGENTS.override.md").canonicalize().unwrap();
        assert!(prompt.contains(&format!("### Source: {}", source.display())));
    }

    #[test]
    fn deduplicates_import_cycles_and_does_not_scan_descendants() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        fs::create_dir_all(cwd.join("child")).unwrap();
        fs::write(cwd.join("AGENTS.md"), "@./rules.md\nREPO_RULE").unwrap();
        fs::write(cwd.join("rules.md"), "@./AGENTS.md\nIMPORTED_RULE").unwrap();
        fs::write(cwd.join("child/AGENTS.md"), "NOT_YET_APPLICABLE").unwrap();
        let prompt = build_with_instructions(&cwd, "usage", 4000, root.path(), &cwd).unwrap();
        assert_eq!(prompt.matches("REPO_RULE").count(), 1);
        assert_eq!(prompt.matches("IMPORTED_RULE").count(), 1);
        assert!(!prompt.contains("NOT_YET_APPLICABLE"));
    }

    #[test]
    fn expands_absolute_and_home_relative_imports() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("repo");
        fs::create_dir(&cwd).unwrap();
        let rules = root.path().join("rules.md");
        fs::write(&rules, "COMMAND_RULE").unwrap();
        fs::write(
            cwd.join("AGENTS.md"),
            format!("@{}\n@~/rules.md", rules.display()),
        )
        .unwrap();
        let prompt = build_with_instructions(&cwd, "usage", 4000, root.path(), &cwd).unwrap();
        assert_eq!(prompt.matches("COMMAND_RULE").count(), 1);
    }

    #[test]
    fn rejects_missing_imports_non_files_and_oversized_instructions() {
        let root = tempfile::tempdir().unwrap();
        let agents = root.path().join("AGENTS.md");
        fs::write(&agents, "@missing.md").unwrap();
        let build =
            || build_with_instructions(root.path(), "usage", 4000, root.path(), root.path());
        assert!(build().unwrap_err().to_string().contains("missing.md"));
        fs::write(&agents, "@directory").unwrap();
        fs::create_dir(root.path().join("directory")).unwrap();
        assert!(build().unwrap_err().to_string().contains("directory"));
        fs::write(&agents, "x".repeat(MAX_BYTES + 1)).unwrap();
        assert!(
            build()
                .unwrap_err()
                .to_string()
                .contains("exceed 65536 bytes")
        );
    }

    #[test]
    fn bounds_import_fanout() {
        let root = tempfile::tempdir().unwrap();
        let imports = (0..MAX_FILES)
            .map(|n| {
                let name = format!("rule-{n}.md");
                fs::write(root.path().join(&name), "rule").unwrap();
                format!("@{name}\n")
            })
            .collect::<String>();
        fs::write(root.path().join("AGENTS.md"), imports).unwrap();
        let error = build_with_instructions(root.path(), "usage", 4000, root.path(), root.path())
            .unwrap_err();
        assert!(error.to_string().contains("exceed 32 files"));
    }
}
