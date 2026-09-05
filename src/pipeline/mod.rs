//! Stage-aware REST API pipeline executor.
//!
//! Sequential stages run one call at a time, each with access to all prior
//! outputs. Parallel stages fire all their calls concurrently via
//! `futures::future::join_all` — wall-clock cost = slowest call in the stage.
//!
//! Auth fix: credential templates (e.g. "${steps.authenticate}") are resolved
//! through the context-aware `expr` interpolators before building auth headers.
//!
//! Injection-safe construction (#11): every caller-derived value is encoded for
//! its surrounding syntax — URLs are percent-encoded, JSON bodies are built
//! position-aware, header values are CR/LF-stripped — and an unresolved or
//! malformed expression is a hard error, never a silent empty string.
//!
//! Global deadline: the configured `pipelineTimeoutMs` caps the total wall-clock
//! duration of all stages combined. Completed stages are NOT rolled back when a
//! later stage fails — operators should use idempotency keys or compensating
//! actions for mutating pipelines.

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

    #[error("call '{call}' response of {size} bytes exceeded limit of {limit} bytes")]
    ResponseTooLarge {
        call: String,
        size: usize,
        limit: usize,
    },

    #[error("call '{call}' could not build request: {message}")]
    Interpolation { call: String, message: String },

    #[error("call '{call}' timed out")]
    Timeout { call: String },

    #[error("pipeline exceeded global deadline")]
    GlobalTimeout,
}

/// Maps each call name to its registered Flex Gateway `Service` handle.
pub type ServiceMap = HashMap<String, Rc<Service>>;

/// Execute the full pipeline for a single `tools/call` invocation.
///
/// `args` is the (already DataWeave-transformed) input from the MCP client.
/// `incoming_auth` is the raw Authorization header value from the MCP request,
/// forwarded as-is for calls with authType=passthrough.
///
/// ## Cancellation and rollback
///
/// When a stage fails, execution stops and the error is returned. Stages that
/// already completed are NOT rolled back — their side effects (e.g. order
/// creation, payment charges) cannot be undone by this policy. Operators should
/// design mutating pipelines with idempotency keys or compensating actions.
pub async fn run_pipeline(
    config: &PolicyConfig,
    services: &ServiceMap,
    args: &Value,
    http_client: &HttpClient,
    incoming_auth: Option<&str>,
) -> Result<PipelineResult, PipelineError> {
    let mut step_outputs: StepOutputs = HashMap::new();

    // Track elapsed time against the global pipeline deadline.
    let deadline_ms = config.pipeline_timeout_ms as u64;
    let mut elapsed_ms: u64 = 0;

    for stage in &config.stages {
        match stage {
            Stage::Sequential(call) => {
                if elapsed_ms >= deadline_ms {
                    return Err(PipelineError::GlobalTimeout);
                }
                let remaining_ms = deadline_ms - elapsed_ms;
                let call_timeout_ms =
                    call.timeout_ms.unwrap_or(config.per_request_timeout_ms) as u64;
                let effective_timeout_ms = call_timeout_ms.min(remaining_ms);

                let service = services.get(&call.name).expect("service map out of sync");
                let start = std::time::Instant::now();
                let output = execute_call(
                    call,
                    service,
                    args,
                    &step_outputs,
                    http_client,
                    effective_timeout_ms as u32,
                    incoming_auth,
                    config.max_response_bytes,
                )
                .await;
                elapsed_ms += start.elapsed().as_millis() as u64;

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
                if elapsed_ms >= deadline_ms {
                    return Err(PipelineError::GlobalTimeout);
                }
                let remaining_ms = deadline_ms - elapsed_ms;

                // Snapshot outputs so all parallel calls see the same prior state.
                let snapshot = step_outputs.clone();

                let futures: Vec<_> = calls
                    .iter()
                    .map(|call| {
                        let service = services.get(&call.name).expect("service map out of sync");
                        let call_timeout_ms =
                            call.timeout_ms.unwrap_or(config.per_request_timeout_ms) as u64;
                        let effective_timeout_ms = call_timeout_ms.min(remaining_ms);
                        execute_call(
                            call,
                            service,
                            args,
                            &snapshot,
                            http_client,
                            effective_timeout_ms as u32,
                            incoming_auth,
                            config.max_response_bytes,
                        )
                    })
                    .collect();

                let start = std::time::Instant::now();
                let results = join_all(futures).await;
                elapsed_ms += start.elapsed().as_millis() as u64;

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
    timeout_ms: u32,
    incoming_auth: Option<&str>,
    max_response_bytes: usize,
) -> Result<Value, PipelineError> {
    // Injection-safe construction (#11): each field is interpolated for its own
    // syntax, and an unresolved/malformed expression is a hard error rather than
    // a silent empty string.
    let interp_err = |e: expr::InterpError| PipelineError::Interpolation {
        call: call.name.clone(),
        message: e.to_string(),
    };

    let path = expr::interpolate_url(&call.path, args, step_outputs).map_err(interp_err)?;
    let body_str = match call.body_template.as_deref() {
        Some(t) => expr::interpolate_json_body(t, args, step_outputs).map_err(interp_err)?,
        None => String::new(),
    };

    let mut headers: Vec<(String, String)> = vec![
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/json".into()),
    ];

    // Auth — resolve credential templates through expr::interpolate before
    // building the auth header so ${steps.*} tokens work correctly.
    match &call.auth {
        AuthConfig::Passthrough => {
            if let Some(auth_val) = incoming_auth {
                headers.push(("authorization".into(), auth_val.to_string()));
            }
        }
        AuthConfig::None | AuthConfig::CustomHeaders => {}
        auth => {
            let raw_cred = auth.raw_credential().unwrap_or("");
            let resolved_cred =
                expr::interpolate_header(raw_cred, args, step_outputs).map_err(interp_err)?;
            if let Some((k, v)) = auth.auth_header_resolved(&resolved_cred) {
                headers.push((k, v));
            }
        }
    }

    // Extra / customHeaders — always interpolated (CR/LF stripped).
    for (k, v) in &call.extra_headers {
        let resolved_v = expr::interpolate_header(v, args, step_outputs).map_err(interp_err)?;
        headers.push((k.clone(), resolved_v));
    }

    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();

    let timeout = Duration::from_millis(timeout_ms.into());

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

    // Payload-size cap (#16): a downstream response larger than the configured
    // limit fails the call rather than being buffered/parsed unbounded.
    if body_bytes.len() > max_response_bytes {
        return Err(PipelineError::ResponseTooLarge {
            call: call.name.clone(),
            size: body_bytes.len(),
            limit: max_response_bytes,
        });
    }

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
