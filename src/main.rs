//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::{
    hash::{Hash, Hasher},
    path::PathBuf,
};

use anyhow::{anyhow, Context};
use clap::Parser;
use directories::ProjectDirs;
use tokio::sync::mpsc::channel;

use lotus::collectors::{collect_rules, collect_tests};
use lotus::server::run_server;
use lotus::test_cases::run_tests;

const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Optional target path (e.g. the path to a directory containing `config` and `tests`
    /// subdirectories)
    target: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let proj_dirs = ProjectDirs::from("net", "nausicaea", "lotus").ok_or(anyhow!(
        "Unable to determine the project directories based on the qualifier 'net.nausicaea.lotus'"
    ))?;

    let args = Arguments::parse();

    let cwd = if let Some(t) = args.target {
        t
    } else {
        std::env::current_dir()?
    };

    let target_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::default();
        cwd.hash(&mut hasher);
        hasher.finish().to_string()
    };

    let cache_dir = proj_dirs.cache_dir().join(target_hash);
    let rules_dir = cwd.join(RULES_DIR);
    let tests_dir = cwd.join(TESTS_DIR);

    let test_cases = collect_tests(&tests_dir).context("Collecting all test cases")?;
    if test_cases.is_empty() {
        return Err(anyhow!("No test cases were found"));
    }

    let rules = collect_rules(&rules_dir).context("Collecting all rules")?;
    if rules.is_empty() {
        return Err(anyhow!("No rules were found"));
    }

    if !cache_dir.is_dir() {
        std::fs::create_dir_all(&cache_dir).context("Creating the cache directory")?;
    }

    let (sender, receiver) = channel(32);
    tokio::select!(
        _ = tokio::spawn(async move { run_server(sender).await.context("Running the event responder server").unwrap() }) => {},
        _ = tokio::spawn(async move {
            run_tests(receiver, &cache_dir, rules, test_cases)
                .await
                .context("Running the test runner")
                .unwrap()
        }) => {},
    );

    Ok(())
}
