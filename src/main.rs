//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::path::PathBuf;
use std::path::Path;

use anyhow::anyhow;

const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";
const INPUT_FILE: &'static str = "input.json";
const OUTPUT_FILE: &'static str = "output.json";

#[derive(Debug)]
struct TestCase {
    input: PathBuf,
    output: PathBuf,
}

fn collect(tests_dir: &Path) -> anyhow::Result<Vec<TestCase>> {
    let mut test_cases: Vec<TestCase> = Vec::new();
    for dir_entry in std::fs::read_dir(tests_dir)? {
        let dir_entry = dir_entry?;
        if !dir_entry.file_type()?.is_dir() {
            continue
        }
        let test_case_dir = dir_entry.path();
        let input_file = test_case_dir.join(INPUT_FILE);
        if !input_file.is_file() {
            return Err(anyhow!("The input file was not found: {}", input_file.display()));
        }
        let output_file = test_case_dir.join(OUTPUT_FILE);
        if !output_file.is_file() {
            return Err(anyhow!("The output file was not found: {}", output_file.display()));
        }

        test_cases.push(TestCase {
            input: input_file,
            output: output_file,
        });
    }

    Ok(test_cases)
}

fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let rules_dir = cwd.join(RULES_DIR);
    let tests_dir = cwd.join(TESTS_DIR);

    let test_cases = collect(&tests_dir)?;
    if test_cases.is_empty() {
        return Err(anyhow!("No test cases were found"));
    }

    println!("rules={}\ntests={}", rules_dir.display(), tests_dir.display());

    Ok(())
}
