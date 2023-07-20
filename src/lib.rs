use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use clap::Parser;
use directories::ProjectDirs;
use tokio::sync::mpsc::channel;

use self::collectors::{collect_rules, collect_tests};
use self::runner::run_tests;
use self::server::run_server;

pub mod assets;
pub mod collectors;
pub mod docker;
pub mod runner;
pub mod server;

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
const INPUT_PORT: u16 = 5066;
const OUTPUT_PORT: u16 = 5067;
const API_PORT: u16 = 9600;
const INPUT_FILE: &'static str = "input.json";
const EXPECTED_FILE: &'static str = "expected.json";
const RULE_EXTENSION: &'static str = "conf";
const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";
const FQAN: [&'static str; 3] = ["net", "nausicaea", "lotus"];
const CHANNEL_CAPACITY: usize = 32;
const IMAGE_ARCHIVE_NAME: &'static str = "image.tar";
const PIPELINE_NAME: &'static str = "logstash.conf";
const INPUT_TEMPLATE_NAME: &'static str = "input.conf";
const OUTPUT_TEMPLATE_NAME: &'static str = "output.conf";

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
pub struct DefaultArguments {
    /// Optional target path (e.g. the path to a directory containing `config` and `tests`
    /// subdirectories)
    pub target: Option<PathBuf>,
    /// If set, do not delete the Docker container after completion of the test run
    #[arg(short, long)]
    pub no_delete_container: bool,
    /// Optionally change the location of the Logstash rules
    #[arg(short, long, default_value_t = String::from(RULES_DIR), env = "LOTUS_RULES_DIR")]
    pub rules_dir: String,
    /// Optionally change the location of the test cases
    #[arg(short, long, default_value_t = String::from(TESTS_DIR), env = "LOTUS_TESTS_DIR")]
    pub tests_dir: String,
}

impl DefaultArguments {
    fn target(&self) -> Result<PathBuf, anyhow::Error> {
        let Some(ref target) = self.target else {
            let cwd = std::env::current_dir()?;
            return Ok(cwd);
        };

        Ok(target.to_path_buf())
    }
}

impl Default for DefaultArguments {
    fn default() -> Self {
        Self {
            target: None,
            no_delete_container: false,
            rules_dir: String::from(RULES_DIR),
            tests_dir: String::from(TESTS_DIR),
        }
    }
}

pub async fn default_runner(args: &DefaultArguments) -> anyhow::Result<()> {
    let proj_dirs = ProjectDirs::from(FQAN[0], FQAN[1], FQAN[2]).ok_or(anyhow!(
        "Unable to determine the project directories based on the qualifier '{}'",
        FQAN.join(".")
    ))?;

    let target = args
        .target()
        .context("Determining the target location i.e., your project location")?;

    let target_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::default();
        target.hash(&mut hasher);
        hasher.finish().to_string()
    };

    let cache_dir = proj_dirs.cache_dir().join(target_hash);
    let rules_dir = target.join(&args.rules_dir);
    let tests_dir = target.join(&args.tests_dir);

    let rules = collect_rules(&rules_dir).context("Collecting all rules")?;
    if rules.is_empty() {
        return Err(anyhow!("No rules were found"));
    }

    println!("Collected {} Logstash rule files", rules.len());

    let test_cases = collect_tests(&tests_dir).context("Collecting all test cases")?;
    if test_cases.is_empty() {
        return Err(anyhow!("No test cases were found"));
    }

    println!("Collected {} test cases", test_cases.len());

    if !cache_dir.is_dir() {
        std::fs::create_dir_all(&cache_dir).context("Creating the cache directory")?;
    }

    let (sender_for_server, receiver_for_test_runner) = channel(CHANNEL_CAPACITY);
    tokio::select!(
        _ = tokio::spawn(async move {
            run_server(sender_for_server)
                .await
                .context("Running the event responder server")
                .unwrap()
        }) => {},
        e = run_tests(receiver_for_test_runner, cache_dir, rules, test_cases, !args.no_delete_container) => { e.unwrap() },
    );

    Ok(())
}
