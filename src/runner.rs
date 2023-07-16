use std::{fs::File, path::PathBuf, time::Duration};

use assert_json_diff::assert_json_matches_no_panic;
use bollard::Docker;
use reqwest::Client;

use anyhow::{anyhow, Context};
use serde_json::{from_reader, Value};
use tokio::sync::mpsc::Receiver;

use crate::docker::{build_container_image, create_container, healthy, Container};
use crate::{INPUT_PORT, LOCALHOST};

#[derive(Debug)]
struct InnerTestContext {
    docker: Docker,
    container: Container,
    http_client: Client,
    receiver: Receiver<Value>,
}

impl InnerTestContext {
    async fn new(
        receiver: Receiver<Value>,
        cache_dir: PathBuf,
        rules: Vec<PathBuf>,
    ) -> anyhow::Result<Self> {
        let docker =
            Docker::connect_with_local_defaults().context("Connecting to the Docker API")?;

        let image = build_container_image(&docker, &cache_dir, &rules)
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

        let http_client = reqwest::Client::new();

        Ok(InnerTestContext {
            docker,
            container,
            http_client,
            receiver,
        })
    }
}

#[derive(Debug)]
pub struct TestContext(Option<InnerTestContext>);

impl TestContext {
    pub async fn new(
        receiver: Receiver<Value>,
        cache_dir: PathBuf,
        rules: Vec<PathBuf>,
    ) -> anyhow::Result<Self> {
        Ok(Self(Some(
            InnerTestContext::new(receiver, cache_dir, rules).await?,
        )))
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        if let Some(tc) = self.0.take() {
            tokio::spawn(async move {
                let _ = tc.docker.stop_container(&tc.container.id, None).await;
            });
        }
    }
}

#[derive(Debug)]
pub struct TestCase {
    pub(crate) input: PathBuf,
    pub(crate) expected: PathBuf,
}

pub async fn run_single_test(
    client: &Client,
    receiver: &mut Receiver<Value>,
    test_case: &TestCase,
) -> anyhow::Result<()> {
    let input = File::open(&test_case.input).context("When opening the input file")?;
    let input_data = from_reader::<_, Value>(input).context("When deserializing the input file")?;

    client
        .post(format!("http://{}:{}/", LOCALHOST, INPUT_PORT))
        .json(&input_data)
        .send()
        .await
        .context("Sending input data to the Logstash container via HTTP")?;

    let output_data = receiver
        .recv()
        .await
        .ok_or(anyhow!("Logstash did not send output event data"))?;

    let expected =
        File::open(&test_case.expected).context("When opening the expected output file")?;
    let expected_data =
        from_reader::<_, Value>(expected).context("Deserializing the expected output file")?;

    let config = assert_json_diff::Config::new(assert_json_diff::CompareMode::Strict);
    assert_json_matches_no_panic(&output_data, &expected_data, config)
        .map_err(|e| anyhow!("{}", e))
        .context("Comparing the Logstash output (lhs) with the expected output (rhs)")?;

    Ok(())
}

pub async fn run_tests(
    receiver: Receiver<Value>,
    cache_dir: PathBuf,
    rules: Vec<PathBuf>,
    test_cases: Vec<TestCase>,
) -> anyhow::Result<()> {
    let mut context = TestContext::new(receiver, cache_dir, rules)
        .await
        .context("Bootstrapping the test environment")?;

    for (i, test_case) in test_cases.iter().enumerate() {
        let test_case_name = test_case
            .input
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|dn| dn.to_str())
            .ok_or(anyhow!("Unable to determine the name of test case {}", i))?;

        let (http_client, receiver) = context
            .0
            .as_mut()
            .map(|ctx| (&ctx.http_client, &mut ctx.receiver))
            .unwrap();

        run_single_test(http_client, receiver, test_case)
            .await
            .with_context(|| format!("Running test case {}: {}", i, test_case_name))?;
    }

    Ok(())
}
