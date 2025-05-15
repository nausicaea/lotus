use std::path::{Path, PathBuf};

use crate::runner::TestCase;
use crate::{EXPECTED_FILE, INPUT_FILE, RULE_EXTENSION, SCRIPT_EXTENSION};
use anyhow::{anyhow, Context};
use tracing::instrument;

#[instrument]
pub fn collect_tests(tests_dir: &Path) -> anyhow::Result<Vec<TestCase>> {
    let mut test_cases: Vec<TestCase> = Vec::new();
    let dir_iter = std::fs::read_dir(tests_dir)
        .with_context(|| format!("Reading the test cases directory: {}", tests_dir.display()))?;

    for dir_entry in dir_iter {
        let dir_entry = dir_entry.context("Collecting a test case")?;
        let file_type = dir_entry
            .file_type()
            .context("Determining the file type of the test case")?;
        if !file_type.is_dir() {
            continue;
        }
        let test_case_dir = dir_entry.path();
        let input_file = test_case_dir.join(INPUT_FILE);
        if !input_file.is_file() {
            return Err(anyhow!(
                "The input file was not found: {}",
                input_file.display()
            ));
        }
        let expected_file = test_case_dir.join(EXPECTED_FILE);
        if !expected_file.is_file() {
            return Err(anyhow!(
                "The expected output file was not found: {}",
                expected_file.display()
            ));
        }

        test_cases.push(TestCase {
            input: input_file,
            expected: expected_file,
        });
    }

    Ok(test_cases)
}

#[instrument(skip(predicate))]
fn collect_files<F: Fn(&std::ffi::OsStr) -> bool>(
    directory: &Path,
    predicate: F,
) -> anyhow::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    let dir_iter = std::fs::read_dir(directory)
        .with_context(|| format!("Reading the directory: {}", directory.display()))?;

    for dir_entry in dir_iter {
        let dir_entry = dir_entry.context("Collecting a file")?;
        let file_type = dir_entry.file_type().context("Determining the file type")?;
        if !file_type.is_file() {
            continue;
        }
        let file = dir_entry.path();
        let Some(ext) = file.extension() else {
            continue;
        };
        if !predicate(ext) {
            continue;
        }
        files.push(file);
    }

    files.sort();

    Ok(files)
}

#[instrument]
pub fn collect_rules(rules_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    collect_files(rules_dir, |ext| ext == RULE_EXTENSION)
}

#[instrument]
pub fn collect_scripts(scripts_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    collect_files(scripts_dir, |ext| ext == SCRIPT_EXTENSION)
}

#[instrument]
pub fn collect_patterns(patterns_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    collect_files(patterns_dir, |_| true)
}
