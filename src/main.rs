//! Let `/` refer to the current working directory. The directory `/tests` contains subdirectories,
//! one for each test case. Input and output rules are fixed by the test runner. Each test case
//! contains files for input and expected output as JSON with a single event. Logstash filter rules
//! are created from the concatenated files in the `/rules` directory and the predefined `input`
//! (first) and `output` (last) rules. Files in `/rules` are sorted lexicographically before
//! concatenation.

use std::{
    fs::File,
    hash::{Hash, Hasher},
    io::Read,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context};
use axum::{extract::State, http::StatusCode, Json};
use bollard::{
    container::{Config, LogsOptions},
    image::BuildImageOptions,
    models::{BuildInfo, HealthStatusEnum, HostConfig, ImageId, PortBinding},
};
use clap::Parser;
use directories::ProjectDirs;
use futures_util::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::mpsc::{channel, Receiver, Sender},
    time::sleep,
};

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
const INPUT_PORT: u16 = 5066;
const OUTPUT_PORT: u16 = 5067;
const API_PORT: u16 = 9600;
const RULES_DIR: &'static str = "rules";
const TESTS_DIR: &'static str = "tests";
const INPUT_FILE: &'static str = "input.json";
const EXPECTED_FILE: &'static str = "expected.json";
const RULE_EXTENSION: &'static str = "conf";

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Arguments {
    /// Optional target path (e.g. the path to a directory containing `config` and `tests`
    /// subdirectories)
    target: Option<PathBuf>,
}

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

#[derive(Debug, Clone)]
struct ServerState {
    sender: Sender<serde_json::Value>,
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct Event {}

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
        "api_port": API_PORT,
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
    File::open(&archive_path)?.read_to_end(&mut archive_buffer)?;
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
                        [INPUT_PORT, API_PORT]
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

async fn healthy(
    docker: &bollard::Docker,
    container: &Container,
    retries: usize,
    delay: Duration,
) -> anyhow::Result<()> {
    let mut curr_retries = 0;
    loop {
        let inspect = docker
            .inspect_container(&container.id, None)
            .await
            .context("Inspecting the Docker container")?;
        let health_status = inspect.state.and_then(|s| s.health).and_then(|h| h.status);
        match health_status {
            Some(HealthStatusEnum::HEALTHY) => {
                println!("The container is up and healthy");
                break;
            }
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
                ));
            }
        }
    }

    Ok(())
}

async fn root(
    State(state): State<ServerState>,
    Json(payload): Json<serde_json::Value>,
) -> StatusCode {
    state
        .sender
        .send(payload)
        .await
        .context("When sending the request payload to the main task")
        .unwrap();
    StatusCode::NO_CONTENT
}

async fn run_server(sender: Sender<serde_json::Value>) -> anyhow::Result<()> {
    println!("Starting server");
    let state = ServerState { sender };
    axum::Server::bind(&SocketAddr::from(([0, 0, 0, 0], OUTPUT_PORT)))
        .serve(
            axum::Router::new()
                .route("/", axum::routing::post(root))
                .with_state(state)
                .into_make_service(),
        )
        .await
        .context("When running the output responder server")?;

    Ok(())
}

async fn run_tests(
    mut receiver: Receiver<serde_json::Value>,
    cache_dir: &Path,
    rules: Vec<PathBuf>,
    test_cases: Vec<TestCase>,
) -> anyhow::Result<()> {
    println!("Starting test runner");

    let docker =
        bollard::Docker::connect_with_local_defaults().context("Connecting to the Docker API")?;

    let image = build_container_image(&docker, cache_dir, &rules)
        .await
        .context("Building the Docker container image")?;

    let container = create_container(&docker, &image)
        .await
        .context("Creating the Docker container")?;

    docker
        .start_container::<String>(&container.id, None)
        .await
        .context("Starting the Docker container")?;

    let retries = 10;
    let delay = Duration::from_secs(10);
    healthy(&docker, &container, retries, delay)
        .await
        .context("Waiting for the Docker container to be healthy")?;

    let client = reqwest::Client::new();
    let logstash_info = client
        .get(format!("http://{}:{}/", LOCALHOST, API_PORT))
        .send()
        .await?
        .json::<Info>()
        .await?;

    println!("{:?}", logstash_info);

    println!("Running individual test cases");

    for test_case in test_cases {
        let mut input_data: Vec<u8> = Vec::new();
        File::open(&test_case.input)?.read_to_end(&mut input_data)?;

        client
            .post(format!("http://{}:{}/", LOCALHOST, INPUT_PORT))
            .body(reqwest::Body::from(input_data))
            .send()
            .await?;

        let output_data = receiver.recv().await;
        println!("{:?}", output_data);
    }

    docker
        .stop_container(&container.id, None)
        .await
        .context("Stopping the Docker container")?;

    receiver.close();

    Ok(())
}

async fn monitor(docker: &bollard::Docker, container: &Container) -> anyhow::Result<()> {
    let mut log_stream = docker.logs::<String>(
        &container.id,
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
    let (server, test_runner) = tokio::join!(
        tokio::spawn(async move { run_server(sender).await.unwrap() }),
        tokio::spawn(async move {
            run_tests(receiver, &cache_dir, rules, test_cases)
                .await
                .unwrap()
        }),
    );
    server?;
    test_runner?;

    Ok(())
}
