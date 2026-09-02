//! Stage-aware REST API pipeline executor.
//!
//! Sequential stages run one call at a time, each with access to all prior
//! outputs. Parallel stages fire all their calls concurrently via
//! `futures::future::join_all` — wall-clock cost = slowest call in the stage.
//!
//! Auth fix: credential templates (e.g. "${steps.authenticate}") are resolved
//! through `expr::interpolate` before building auth headers, so dynamic tokens
//! from earlier pipeline steps work correctly.
//!
//! Passthrough auth: when authType=passthrough the incoming Authorization
//! header from the MCP client is forwarded unchanged to the backend call.

pub mod expr;

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures::future::join_all;
use pdk::hl::{HttpClient, Service};
use pdk::logger;
use serde_json::Value;

use crate::config::{AuthConfig, CallDef, PolicyConfig, Stage};
use expr::StepOutputs;

pub struct PipelineResult {
    /// Composite JSON object of all step outputs keyed by call name.
    pub final_output: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("call '{call}' transport error: {message}")]
    Transport { call: String, message: String },

    #[error("call '{call}' returned HTTP {status}")]
    HttpStatus { call: String, status: u32 },

    #[error("call '{call}' returned non-JSON body: {message}")]
    BadJson { call: String, message: String },
}

/// Maps each call name to its registered Flex Gateway `Service` handle.
pub type ServiceMap = HashMap<String, Rc<Service>>;

/// Execute the full pipeline for a single `tools/call` invocation.
///
/// `args` is the (already DataWeave-transformed) input from the MCP client.
/// `incoming_auth` is the raw Authorization header value from the MCP request,
/// forwarded as-is for calls with authType=passthrough.
pub async fn run_pipeline(
    config: &PolicyConfig,
    services: &ServiceMap,
    args: &Value,
    http_client: &HttpClient,
    incoming_auth: Option<&str>,
) -> Result<PipelineResult, PipelineError> {
    let mut step_outputs: StepOutputs = HashMap::new();

    for stage in &config.stages {
        match stage {
            Stage::Sequential(call) => {
                let service = services.get(&call.name).expect("service map out of sync");
                let output = execute_call(
                    call,
                    service,
                    args,
                    &step_outputs,
                    http_client,
                    config,
                    incoming_auth,
                )
                .await;

                match output {
                    Ok(v) => {
                        step_outputs.insert(call.name.clone(), v);
                    }
                    Err(e) if call.stop_on_error => return Err(e),
                    Err(e) => {
                        logger::warn!(
                            "mcp-tool-composer: call '{}' failed (stopOnError=false): {}",
                            call.name,
                            e
                        );
                        step_outputs.insert(call.name.clone(), Value::Null);
                    }
                }
            }

            Stage::Parallel(calls) => {
                // Snapshot outputs so all parallel calls see the same prior state.
                let snapshot = step_outputs.clone();

                let futures: Vec<_> = calls
                    .iter()
                    .map(|call| {
                        let service =
                            services.get(&call.name).expect("service map out of sync");
                        execute_call(
                            call,
                            service,
                            args,
                            &snapshot,
                            http_client,
                            config,
                            incoming_auth,
                        )
                    })
                    .collect();

                let results = join_all(futures).await;

                let mut fatal: Option<PipelineError> = None;
                for (call, result) in calls.iter().zip(results.into_iter()) {
                    match result {
                        Ok(v) => {
                            step_outputs.insert(call.name.clone(), v);
                        }
                        Err(e) if call.stop_on_error => {
                            fatal = Some(e);
                        }
                        Err(e) => {
                            logger::warn!(
                                "mcp-tool-composer: call '{}' failed (stopOnError=false): {}",
                                call.name,
                                e
                            );
                            step_outputs.insert(call.name.clone(), Value::Null);
                        }
                    }
                }
                if let Some(err) = fatal {
                    return Err(err);
                }
            }
        }
    }

    // Build final output, replacing masked call values with "***".
    let masked_names: std::collections::HashSet<&str> = config
        .all_calls()
        .filter(|c| c.mask_in_output)
        .map(|c| c.name.as_str())
        .collect();

    let final_output = Value::Object(
        step_outputs
            .iter()
            .map(|(k, v)| {
                if masked_names.contains(k.as_str()) {
                    (k.clone(), Value::String("***".into()))
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect(),
    );

    Ok(PipelineResult { final_output })
}

async fn execute_call(
    call: &CallDef,
    service: &Service,
    args: &Value,
    step_outputs: &StepOutputs,
    http_client: &HttpClient,
    config: &PolicyConfig,
    incoming_auth: Option<&str>,
) -> Result<Value, PipelineError> {
    let path = expr::interpolate(&call.path, args, step_outputs);
    let body_str = call
        .body_template
        .as_deref()
        .map(|t| expr::interpolate(t, args, step_outputs))
        .unwrap_or_default();

    let mut headers: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/json".into()),
    ];

    // Auth — resolve credential templates through expr::interpolate before building
    // the auth header. This fixes the bug where "${steps.authenticate}" was never
    // expanded in the old auth_header() call.
    match &call.auth {
        AuthConfig::Passthrough => {
            if let Some(auth_val) = incoming_auth {
                headers.push(("authorization".into(), auth_val.to_string()));
            }
        }
        AuthConfig::None | AuthConfig::CustomHeaders => {}
        auth => {
            let raw_cred = auth.raw_credential().unwrap_or("");
            let resolved_cred = expr::interpolate(raw_cred, args, step_outputs);
            if let Some((k, v)) = auth.auth_header_resolved(&resolved_cred) {
                headers.push((k, v));
            }
        }
    }

    // Extra / customHeaders — always interpolated.
    for (k, v) in &call.extra_headers {
        let resolved_v = expr::interpolate(v, args, step_outputs);
        headers.push((k.clone(), resolved_v));
    }

    let header_refs: Vec<(&str, &str)> =
        headers.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

    let timeout = Duration::from_millis(
        call.timeout_ms.unwrap_or(config.per_request_timeout_ms).into(),
    );

    logger::debug!(
        "mcp-tool-composer: call '{}' {} {}",
        call.name,
        call.method.as_str(),
        path
    );

    let response = http_client
        .request(service)
        .path(&path)
        .headers(header_refs)
        .body(body_str.as_bytes())
        .timeout(timeout)
        .send(call.method.as_str())
        .await
        .map_err(|e| PipelineError::Transport {
            call: call.name.clone(),
            message: e.to_string(),
        })?;

    let status = response.status_code();
    if !(200..300).contains(&(status as usize)) {
        return Err(PipelineError::HttpStatus {
            call: call.name.clone(),
            status,
        });
    }

    let body_bytes = response.body();
    let json: Value = serde_json::from_slice(body_bytes).map_err(|e| PipelineError::BadJson {
        call: call.name.clone(),
        message: e.to_string(),
    })?;

    let output = match &call.output_extract {
        None => json,
        Some(dot_path) => expr::traverse_value(&json, dot_path)
            .cloned()
            .unwrap_or(Value::Null),
    };

    Ok(output)
}
