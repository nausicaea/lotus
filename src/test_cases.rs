use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

use assert_json_diff::assert_json_matches_no_panic;

use anyhow::{anyhow, Context};
use tokio::sync::mpsc::Receiver;

use crate::docker::{build_container_image, create_container, healthy};
use crate::{INPUT_PORT, LOCALHOST};

#[derive(Debug)]
pub struct TestCase {
    pub(crate) input: PathBuf,
    pub(crate) expected: PathBuf,
}

pub async fn run_test_case(
    client: &reqwest::Client,
    receiver: &mut Receiver<serde_json::Value>,
    test_case: &TestCase,
) -> anyhow::Result<()> {
    let input = File::open(&test_case.input).context("When opening the input file")?;
    let input_data = serde_json::from_reader::<_, serde_json::Value>(input)
        .context("When deserializing the input file")?;

    client
        .post(format!("http://{}:{}/", LOCALHOST, INPUT_PORT))
        .json(&input_data)
        .send()
        .await
        .context("When sending input data to the Logstash container via HTTP")?;

    let output_data = receiver
        .recv()
        .await
        .ok_or(anyhow!("Logstash did not send output event data"))?;

    let expected =
        File::open(&test_case.expected).context("When opening the expected output file")?;
    let expected_data = serde_json::from_reader::<_, serde_json::Value>(expected)
        .context("When deserializing the expected output file")?;

    let config = assert_json_diff::Config::new(assert_json_diff::CompareMode::Strict);
    if let Err(e) = assert_json_matches_no_panic(&output_data, &expected_data, config) {
        println!("{}", e);
    }

    Ok(())
}

pub async fn run_tests(
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

    println!("Running individual test cases");
    let client = reqwest::Client::new();
    for (i, test_case) in test_cases.iter().enumerate() {
        let test_case_name = test_case
            .input
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|dn| dn.to_str())
            .ok_or(anyhow!("Unable to determine the name of test case {}", i))?;

        run_test_case(&client, &mut receiver, test_case)
            .await
            .with_context(|| format!("When running test case {}: {}", i, test_case_name))?;
    }

    docker
        .stop_container(&container.id, None)
        .await
        .context("Stopping the Docker container")?;

    Ok(())
}
