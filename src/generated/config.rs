use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Headers0Config {
    #[serde(alias = "name")]
    pub name: String,
    #[serde(alias = "value")]
    pub value: String,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Calls0Config {
    #[serde(alias = "apiKey")]
    pub api_key: Option<String>,
    #[serde(alias = "authType")]
    pub auth_type: Option<String>,
    #[serde(alias = "bodyTemplate")]
    pub body_template: Option<String>,
    #[serde(alias = "endpoint", deserialize_with = "pdk::serde::deserialize_service")]
    pub endpoint: pdk::hl::Service,
    #[serde(alias = "headerName")]
    pub header_name: Option<String>,
    #[serde(alias = "headers")]
    pub headers: Option<Vec<Headers0Config>>,
    #[serde(alias = "maskInOutput")]
    pub mask_in_output: Option<bool>,
    #[serde(alias = "method")]
    pub method: Option<String>,
    #[serde(alias = "name")]
    pub name: String,
    #[serde(alias = "outputExtract")]
    pub output_extract: Option<String>,
    #[serde(alias = "password")]
    pub password: Option<String>,
    #[serde(alias = "path")]
    pub path: Option<String>,
    #[serde(alias = "stopOnError")]
    pub stop_on_error: Option<bool>,
    #[serde(alias = "timeoutMs")]
    pub timeout_ms: Option<i64>,
    #[serde(alias = "token")]
    pub token: Option<String>,
    #[serde(alias = "username")]
    pub username: Option<String>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Stages0Config {
    #[serde(alias = "calls")]
    pub calls: Vec<Calls0Config>,
    #[serde(alias = "parallel")]
    pub parallel: Option<bool>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(
        alias = "inputTransform",
        default,
        deserialize_with = "de_input_transform_0"
    )]
    pub input_transform: Option<pdk::script::Script>,
    #[serde(alias = "mcpEndpoint")]
    pub mcp_endpoint: Option<String>,
    #[serde(
        alias = "outputTransform",
        default,
        deserialize_with = "de_output_transform_1"
    )]
    pub output_transform: Option<pdk::script::Script>,
    #[serde(alias = "perRequestTimeoutMs")]
    pub per_request_timeout_ms: Option<i64>,
    #[serde(alias = "pipelineTimeoutMs")]
    pub pipeline_timeout_ms: Option<i64>,
    #[serde(alias = "stages")]
    pub stages: Vec<Stages0Config>,
    #[serde(alias = "strictMode")]
    pub strict_mode: Option<bool>,
    #[serde(alias = "toolDescription")]
    pub tool_description: String,
    #[serde(alias = "toolInputSchema")]
    pub tool_input_schema: Option<String>,
    #[serde(alias = "toolName")]
    pub tool_name: String,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    let config: Config = serde_json::from_slice(abi.get_configuration())
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse configuration '{}'. Cause: {}",
                String::from_utf8_lossy(abi.get_configuration()), err
            )
        })?;
    for current in config.stages {
        for current in current.calls {
            abi.service_create(current.endpoint)?;
        }
    }
    abi.setup()?;
    Ok(())
}
fn de_input_transform_0<'de, D>(
    deserializer: D,
) -> Result<Option<pdk::script::Script>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let exp: Option<pdk::script::Expression> = serde::de::Deserialize::deserialize(
        deserializer,
    )?;
    exp.map(|exp| {
            pdk::script::ScriptingEngine::script(&exp)
                .input(pdk::script::Input::Payload(pdk::script::Format::Json))
                .compile()
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}
fn de_output_transform_1<'de, D>(
    deserializer: D,
) -> Result<Option<pdk::script::Script>, D::Error>
where
    D: serde::de::Deserializer<'de>,
{
    let exp: Option<pdk::script::Expression> = serde::de::Deserialize::deserialize(
        deserializer,
    )?;
    exp.map(|exp| {
            pdk::script::ScriptingEngine::script(&exp)
                .input(pdk::script::Input::Payload(pdk::script::Format::Json))
                .compile()
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}
