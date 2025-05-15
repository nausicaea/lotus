use std::{path::PathBuf, time::Duration};

use assert_json_diff::assert_json_matches_no_panic;
use bollard::Docker;
use reqwest::Client;

use anyhow::{anyhow, Context};
use serde_json::{from_reader, Value};
use tokio::sync::mpsc::Receiver;
use tracing::{debug, info_span, instrument, Instrument};

use crate::docker::{build_container_image, create_container, healthy, Container};
use crate::{INPUT_PORT, LOCALHOST};

#[derive(Debug)]
pub struct TestContext {
    docker: Docker,
    container: Container,
    http_client: Client,
    receiver: Receiver<Value>,
}

impl TestContext {
    #[instrument]
    pub async fn new(
        receiver: Receiver<Value>,
        cache_dir: PathBuf,
        rules: Vec<PathBuf>,
        scripts: Vec<PathBuf>,
        patterns: Vec<PathBuf>,
        delete_container: bool,
    ) -> anyhow::Result<Self> {
        debug!("Connect to the Docker API");
        let docker =
            Docker::connect_with_local_defaults().context("Connecting to the Docker API")?;

        debug!("Build the Logstash container image");
        let image = build_container_image(&docker, &cache_dir, &rules, &scripts, &patterns)
            .await
            .context("Building the Docker container image for Logstash")?;

        debug!("Create the Logstash container");
        let container = create_container(&docker, &image, delete_container)
            .await
            .context("Creating the Logstash Docker container")?;

        debug!("Start the Logstash container");
        docker
            .start_container::<String>(&container.id, None)
            .await
            .context("Starting the Logstash Docker container")?;

        debug!("Wait for the Logstash container to become healthy");
        let retries = 10;
        let delay = Duration::from_secs(10);
        healthy(&docker, &container, retries, delay)
            .await
            .context("Waiting for the Docker container to be healthy")?;

        let http_client = reqwest::Client::new();

        Ok(Self {
            docker,
            container,
            http_client,
            receiver,
        })
    }

    #[instrument]
    async fn close(self) -> anyhow::Result<()> {
        self.docker.stop_container(&self.container.id, None).await?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct TestCase {
    pub(crate) input: PathBuf,
    pub(crate) expected: PathBuf,
}

#[instrument]
pub async fn run_single_test(
    client: &Client,
    receiver: &mut Receiver<Value>,
    test_case: &TestCase,
    verbose: bool,
) -> anyhow::Result<()> {
    debug!("Deserialize the input file as JSON");
    let input = tokio::fs::File::open(&test_case.input)
        .await
        .context("When opening the input file")?;
    let input = input.into_std().await;
    let input_data = tokio::task::spawn_blocking(|| {
        from_reader::<_, Value>(input).context("When deserializing the input file")
    })
    .await??;

    let request_span = info_span!("logstash_request");
    debug!("Post the input data to Logstash running at {LOCALHOST}:{INPUT_PORT}");
    client
        .post(format!("http://{}:{}/", LOCALHOST, INPUT_PORT))
        .json(&input_data)
        .send()
        .instrument(request_span)
        .await
        .context("Sending input data to the Logstash container via HTTP")?;

    let response_span = info_span!("logstash_response");
    debug!("Wait for a message from the Logstash response handler (MPSC channel)");
    let output_data = receiver
        .recv()
        .instrument(response_span)
        .await
        .ok_or(anyhow!("Logstash did not send output event data"))?;

    debug!("Deserialize the expected output file as JSON");
    let expected = tokio::fs::File::open(&test_case.expected)
        .await
        .context("When opening the expected output file")?;
    let expected = expected.into_std().await;
    let expected_data = tokio::task::spawn_blocking(|| {
        from_reader::<_, Value>(expected).context("Deserializing the expected output file")
    })
    .await??;

    debug!("Compare the JSON objects of the Logstash output (lhs) and the expected output (rhs)");
    let config = assert_json_diff::Config::new(assert_json_diff::CompareMode::Strict);
    assert_json_matches_no_panic(&output_data, &expected_data, config)
        .map_err(|e| {
            let output_json = match serde_json::to_string_pretty(&output_data) {
                Ok(oj) => oj,
                Err(e) => return Into::<anyhow::Error>::into(e),
            };
            let expected_json = match serde_json::to_string_pretty(&expected_data) {
                Ok(ej) => ej,
                Err(e) => return Into::<anyhow::Error>::into(e),
            };

            if verbose {
                anyhow!("{e}\n\nactual:\n{output_json}\n\nexpected:\n{expected_json}")
            } else {
                anyhow!("{e}")
            }
        })
        .context("Comparing the actual Logstash output (lhs) with the expected output (rhs)")?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[instrument]
pub async fn run_tests(
    receiver: Receiver<Value>,
    cache_dir: PathBuf,
    rules: Vec<PathBuf>,
    test_cases: Vec<TestCase>,
    scripts: Vec<PathBuf>,
    patterns: Vec<PathBuf>,
    delete_container: bool,
    verbose: bool,
) -> anyhow::Result<()> {
    let mut test_result: anyhow::Result<()> = Ok(());

    debug!("Create the test environment");
    let mut context = TestContext::new(
        receiver,
        cache_dir,
        rules,
        scripts,
        patterns,
        delete_container,
    )
    .await
    .context("Bootstrapping the test environment")?;

    for (i, test_case) in test_cases.iter().enumerate() {
        debug!("Run test case {i}: {test_case:?}");
        let r = run_single_test(
            &context.http_client,
            &mut context.receiver,
            test_case,
            verbose,
        )
        .await
        .with_context(|| format!("Running test case {}: {}", i, test_case.input.display()));

        match r {
            Ok(()) => (),
            Err(e) => {
                test_result = Err(e);
                break;
            }
        }
    }

    context.close().await?;

    test_result
}
