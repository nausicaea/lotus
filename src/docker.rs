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
use tracing::instrument;

use crate::{
    assets::{ConfigAssets, PipelineAssets},
    PATTERNS_DIR, SCRIPTS_DIR,
};
use crate::{
    API_PORT, FQAN, IMAGE_ARCHIVE_NAME, INPUT_PORT, INPUT_TEMPLATE_NAME, LOCALHOST, OUTPUT_PORT,
    OUTPUT_TEMPLATE_NAME, PIPELINE_NAME,
};

#[derive(Debug, Clone)]
pub struct Image {
    pub(crate) id: String,
}

#[derive(Debug, Clone)]
pub struct Container {
    pub(crate) id: String,
}

pub fn build_image_archive(
    cache_dir: &Path,
    rules: &[PathBuf],
    scripts: &[PathBuf],
    patterns: &[PathBuf],
) -> anyhow::Result<PathBuf> {
    // Create the tar archive
    let archive_path = cache_dir.join(IMAGE_ARCHIVE_NAME);
    let archive =
        File::create(&archive_path).context("Creating the container image tar archive file")?;
    let mut ark = tar::Builder::new(archive);
    ark.mode(tar::HeaderMode::Deterministic);

    // Prepare the Handlebars templating context
    let ctx = handlebars::Context::wraps(serde_json::json!({
        "input_port": INPUT_PORT,
        "output_port": OUTPUT_PORT,
        "api_port": API_PORT,
        "pipeline_name": PIPELINE_NAME,
        "scripts_dir": SCRIPTS_DIR,
        "patterns_dir": PATTERNS_DIR,
    }))
    .context("Creating the Handlebars variable context")?;

    // Prepare the Handlebars renderer
    let mut hbs = handlebars::Handlebars::new();
    hbs.set_dev_mode(true);
    hbs.set_strict_mode(true);
    hbs.register_embed_templates::<ConfigAssets>()
        .context("Loading the Logstash config assets")?;

    // Template all fixed Logstash configuration and add them to the archive
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

    // Concatenate each individual rule to a complete pipeline file
    let pipeline_path = cache_dir.join(PIPELINE_NAME);
    {
        let mut hbs = handlebars::Handlebars::new();
        hbs.set_dev_mode(true);
        hbs.set_strict_mode(true);
        hbs.register_embed_templates::<PipelineAssets>()
            .context("Loading the Logstash pipeline assets")?;
        let mut pipeline = File::create(&pipeline_path)
            .with_context(|| format!("Creating the pipeline file: {}", pipeline_path.display()))?;
        hbs.render_with_context_to_write(INPUT_TEMPLATE_NAME, &ctx, &mut pipeline)
            .context("Rendering the template input.conf to the pipeline file")?;
        for rule in rules {
            std::io::copy(
                &mut File::open(rule)
                    .with_context(|| format!("Opening the rule file: {}", rule.display()))?,
                &mut pipeline,
            )
            .context("Adding the rule file to the pipeline file")?;
        }
        hbs.render_with_context_to_write(OUTPUT_TEMPLATE_NAME, &ctx, &mut pipeline)
            .context("Rendering the template output.conf to the pipeline file")?;
    }

    // Append the complete pipeline to the archive
    ark.append_file(
        PIPELINE_NAME,
        &mut File::open(&pipeline_path).context("Opening the pipeline file")?,
    )
    .context("Adding the pipeline file 'logstash.conf' to the tar archive")?;

    // Append all ruby scripts to the archive
    let scripts_base_dir = PathBuf::from(SCRIPTS_DIR);
    for script in scripts {
        let script_name = scripts_base_dir.join(script.file_name().ok_or_else(|| {
            anyhow!("Finding file name of ruby script file {}", script.display())
        })?);
        ark.append_file(
            &script_name,
            &mut File::open(script)
                .with_context(|| format!("Opening ruby script file: {}", script.display()))?,
        )
        .with_context(|| {
            format!(
                "Appending the ruby script to the archive: {}",
                script_name.display()
            )
        })?;
    }
    // Always create a dummy script file so that the output directory exists
    ark.append_file(
        scripts_base_dir.join(".gitkeep"),
        &mut tempfile::tempfile().context("Creating a dummy script file")?,
    )
    .context("Appending a dummy script file to the archive")?;

    // Append all grok patterns to the archive
    let patterns_base_dir = PathBuf::from(PATTERNS_DIR);
    for pattern in patterns {
        let pattern_name = patterns_base_dir.join(pattern.file_name().ok_or_else(|| {
            anyhow!(
                "Finding file name of grok pattern file {}",
                pattern.display()
            )
        })?);
        ark.append_file(
            &pattern_name,
            &mut File::open(pattern)
                .with_context(|| format!("Opening grok pattern file: {}", pattern.display()))?,
        )
        .with_context(|| {
            format!(
                "Appending the grok pattern file to the archive: {}",
                pattern_name.display()
            )
        })?;
    }
    // Always create a dummy pattern file so that the output directory exists
    ark.append_file(
        patterns_base_dir.join(".gitkeep"),
        &mut tempfile::tempfile().context("Creating a dummy pattern file")?,
    )
    .context("Appending a dummy pattern file to the archive")?;

    // Close the archive
    ark.finish().context("Finishing the tar archive")?;

    Ok(archive_path)
}

#[instrument]
pub async fn build_container_image(
    docker: &bollard::Docker,
    cache_dir: &Path,
    rules: &[PathBuf],
    scripts: &[PathBuf],
    patterns: &[PathBuf],
) -> anyhow::Result<Image> {
    // Copy the static files over to the cache directory and build the tar archive
    let archive_path = build_image_archive(cache_dir, rules, scripts, patterns)
        .context("Creating the image archive")?;

    // Build the container image from the tar archive
    let mut archive_buffer = Vec::new();
    File::open(&archive_path)
        .context("Opening the archive file")?
        .read_to_end(&mut archive_buffer)
        .context("Reading the archive file into memory")?;
    let cache_name = cache_dir
        .file_name()
        .and_then(|f| f.to_str())
        .ok_or(anyhow!("Cannot determine the name of the cache directory"))?;
    let image_tag = format!("{}/{}-{}:latest", FQAN[1], FQAN[2], cache_name);
    let mut builder_stream = docker.build_image::<String>(
        BuildImageOptions {
            t: image_tag.clone(),
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
                return Err(anyhow!(
                    "Docker stream error when building image {}: {}",
                    &image_tag,
                    error
                ))
            }
            Err(e) => {
                return Err(anyhow!(
                    "Unspecified error building image {}: {}",
                    &image_tag,
                    e
                ))
            }
        }
    }

    image_id.ok_or(anyhow!("No container image ID was found"))
}

#[instrument]
pub async fn create_container(
    docker: &bollard::Docker,
    image: &Image,
    delete_container: bool,
) -> anyhow::Result<Container> {
    let response = docker
        .create_container::<String, String>(
            None,
            Config {
                image: Some(image.id.clone()),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                host_config: Some(HostConfig {
                    auto_remove: Some(delete_container),
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

#[instrument]
pub async fn healthy(
    docker: &bollard::Docker,
    container: &Container,
    retries: usize,
    delay: Duration,
) -> anyhow::Result<()> {
    use HealthStatusEnum::*;

    let mut curr_retries = 0;
    'retry: loop {
        let inspect = docker
            .inspect_container(&container.id, None)
            .await
            .context("Inspecting the Docker container")?;

        let health = inspect
            .state
            .and_then(|s| s.health)
            .and_then(|h| h.status.map(|hs| (h, hs)));
        match health {
            Some((_, HEALTHY)) => break 'retry,
            Some((_, STARTING)) => {
                curr_retries += 1;
                if curr_retries >= retries {
                    return Err(anyhow!(
                        "Failed to determine the Docker container health status after {} retries.",
                        retries
                    ));
                }
                sleep(delay).await;
            }
            Some((h, hs)) => {
                return Err(anyhow!(
                    "Unexpected Docker container health status '{}': {:?}",
                    hs,
                    h,
                ));
            }
            None => {
                return Err(anyhow!("No Docker container health status was found"));
            }
        }
    }

    Ok(())
}
