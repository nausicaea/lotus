//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use bollard::container::{Config, LogsOptions};
use bollard::models::HealthConfig;
use bollard::models::HealthStatusEnum;
use bollard::models::HostConfig;
use bollard::models::PortBinding;
use clap::Parser;
use directories::ProjectDirs;
use futures_util::stream::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use similar::TextDiff;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::time::sleep;

const BUFFER_SIZE: usize = 4096;
const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
const INPUT_PORT: u16 = 5066;
const OUTPUT_PORT: u16 = 5067;
const API_PORT: u16 = 9600;
const INPUT_SOCKET: SocketAddr = SocketAddr::new(LOCALHOST, INPUT_PORT);
const OUTPUT_SOCKET: SocketAddr = SocketAddr::new(LOCALHOST, OUTPUT_PORT);
const API_URL: &'static str = "https://127.0.0.1:9600/";
const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";
const INPUT_FILE: &'static str = "input.json";
const OUTPUT_FILE: &'static str = "output.json";
const DOCKER_IMAGE: &'static str = "docker.elastic.co/logstash/logstash:8.5.3";
const RULE_EXTENSION: &'static str = "conf";
const INPUT_RULE: &'static str = const_format::formatcp!("input {{ http {{ host => '0.0.0.0' port => {INPUT_PORT} response_code => 204 codec => json }} }}");
const OUTPUT_RULE: &'static str = "output { stdout { codec => json } }";
const CONFIG: &'static str = r#"---
http.host: "0.0.0.0"
# automatic reloading will not work with stdin input
config.reload.automatic: false
xpack.monitoring.enabled: false
log.level: info
log.format: json
# having one worker ensures the order of logs stays consistent to prevent concurrency issues
pipeline.ordered: false
pipeline.workers: 1
pipeline.ecs_compatibility: v1
"#;
const PIPELINE_CONFIG: &'static str = r#"---
- pipeline.id: main
  path.config: "/usr/share/logstash/pipeline"
"#;

#[derive(Debug)]
struct TestCase {
    input: PathBuf,
    output: PathBuf,
}

#[derive(Debug)]
struct Container {
    id: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Info {
    status: String,
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

async fn wait_for_healthy(
    docker: &bollard::Docker,
    container_id: &str,
    retries: usize,
    delay: Duration,
) -> anyhow::Result<()> {
    let mut curr_retries = 0;
    loop {
        let inspect = docker.inspect_container(container_id, None).await?;
        let health_status = inspect.state.and_then(|s| s.health).and_then(|h| h.status);
        match health_status {
            Some(HealthStatusEnum::HEALTHY) => break,
            None | Some(HealthStatusEnum::STARTING) => {
                curr_retries += 1;
                if curr_retries >= retries {
                    return Err(anyhow!(
                        "Failed to determine the Docker container health status after {} retries.",
                        retries
                    ));
                }
                sleep(delay).await;
            }
            Some(hs) => {
                return Err(anyhow!(
                    "Unexpected Docker container health status: {:?}",
                    hs
                ))
            }
        }
    }

    Ok(())
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
                healthcheck: Some(HealthConfig {
                    test: Some(["CMD-SHELL", r#"test $(curl -s "http://127.0.0.1:9600" | grep -Po '(?<="status":")\w+(?=")') = "green""#].into_iter().map(|s| s.to_string()).collect()),
                    ..Default::default()
                }),
                exposed_ports: Some(
                    [INPUT_PORT, OUTPUT_PORT, API_PORT]
                        .into_iter()
                        .map(|p| (format!("{}/tcp", p), std::collections::HashMap::default()))
                        .collect(),
                ),
                host_config: Some(HostConfig {
                    auto_remove: Some(false),
                    port_bindings: Some(
                        [INPUT_PORT, OUTPUT_PORT, API_PORT]
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

    let retries = 10;
    let delay = Duration::from_secs(10);
    wait_for_healthy(docker, &response.id, retries, delay).await?;

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

async fn connect_with_retries(
    socket: SocketAddr,
    retries: usize,
    delay: Duration,
) -> anyhow::Result<TcpStream> {
    let mut curr_retries = 0;
    loop {
        match TcpStream::connect(socket).await {
            Ok(strm) => {
                return Ok(strm);
            }
            Err(_) => {
                curr_retries += 1;
                if curr_retries >= retries {
                    return Err(anyhow!("Failed to connect after {} retries.", retries));
                }
                sleep(delay).await;
            }
        }
    }
}

async fn stream_write<const B: usize>(
    file_path: &Path,
    stream: &mut TcpStream,
) -> anyhow::Result<()> {
    // Open the local file for reading
    let mut file = File::open(file_path)?;

    // Create a buffer to hold the file contents
    let mut buffer = [0u8; B];

    // Read chunks from the file and write them to the TcpStream
    loop {
        let bytes_read = file.read(&mut buffer[..])?;
        if bytes_read == 0 {
            // End of file reached
            break;
        }

        let mut bytes_written = 0;
        loop {
            // Wait for the socket to be writable
            stream.writable().await?;

            // Try to write data, this may still fail with `WouldBlock`
            // if the readiness event is a false positive.
            match stream.try_write(&buffer[bytes_written..bytes_read]) {
                Ok(n) if n >= bytes_read => break,
                Ok(n) => {
                    bytes_written += n;
                }
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    return Err(e.into());
                }
            }
        }
    }

    // Flush any remaining data in the TcpStream
    stream.flush().await?;

    Ok(())
}

async fn stream_read<const B: usize>(stream: &TcpStream) -> anyhow::Result<String> {
    let mut output: Vec<u8> = Vec::new();

    let mut buffer = [0u8; B];

    let mut bytes_read = 0;
    loop {
        stream.readable().await?;

        match stream.try_read(&mut buffer[bytes_read..]) {
            Ok(0) => break,
            Ok(n) => {
                output.extend_from_slice(&buffer[..n]);
                bytes_read += n;
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                continue;
            }
            Err(e) => {
                return Err(e.into());
            }
        }
    }

    let output = String::from_utf8(output)?;

    Ok(output)
}

async fn run_test_case(
    test_case: &TestCase,
    input_stream: &mut TcpStream,
    output_stream: &TcpStream,
) -> anyhow::Result<()> {
    stream_write::<BUFFER_SIZE>(&test_case.input, input_stream).await?;
    sleep(Duration::from_secs(60)).await;
    let output_data = stream_read::<BUFFER_SIZE>(output_stream).await?;

    let expected_output_data = std::fs::read_to_string(&test_case.output)?;
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

    Ok(())
}

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
        std::fs::create_dir_all(&cache_dir)?;
    }

    // Write the Logstash config
    let config_path = cache_dir.join("logstash.yml");
    let config = File::create(&config_path)?;
    let mut config_buffer = std::io::BufWriter::new(config);
    write!(config_buffer, "{}", CONFIG)?;

    // Write the Logstash pipeline config
    let pipeline_config_path = cache_dir.join("pipelines.yml");
    let pipeline_config = File::create(&pipeline_config_path)?;
    let mut pipeline_config_buffer = std::io::BufWriter::new(pipeline_config);
    write!(pipeline_config_buffer, "{}", PIPELINE_CONFIG)?;

    // Write the Logstash pipeline
    let pipeline_path = cache_dir.join("logstash.conf");
    let pipeline = File::create(&pipeline_path)?;
    let mut pipeline_buffer = std::io::BufWriter::new(pipeline);
    writeln!(pipeline_buffer, "{}", INPUT_RULE)?;
    for rule in rules {
        let rule_file = std::fs::File::open(&rule)?;
        let mut rule_buffer = std::io::BufReader::new(rule_file);
        std::io::copy(&mut rule_buffer, &mut pipeline_buffer)?;
    }
    writeln!(pipeline_buffer, "{}", OUTPUT_RULE)?;

    let docker = bollard::Docker::connect_with_local_defaults()?;

    let container =
        spawn_logstash(&docker, &config_path, &pipeline_config_path, &pipeline_path).await?;

    // let client = Client::new();
    // let logstash_api_info = client.get("http://127.0.0.1:9600")
    //     .send()
    //     .await?
    //     .json::<Info>()
    //     .await?;

    // docker.stop_container(&container.id, None).await?;

    Ok(())
}
