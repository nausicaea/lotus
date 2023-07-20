//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use clap::Parser;

use lotus::{default_runner, DefaultArguments};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = DefaultArguments::parse();
    default_runner(&args).await
}
