use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "logstash/config"]
pub struct ConfigAssets;

#[derive(RustEmbed)]
#[folder = "logstash/pipeline"]
pub struct PipelineAssets;
