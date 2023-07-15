//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::collections::HashMap;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::io::{self, Read, Seek};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use bollard::container::{Config, LogsOptions};
use bollard::image::BuildImageOptions;
use bollard::models::BuildInfo;
use bollard::models::HealthConfig;
use bollard::models::HealthStatusEnum;
use bollard::models::HostConfig;
use bollard::models::ImageId;
use bollard::models::Mount;
use bollard::models::MountTypeEnum;
use bollard::models::PortBinding;
use bollard::volume::CreateVolumeOptions;
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
const EXPECTED_FILE: &'static str = "expected.json";
const RULE_EXTENSION: &'static str = "conf";

#[derive(rust_embed::RustEmbed)]
#[folder = "logstash/config"]
struct ConfigAssets;

#[derive(rust_embed::RustEmbed)]
#[folder = "logstash/pipeline"]
struct PipelineAssets;

#[derive(Debug)]
struct TestCase {
    input: PathBuf,
    expected: PathBuf,
}

#[derive(Debug)]
struct Image {
    id: String,
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

async fn build_container_image(
    docker: &bollard::Docker,
    cache_dir: &Path,
    rules: &[PathBuf],
) -> anyhow::Result<Image> {
    let ctx = handlebars::Context::wraps(serde_json::json!({
        "input_port": INPUT_PORT,
        "output_port": OUTPUT_PORT,
    }))
    .context("Creating the Handlebars variable context")?;

    // Copy the static files over to the cache directory and build the tar archive
    let archive_path = cache_dir.join("image.tar");
    let archive =
        File::create(&archive_path).context("Creating the container image tar archive file")?;
    let mut ark = tar::Builder::new(archive);
    ark.mode(tar::HeaderMode::Deterministic);
    let mut hbs = handlebars::Handlebars::new();
    hbs.set_dev_mode(true);
    hbs.set_strict_mode(true);
    hbs.register_embed_templates::<ConfigAssets>()
        .context("Loading the Logstash config assets")?;
    for name in hbs.get_templates().keys() {
        let pth = cache_dir.join(name);
        hbs.render_with_context_to_write(
            name,
            &ctx,
            File::create(&pth)
                .with_context(|| format!("Creating the config file: {}", pth.display()))?,
        )
        .with_context(|| format!("Rendering the template: {}", name))?;
        ark.append_file(
            name,
            &mut File::open(&pth)
                .with_context(|| format!("Opening the config file: {}", pth.display()))?,
        )
        .with_context(|| format!("Appending the file to the tar archive: {}", name))?;
    }

    // Concatenate the pipeline file
    let pipeline_path = cache_dir.join("logstash.conf");
    {
        let mut hbs = handlebars::Handlebars::new();
        hbs.set_dev_mode(true);
        hbs.set_strict_mode(true);
        hbs.register_embed_templates::<PipelineAssets>()
            .context("Loading the Logstash pipeline assets")?;
        let mut pipeline = File::create(&pipeline_path)
            .with_context(|| format!("Creating the pipeline file: {}", pipeline_path.display()))?;
        hbs.render_with_context_to_write("input.conf", &ctx, &mut pipeline)
            .context("Rendering the template input.conf to the pipeline file")?;
        for rule in rules {
            std::io::copy(
                &mut File::open(rule)
                    .with_context(|| format!("Opening the rule file: {}", rule.display()))?,
                &mut pipeline,
            )
            .context("Adding the rule file to the pipeline file")?;
        }
        hbs.render_with_context_to_write("output.conf", &ctx, &mut pipeline)
            .context("Rendering the template output.conf to the pipeline file")?;
    }

    ark.append_file(
        "logstash.conf",
        &mut File::open(&pipeline_path).context("Opening the pipeline file")?,
    )
    .context("Adding the pipeline file 'logstash.conf' to the tar archive")?;

    // Close the archive
    ark.finish().context("Finishing the tar archive")?;

    // Build the container image from the tar archive
    let mut archive_buffer = Vec::new();
    let mut archive = File::open(&archive_path)?;
    archive.read_to_end(&mut archive_buffer)?;
    let mut builder_stream = docker.build_image(
        BuildImageOptions {
            t: "lotus-logstash:latest",
            ..Default::default()
        },
        None,
        Some(archive_buffer.into()),
    );

    let mut image_id: Option<Image> = None;
    while let Some(bi) = builder_stream.next().await {
        match bi {
            Ok(BuildInfo {
                stream: Some(output),
                ..
            }) => print!("{}", output),
            Ok(BuildInfo {
                aux: Some(ImageId { id }),
                ..
            }) => {
                image_id = id.map(|id| Image { id });
            }
            Ok(bi) => println!("Unknown build state: {:?}", bi),
            Err(bollard::errors::Error::DockerStreamError { error }) => {
                return Err(anyhow!("{}", error))
            }
            Err(e) => return Err(e.into()),
        }
    }

    image_id.ok_or(anyhow!("No container image ID was found"))
}

async fn create_container(docker: &bollard::Docker, image: &Image) -> anyhow::Result<Container> {
    let response = docker
        .create_container::<String, String>(
            None,
            Config {
                image: Some(image.id.clone()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                host_config: Some(HostConfig {
                    auto_remove: Some(true),
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
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .context("Creating the Docker container")?;

    Ok(Container { id: response.id })
}

async fn start_container(docker: &bollard::Docker, container: &Container) -> anyhow::Result<()> {
    docker
        .start_container::<String>(&container.id, None)
        .await
        .context("Starting the Docker container")?;

    // let retries = 10;
    // let delay = Duration::from_secs(10);
    // wait_for_healthy(docker, &response.id, retries, delay).await?;

    Ok(())
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

    let expected_output_data = std::fs::read_to_string(&test_case.expected)?;
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

    let docker = bollard::Docker::connect_with_local_defaults()?;

    let image = build_container_image(&docker, &cache_dir, &rules).await?;

    let container = create_container(&docker, &image).await?;

    start_container(&docker, &container).await?;

    Ok(())
}
