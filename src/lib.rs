use std::hash::{Hash, Hasher};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use clap::Parser;
use directories::ProjectDirs;
use tokio::sync::mpsc::channel;
use tracing::{debug, info, instrument};

use crate::collectors::{collect_patterns, collect_scripts};

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
const INPUT_FILE: &str = "input.json";
const EXPECTED_FILE: &str = "expected.json";
const RULE_EXTENSION: &str = "conf";
const SCRIPT_EXTENSION: &str = "rb";
const RULES_DIR: &str = "rules";
const TESTS_DIR: &str = "tests";
const SCRIPTS_DIR: &str = "scripts";
const PATTERNS_DIR: &str = "patterns";
const FQAN: [&str; 3] = ["net", "nausicaea", "lotus"];
const CHANNEL_CAPACITY: usize = 32;
const IMAGE_ARCHIVE_NAME: &str = "image.tar";
const PIPELINE_NAME: &str = "logstash.conf";
const INPUT_TEMPLATE_NAME: &str = "input.conf";
const OUTPUT_TEMPLATE_NAME: &str = "output.conf";

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
    /// Optionally change the location of the ruby scripts associated with the pipeline
    #[arg(short, long, default_value_t = String::from(SCRIPTS_DIR), env = "LOTUS_SCRIPTS_DIR")]
    pub scripts_dir: String,
    /// Optionally change the location of the grok patterns associated with the pipeline
    #[arg(short, long, default_value_t = String::from(PATTERNS_DIR), env = "LOTUS_PATTERNS_DIR")]
    pub patterns_dir: String,
}

impl DefaultArguments {
    #[instrument]
    fn target(&self) -> Result<PathBuf, anyhow::Error> {
        let Some(ref target) = self.target else {
            let cwd = std::env::current_dir()?;
            return Ok(cwd);
        };

        Ok(target.to_path_buf())
    }
}

impl Default for DefaultArguments {
    #[instrument]
    fn default() -> Self {
        Self {
            target: None,
            no_delete_container: false,
            rules_dir: String::from(RULES_DIR),
            tests_dir: String::from(TESTS_DIR),
            scripts_dir: String::from(SCRIPTS_DIR),
            patterns_dir: String::from(PATTERNS_DIR),
        }
    }
}

#[instrument]
pub async fn default_runner(args: &DefaultArguments) -> anyhow::Result<()> {
    debug!("Determine the project data and cache directories");
    let proj_dirs = ProjectDirs::from(FQAN[0], FQAN[1], FQAN[2]).ok_or(anyhow!(
        "Unable to determine the project directories based on the qualifier '{}'",
        FQAN.join(".")
    ))?;

    debug!("Retrieve the test target directory (i.e. project directory)");
    let target = args
        .target()
        .context("Determining the target location i.e., your project location")?;

    debug!("Calculate a HashMap-based hash value for the target location");
    let target_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::default();
        target.hash(&mut hasher);
        hasher.finish().to_string()
    };

    debug!("Determine the cache, Logstash rules, and tests directories");
    let cache_dir = proj_dirs.cache_dir().join(target_hash);
    let rules_dir = target.join(&args.rules_dir);
    let tests_dir = target.join(&args.tests_dir);
    let scripts_dir = target.join(&args.scripts_dir);
    let patterns_dir = target.join(&args.patterns_dir);

    debug!("Collect all Logstash rules");
    let rules = collect_rules(&rules_dir).context("Collecting all rules")?;
    if rules.is_empty() {
        return Err(anyhow!("No rules were found"));
    }

    info!("Collected {} Logstash rule files", rules.len());

    debug!("Collect all test cases");
    let test_cases = collect_tests(&tests_dir).context("Collecting all test cases")?;
    if test_cases.is_empty() {
        return Err(anyhow!("No test cases were found"));
    }

    info!("Collected {} test cases", test_cases.len());

    let scripts = if scripts_dir.is_dir() {
        debug!("Collect all ruby scripts");
        let scripts = collect_scripts(&scripts_dir).context("Collecting all ruby scripts")?;
        info!("Collected {} ruby scripts", scripts.len());
        scripts
    } else {
        Vec::default()
    };

    let patterns = if patterns_dir.is_dir() {
        debug!("Collect all grok patterns");
        let patterns = collect_patterns(&patterns_dir).context("Collecting all grok patterns")?;
        info!("Collected {} grok patterns", patterns.len());
        patterns
    } else {
        Vec::default()
    };

    if !cache_dir.is_dir() {
        debug!("Create a cache directory");
        std::fs::create_dir_all(&cache_dir)
            .with_context(|| format!("Creating the cache directory: {}", cache_dir.display()))?;
    }

    debug!(
        "Create a communication channel between the test executor and the test response handler"
    );
    let (sender_for_server, receiver_for_test_runner) = channel(CHANNEL_CAPACITY);

    debug!("Launch both the test executor and the test response handler");
    tokio::select!(
        _ = tokio::spawn(async move {
            run_server(sender_for_server)
                .await
                .context("Running the event responder server")
                .unwrap()
        }) => {},
        e = run_tests(receiver_for_test_runner, cache_dir, rules, test_cases, scripts, patterns, !args.no_delete_container) => {
            e.expect("Error running Logstash tests");
        },
    );

    Ok(())
}
