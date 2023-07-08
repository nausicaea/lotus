//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::path::PathBuf;
use std::path::Path;
use std::io::Write;

use anyhow::anyhow;
use anyhow::Context;

use similar::TextDiff;

const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";
const INPUT_FILE: &'static str = "input.json";
const OUTPUT_FILE: &'static str = "output.json";
const DOCKER_IMAGE: &'static str = "docker.elastic.co/logstash/logstash:8.5.3";
const INPUT_RULE: &'static str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/logstash/input.conf"));
const OUTPUT_RULE: &'static str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/logstash/output.conf"));
const CONFIG: &'static str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/logstash/logstash.yml"));

#[derive(Debug)]
struct TestCase {
    input: PathBuf,
    output: PathBuf,
}

fn collect_tests(tests_dir: &Path) -> anyhow::Result<Vec<TestCase>> {
    let mut test_cases: Vec<TestCase> = Vec::new();
    let dir_iter = std::fs::read_dir(tests_dir)
        .with_context(|| format!("Reading the test cases directory: {}", tests_dir.display()))?;

    for dir_entry in dir_iter {
        let dir_entry = dir_entry.context("Collecting a test case")?;
        let file_type = dir_entry.file_type().context("Determining the file type of the test case")?;
        if !file_type.is_dir() {
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

fn collect_rules(rules_dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut rules: Vec<PathBuf> = Vec::new();
    let dir_iter = std::fs::read_dir(rules_dir)
        .with_context(|| format!("Reading the rules directory: {}", rules_dir.display()))?;

    for dir_entry in dir_iter {
        let dir_entry = dir_entry.context("Collecting a rule")?;
        let file_type = dir_entry.file_type().context("Determining the file type of the rule")?;
        if !file_type.is_file() {
            continue
        }
        let rule_file = dir_entry.path();
        match rule_file.extension() {
            None => continue,
            Some(ext) if ext != "conf" => continue,
            _ => (),
        }

        rules.push(rule_file);
    }

    rules.sort();

    Ok(rules)
}

fn run_logstash() -> anyhow::Result<()> {
    // docker run --rm -i \
    //      -v {config.path()}:/usr/share/logstash/config/logstash.yml:ro \
    //      -v {pipeline.path()}:/usr/share/logstash/pipeline/logstash.conf:ro \
    //      -v {output.path()}:/output.json \
    //      {DOCKER_IMAGE} \
    //      < {input.path()} 2>/dev/null

    Ok(())
}

fn run_test_case(test_case: &TestCase) -> anyhow::Result<()> {
    let output = tempfile::NamedTempFile::new()?;

    run_logstash()?;

    let expected_output_data = std::fs::read_to_string(&test_case.output)?;
    let output_data = std::fs::read_to_string(&output.path())?;

    let diff = TextDiff::from_lines(
        &expected_output_data,
        &output_data,
        );

    if diff.ratio() >= 1.0 {
        // Success case
    } else {
        // Failure case
    }

    // Close the files only at the end
    output.close()?;

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let rules_dir = cwd.join(RULES_DIR);
    let tests_dir = cwd.join(TESTS_DIR);

    let test_cases = collect_tests(&tests_dir)
        .context("Collecting all test cases")?;
    if test_cases.is_empty() {
        return Err(anyhow!("No test cases were found"));
    }

    let rules = collect_rules(&rules_dir)
        .context("Collecting all rules")?;
    if rules.is_empty() {
        return Err(anyhow!("No rules were found"));
    }

    // Write the Logstash config
    let config = tempfile::NamedTempFile::new()?;
    let mut config_buffer = std::io::BufWriter::new(config);
    write!(config_buffer, "{}", CONFIG)?;
    let config = config_buffer.into_inner()?;

    // Write the Logstash pipeline
    let pipeline = tempfile::NamedTempFile::new()?;
    let mut pipeline_buffer = std::io::BufWriter::new(pipeline);
    write!(pipeline_buffer, "{}", INPUT_RULE)?;
    for rule in rules {
        let rule_file = std::fs::File::open(&rule)?;
        let mut rule_buffer = std::io::BufReader::new(rule_file);
        std::io::copy(&mut rule_buffer, &mut pipeline_buffer)?;
    }
    write!(pipeline_buffer, "{}", OUTPUT_RULE)?;
    let pipeline = pipeline_buffer.into_inner()?;

    for test_case in &test_cases {
        run_test_case(test_case)?;
    }

    // Close the files only at the end
    pipeline.close()?;
    config.close()?;

    Ok(())
}
