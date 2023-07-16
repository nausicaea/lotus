use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context};
use bollard::{
    container::Config,
    image::BuildImageOptions,
    models::{BuildInfo, HealthStatusEnum, HostConfig, ImageId, PortBinding},
};
use futures_util::stream::StreamExt;
use tokio::time::sleep;

use crate::assets::{ConfigAssets, PipelineAssets};
use crate::{API_PORT, INPUT_PORT, LOCALHOST, OUTPUT_PORT};

#[derive(Debug, Clone)]
pub struct Image {
    pub(crate) id: String,
}

#[derive(Debug, Clone)]
pub struct Container {
    pub(crate) id: String,
}

pub async fn build_container_image(
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
                aux: Some(ImageId { id }),
                ..
            }) => {
                image_id = id.map(|id| Image { id });
            }
            Ok(_) => (),
            Err(bollard::errors::Error::DockerStreamError { error }) => {
                return Err(anyhow!("{}", error))
            }
            Err(e) => return Err(e.into()),
        }
    }

    image_id.ok_or(anyhow!("No container image ID was found"))
}

pub async fn create_container(
    docker: &bollard::Docker,
    image: &Image,
) -> anyhow::Result<Container> {
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

pub async fn healthy(
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
