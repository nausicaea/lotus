//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use clap::Parser;
use lotus::default_runner;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Optional target path (e.g. the path to a directory containing `config` and `tests`
    /// subdirectories)
    pub target: Option<PathBuf>,
    /// If set, do not delete the Docker container after completion of the test run
    #[arg(short, long)]
    pub no_delete_container: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Arguments::parse();

    let target = if let Some(t) = args.target {
        t
    } else {
        std::env::current_dir()?
    };

    default_runner(&target, !args.no_delete_container).await
}
