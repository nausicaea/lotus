//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::path::PathBuf;

use anyhow::{anyhow, Context};
use bollard::container::{Config, LogsOptions};
use bollard::models::HostConfig;
use bollard::models::PortBinding;
use clap::Parser;
use futures_util::stream::StreamExt;
use similar::TextDiff;

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";
const INPUT_FILE: &'static str = "input.json";
const OUTPUT_FILE: &'static str = "output.json";
const DOCKER_IMAGE: &'static str = "docker.elastic.co/logstash/logstash:8.5.3";
const RULE_EXTENSION: &'static str = "conf";
const INPUT_PORT: u16 = 5066;
const OUTPUT_PORT: u16 = 5067;
const INPUT_RULE: &'static str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/logstash/input.conf"));
const OUTPUT_RULE: &'static str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/logstash/output.conf"));
const CONFIG: &'static str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/logstash/logstash.yml"
));
const PIPELINE_CONFIG: &'static str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/logstash/pipelines.yml"
));

#[derive(Debug)]
struct TestCase {
    input: PathBuf,
    output: PathBuf,
}

#[derive(Debug)]
struct Container {
    id: String,
}

fn collect_tests(tests_dir: &Path) -> anyhow::Result<Vec<TestCase>> {
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
        let output_file = test_case_dir.join(OUTPUT_FILE);
        if !output_file.is_file() {
            return Err(anyhow!(
                "The output file was not found: {}",
                output_file.display()
            ));
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
        let file_type = dir_entry
            .file_type()
            .context("Determining the file type of the rule")?;
        if !file_type.is_file() {
            continue;
        }
        let rule_file = dir_entry.path();
        match rule_file.extension() {
            None => continue,
            Some(ext) if ext != RULE_EXTENSION => continue,
            _ => (),
        }

        rules.push(rule_file);
    }

    rules.sort();

    Ok(rules)
}

async fn spawn_logstash(
    docker: &bollard::Docker,
    config_path: &Path,
    pipeline_config_path: &Path,
    pipeline_path: &Path,
) -> anyhow::Result<Container> {
    let response = docker
        .create_container::<String, String>(
            None,
            Config {
                image: Some(DOCKER_IMAGE.to_string()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                exposed_ports: Some(
                    [INPUT_PORT, OUTPUT_PORT]
                        .into_iter()
                        .map(|p| (format!("{}/tcp", p), std::collections::HashMap::default()))
                        .collect(),
                ),
                host_config: Some(HostConfig {
                    auto_remove: Some(true),
                    port_bindings: Some(
                        [INPUT_PORT, OUTPUT_PORT]
                            .into_iter()
                            .map(|p| {
                                (
                                    format!("{}/tcp", p),
                                    Some(vec![PortBinding {
                                        host_ip: Some(LOCALHOST.to_string()),
                                        host_port: Some(format!("{}/tcp", p)),
                                    }]),
                                )
                            })
                            .collect(),
                    ),
                    binds: Some(vec![
                        format!(
                            "{}:/usr/share/logstash/config/logstash.yml:ro",
                            config_path.display()
                        ),
                        format!(
                            "{}:/usr/share/logstash/config/pipelines.yml:ro",
                            pipeline_config_path.display()
                        ),
                        format!(
                            "{}:/usr/share/logstash/pipeline/logstash.conf:ro",
                            pipeline_path.display()
                        ),
                    ]),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await?;

    docker.start_container::<String>(&response.id, None).await?;

    Ok(Container { id: response.id })
}

async fn monitor(docker: &bollard::Docker, container_id: &str) -> anyhow::Result<()> {
    let mut log_stream = docker.logs::<String>(
        container_id,
        Some(LogsOptions {
            follow: true,
            stdout: true,
            stderr: true,
            ..Default::default()
        }),
    );

    while let Some(msg) = log_stream.next().await {
        let msg = msg.map(|m| m.to_string())?;
        println!("{}", msg);
    }

    Ok(())
}

async fn run_test_case(test_case: &TestCase) -> anyhow::Result<()> {
    let output = tempfile::NamedTempFile::new()?;

    todo!();

    let expected_output_data = std::fs::read_to_string(&test_case.output)?;
    let output_data = std::fs::read_to_string(&output.path())?;

    let diff = TextDiff::from_lines(&expected_output_data, &output_data);

    if diff.ratio() >= 1.0 {
        println!("Success!");
    } else {
        eprintln!(
            "{}",
            diff.unified_diff()
                .context_radius(10)
                .header("expected", "actual")
        );
    }

    // Close the files only at the end
    output.close()?;

    Ok(())
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Optional project path (e.g. the path to a directory containing `config` and `tests`
    /// subdirectories)
    project_dir: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Arguments::parse();

    let cwd = if let Some(project_dir) = args.project_dir {
        project_dir
    } else {
        std::env::current_dir()?
    };

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

    // Write the Logstash config
    let config = tempfile::NamedTempFile::new()?;
    let mut config_buffer = std::io::BufWriter::new(config);
    write!(config_buffer, "{}", CONFIG)?;
    let config = config_buffer.into_inner()?;

    // Write the Logstash pipeline config
    let pipeline_config = tempfile::NamedTempFile::new()?;
    let mut pipeline_config_buffer = std::io::BufWriter::new(pipeline_config);
    write!(pipeline_config_buffer, "{}", PIPELINE_CONFIG)?;
    let pipeline_config = pipeline_config_buffer.into_inner()?;

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

    let docker = bollard::Docker::connect_with_local_defaults()?;

    let container = spawn_logstash(
        &docker,
        config.path(),
        pipeline_config.path(),
        pipeline.path(),
    )
    .await?;

    let mut input_stream =
        tokio::net::TcpStream::connect(SocketAddr::new(LOCALHOST, INPUT_PORT)).await?;
    let mut output_stream =
        tokio::net::TcpStream::connect(SocketAddr::new(LOCALHOST, OUTPUT_PORT)).await?;

    for test_case in &test_cases {
        run_test_case(test_case).await?;
    }

    docker.stop_container(&container.id, None).await?;

    // Close the files only at the end
    pipeline.close()?;
    pipeline_config.close()?;
    config.close()?;

    Ok(())
}
