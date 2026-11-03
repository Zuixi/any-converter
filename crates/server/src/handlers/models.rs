//! The model list and one model's record, as an agent asks for them.

use super::*;

pub async fn handle_models(
    State(state): State<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut available = state.config.available_models();
    // Plugin providers list their models at runtime (P6.2): fold the ids
    // into the catalog. A plugin that cannot answer only loses its entries.
    for provider in &state.config.providers {
        let Some(plugin_ref) = &provider.plugin else {
            continue;
        };
        let account = provider.account.clone().unwrap_or_default();
        match state.hosts.models_for(plugin_ref, &account).await {
            Ok(models) => {
                for model in models {
                    if !available.contains(&model.id) {
                        available.push(model.id);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "plugin models failed provider={} error={err}, omitting plugin models",
                    provider.name
                );
            }
        }
    }

    // Code Assist accounts offer their models dynamically (P8): ask the
    // account the way Gemini CLI does and fold the result in after the
    // static catalog. `list_models` itself falls back to the full static
    // table when Code Assist can't be asked; the Err arm only guards
    // against a future strictness change and keeps the static catalog.
    for provider in &state.config.providers {
        if provider.effective_api() != UpstreamApi::CodeAssist {
            continue;
        }
        let Some(account_id) = provider.account_ids().first().copied() else {
            continue;
        };
        // Project discovery happens on the regular Code Assist host even for
        // Antigravity, whose generation goes to the daily one (magpie
        // `LoadBase`).
        let base = provider.code_assist_load_base();
        match state.accounts.list_models(account_id, &base).await {
            Ok(models) => {
                for (id, _name) in models {
                    if !available.contains(&id) {
                        available.push(id);
                    }
                }
            }
            Err(err) => {
                warn!(
                    "code assist models unavailable provider={} error={err}, keeping the static catalog",
                    provider.name
                );
            }
        }
    }

    let is_codex = params.contains_key("client_version");

    if is_codex {
        let models: Vec<_> = available
            .iter()
            .map(|m| build_codex_model_object(m, state.config.model_metadata.get(m.as_str())))
            .collect();
        return Json(serde_json::json!({ "models": models }));
    }

    let data: Vec<_> = available
        .iter()
        .map(|m| {
            build_model_object(
                m,
                "any-converter",
                state.config.model_metadata.get(m.as_str()),
            )
        })
        .collect();

    Json(serde_json::json!({
        "object": "list",
        "data": data,
    }))
}

pub async fn handle_model_retrieve(
    State(state): State<Arc<AppState>>,
    Path(model_id): Path<String>,
) -> Response {
    for provider in &state.config.providers {
        if provider.model_map.contains_key(&model_id) {
            return Json(build_model_object(
                &model_id,
                &provider.name,
                state.config.model_metadata.get(&model_id),
            ))
            .into_response();
        }
    }

    api_error_response(
        StatusCode::NOT_FOUND,
        "model_not_found",
        format!("The model '{}' does not exist", model_id),
    )
}

fn build_model_object(
    model_id: &str,
    owner: &str,
    metadata: Option<&crate::config::ModelMetadata>,
) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "id": model_id,
        "slug": model_id,
        "object": "model",
        "created": 1700000000u64,
        "owned_by": owner,
    });

    if let Some(meta) = metadata {
        if let Some(cw) = meta.context_window {
            obj["context_window"] = serde_json::json!(cw);
        }
        if let Some(mcw) = meta.max_context_window {
            obj["max_context_window"] = serde_json::json!(mcw);
        }
        if let Some(sptc) = meta.supports_parallel_tool_calls {
            obj["supports_parallel_tool_calls"] = serde_json::json!(sptc);
        }
    }

    obj
}

/// Build a Codex CLI-compatible ModelInfo object.
/// Codex deserializes `/models` responses as `ModelsResponse { models: Vec<ModelInfo> }`
/// and many fields are required (no `#[serde(default)]`).
fn build_codex_model_object(
    model_id: &str,
    metadata: Option<&crate::config::ModelMetadata>,
) -> serde_json::Value {
    let ctx = metadata.and_then(|m| m.context_window).unwrap_or(272_000);
    let max_ctx = metadata.and_then(|m| m.max_context_window).unwrap_or(ctx);
    let par_tools = metadata
        .and_then(|m| m.supports_parallel_tool_calls)
        .unwrap_or(false);
    let display = metadata
        .and_then(|m| m.display_name.as_deref())
        .unwrap_or(model_id);
    let desc = metadata
        .and_then(|m| m.description.as_deref())
        .unwrap_or("");

    serde_json::json!({
        "slug": model_id,
        "display_name": display,
        "description": desc,
        "default_reasoning_level": null,
        "supported_reasoning_levels": [],
        "shell_type": "shell_command",
        "visibility": "list",
        "supported_in_api": true,
        "priority": 1,
        "base_instructions": "",
        "supports_reasoning_summaries": false,
        "support_verbosity": false,
        "truncation_policy": { "mode": "bytes", "limit": ctx },
        "supports_parallel_tool_calls": par_tools,
        "context_window": ctx,
        "max_context_window": max_ctx,
        "experimental_supported_tools": [],
    })
}
