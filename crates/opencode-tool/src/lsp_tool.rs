use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::path::PathBuf;

#[cfg(feature = "lsp")]
use lsp_types;

use crate::{Metadata, Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LspParams {
    pub operation: LspOperation,
    pub file_path: String,
    pub line: Option<u32>,
    pub character: Option<u32>,
    pub query: Option<String>,
    pub new_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
    GoToImplementation,
    TypeDefinition,
    Rename,
    Diagnostics,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
}

pub struct LspTool;

/// 1-based の位置を 0-based に変換する。
///
/// スキーマの `minimum: 1` は serde 側では検証されないため、`Some(0)` は
/// `checked_sub(1)` で明示的に拒否する。省略 (`None`) は従来どおり 0 を返す。
fn zero_based_position(value: Option<u32>) -> Result<u32, ToolError> {
    match value {
        Some(v) => v.checked_sub(1).ok_or_else(|| {
            ToolError::InvalidArguments("line and character must be >= 1".to_string())
        }),
        None => Ok(0),
    }
}

/// rename の新しい名前を解決する。
///
/// 省略時は従来どおり "new_name" を使う。空文字・空白のみは受け付けない。
/// 前後空白付きの入力は trim して LSP へ渡す (借用は入力文字列のまま)。
fn resolve_new_name(params: &LspParams) -> Result<Cow<'_, str>, ToolError> {
    match params.new_name.as_deref() {
        Some(name) if name.trim().is_empty() => Err(ToolError::InvalidArguments(
            "newName must not be empty".to_string(),
        )),
        Some(name) => Ok(Cow::Borrowed(name.trim())),
        None => Ok(Cow::Borrowed("new_name")),
    }
}

#[async_trait]
impl Tool for LspTool {
    fn id(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Language Server Protocol operations for code navigation and analysis. Supports goToDefinition, findReferences, hover, documentSymbol, workspaceSymbol, goToImplementation, typeDefinition, rename, diagnostics, prepareCallHierarchy, incomingCalls, and outgoingCalls."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["goToDefinition", "findReferences", "hover", "documentSymbol", "workspaceSymbol", "goToImplementation", "typeDefinition", "rename", "diagnostics", "prepareCallHierarchy", "incomingCalls", "outgoingCalls"],
                    "description": "The LSP operation to perform"
                },
                "filePath": {
                    "type": "string",
                    "description": "The absolute or relative path to the file"
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The line number (1-based, as shown in editors)"
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "The character offset (1-based, as shown in editors)"
                },
                "query": {
                    "type": "string",
                    "description": "Query string for workspaceSymbol operation"
                },
                "newName": {
                    "type": "string",
                    "description": "New name for rename operation"
                }
            },
            "required": ["operation", "filePath"]
        })
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: LspParams = serde_json::from_value(args)
            .map_err(|e| ToolError::InvalidArguments(format!("Invalid parameters: {}", e)))?;

        // パス確認や権限確認より前に、1-based 座標の 0 と空の rename 名を弾く。
        let line = zero_based_position(params.line)?;
        let character = zero_based_position(params.character)?;
        if matches!(&params.operation, LspOperation::Rename) {
            let _ = resolve_new_name(&params)?;
        }

        let path = PathBuf::from(&params.file_path);
        let path_str = path.to_string_lossy().to_string();

        if !path.exists() {
            return Err(ToolError::FileNotFound(format!(
                "File not found: {}",
                params.file_path
            )));
        }

        if ctx.is_external_path(&path_str) {
            ctx.ask_permission(
                crate::PermissionRequest::new("external_directory")
                    .with_pattern(&path_str)
                    .with_metadata("filepath", serde_json::json!(&path_str)),
            )
            .await?;
        }

        ctx.ask_permission(
            crate::PermissionRequest::new("lsp")
                .with_patterns(vec!["*".to_string()])
                .always_allow(),
        )
        .await?;

        #[cfg(feature = "lsp")]
        {
            execute_with_lsp(&params, &path, line, character, &ctx).await
        }

        #[cfg(not(feature = "lsp"))]
        {
            let output =
                format_lsp_placeholder(&params.operation, &params.file_path, line, character);
            let mut metadata = Metadata::new();
            metadata.insert("operation".to_string(), serde_json::json!(params.operation));
            metadata.insert("file_path".to_string(), serde_json::json!(params.file_path));

            Ok(ToolResult {
                output,
                title: format!("LSP: {:?} {}", params.operation, params.file_path),
                metadata,
                truncated: false,
            })
        }
    }
}

#[cfg(feature = "lsp")]
async fn execute_with_lsp(
    params: &LspParams,
    path: &PathBuf,
    line: u32,
    character: u32,
    ctx: &ToolContext,
) -> Result<ToolResult, ToolError> {
    use opencode_lsp::{detect_language, LspClientRegistry};
    use std::sync::Arc;

    let lsp_registry: Option<Arc<LspClientRegistry>> = ctx.lsp_registry.clone();

    let output = match &lsp_registry {
        Some(registry) => {
            // Check if any LSP client can handle this file type (mirrors TS LSP.hasClients)
            if !registry.has_clients(path).await {
                return Err(ToolError::ExecutionError(
                    "No LSP server available for this file type.".to_string(),
                ));
            }

            // Touch the file to open/refresh it in all matching LSP clients,
            // waiting for diagnostics (mirrors TS LSP.touchFile(file, true))
            registry.touch_file(path, true).await.map_err(|e| {
                ToolError::execution_with_source(format!("Failed to touch file in LSP: {}", e), e)
            })?;

            let language = detect_language(path);
            let clients = registry.list().await;
            let client = clients
                .iter()
                .find(|(id, _)| id.contains(language))
                .map(|(_, c)| c.clone());

            match client {
                Some(client) => match &params.operation {
                    LspOperation::GoToDefinition => {
                        match client.goto_definition(path, line, character).await {
                            Ok(Some(loc)) => format_location_result("Definition", loc),
                            Ok(None) => "No definition found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::FindReferences => {
                        match client.references(path, line, character).await {
                            Ok(locs) if !locs.is_empty() => locs
                                .iter()
                                .map(|l| format_location(l))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No references found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::Hover => match client.hover(path, line, character).await {
                        Ok(Some(hover)) => format_hover_result(hover),
                        Ok(None) => "No hover information available.".to_string(),
                        Err(e) => format!("LSP error: {}", e),
                    },
                    LspOperation::DocumentSymbol => match client.document_symbol(path).await {
                        Ok(symbols) if !symbols.is_empty() => symbols
                            .iter()
                            .map(|s| format!("{} ({:?})", s.name, s.kind))
                            .collect::<Vec<_>>()
                            .join("\n"),
                        Ok(_) => "No document symbols found.".to_string(),
                        Err(e) => format!("LSP error: {}", e),
                    },
                    LspOperation::WorkspaceSymbol => {
                        let query = params.query.as_deref().unwrap_or("");
                        match client.workspace_symbol(query).await {
                            Ok(symbols) if !symbols.is_empty() => symbols
                                .iter()
                                .map(|s| format!("{} ({:?})", s.name, s.kind))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No workspace symbols found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::GoToImplementation => {
                        match client.goto_implementation(path, line, character).await {
                            Ok(locs) if !locs.is_empty() => locs
                                .iter()
                                .map(|l| format_location(l))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No implementations found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::TypeDefinition => {
                        match client.type_definition(path, line, character).await {
                            Ok(locs) if !locs.is_empty() => locs
                                .iter()
                                .map(|l| format_location(l))
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No type definitions found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::Rename => {
                        let new_name = resolve_new_name(params)?;
                        match client.rename(path, line, character, &new_name).await {
                            Ok(Some(_edit)) => format!(
                                "Rename preview available. Workspace edit ready for: {}",
                                new_name
                            ),
                            Ok(None) => "Cannot rename symbol at this location.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::Diagnostics => {
                        let diags = client.get_diagnostics(path).await;
                        if diags.is_empty() {
                            "No diagnostics available.".to_string()
                        } else {
                            diags
                                .iter()
                                .map(|d| format!("{:?}: {}", d.severity, d.message))
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    LspOperation::PrepareCallHierarchy => {
                        match client.prepare_call_hierarchy(path, line, character).await {
                            Ok(items) if !items.is_empty() => items
                                .iter()
                                .map(|item| {
                                    format!(
                                        "{} ({:?}) - {}:{}",
                                        item.name,
                                        item.kind,
                                        item.uri.to_string(),
                                        item.range.start.line + 1
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No call hierarchy items found at this location.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::IncomingCalls => {
                        match client.incoming_calls(path, line, character).await {
                            Ok(calls) if !calls.is_empty() => calls
                                .iter()
                                .map(|call| {
                                    let from = &call.from;
                                    format!(
                                        "{} ({:?}) calls from {}:{}",
                                        from.name,
                                        from.kind,
                                        from.uri.to_string(),
                                        from.range.start.line + 1
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No incoming calls found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                    LspOperation::OutgoingCalls => {
                        match client.outgoing_calls(path, line, character).await {
                            Ok(calls) if !calls.is_empty() => calls
                                .iter()
                                .map(|call| {
                                    let to = &call.to;
                                    format!(
                                        "{} ({:?}) calls to {}:{}",
                                        to.name,
                                        to.kind,
                                        to.uri.to_string(),
                                        to.range.start.line + 1
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n"),
                            Ok(_) => "No outgoing calls found.".to_string(),
                            Err(e) => format!("LSP error: {}", e),
                        }
                    }
                },
                None => format!("No LSP client available for language: {}", language),
            }
        }
        None => "LSP registry not available. Enable 'lsp' feature and configure LSP servers."
            .to_string(),
    };

    let mut metadata = Metadata::new();
    metadata.insert("operation".to_string(), serde_json::json!(params.operation));
    metadata.insert("file_path".to_string(), serde_json::json!(params.file_path));

    Ok(ToolResult {
        output,
        title: format!("LSP: {:?} {}", params.operation, params.file_path),
        metadata,
        truncated: false,
    })
}

#[cfg(feature = "lsp")]
fn format_location(loc: &lsp_types::Location) -> String {
    let path = loc.uri.to_string();
    let line = loc.range.start.line + 1;
    let character = loc.range.start.character + 1;
    format!("{}:{}:{}", path, line, character)
}

#[cfg(feature = "lsp")]
fn format_location_result(label: &str, loc: lsp_types::Location) -> String {
    format!("{} found at:\n{}", label, format_location(&loc))
}

#[cfg(feature = "lsp")]
fn format_hover_result(hover: lsp_types::Hover) -> String {
    match hover.contents {
        lsp_types::HoverContents::Scalar(markup) => format_markup(markup),
        lsp_types::HoverContents::Array(markups) => markups
            .into_iter()
            .map(format_markup)
            .collect::<Vec<_>>()
            .join("\n"),
        lsp_types::HoverContents::Markup(content) => content.value,
    }
}

#[cfg(feature = "lsp")]
fn format_markup(markup: lsp_types::MarkedString) -> String {
    match markup {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => {
            format!("```{}\n{}\n```", ls.language, ls.value)
        }
    }
}

#[cfg(not(feature = "lsp"))]
fn format_lsp_placeholder(
    operation: &LspOperation,
    file_path: &str,
    line: u32,
    character: u32,
) -> String {
    match operation {
        LspOperation::GoToDefinition => {
            format!("LSP goToDefinition at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::FindReferences => {
            format!("LSP findReferences at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::Hover => {
            format!("LSP hover at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::DocumentSymbol => {
            format!("Document symbols for: {}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path)
        }
        LspOperation::WorkspaceSymbol => {
            "LSP workspace symbols\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.".to_string()
        }
        LspOperation::GoToImplementation => {
            format!("LSP goToImplementation at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::TypeDefinition => {
            format!("LSP typeDefinition at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::Rename => {
            format!("LSP rename at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::Diagnostics => {
            format!("Diagnostics for: {}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path)
        }
        LspOperation::PrepareCallHierarchy => {
            format!("LSP prepareCallHierarchy at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::IncomingCalls => {
            format!("LSP incomingCalls at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
        LspOperation::OutgoingCalls => {
            format!("LSP outgoingCalls at {}:{}:{}\n\nEnable 'lsp' feature and configure LSP servers for real LSP support.", file_path, line + 1, character + 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> ToolContext {
        ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            "/tmp".to_owned(),
        )
    }

    #[test]
    fn identity_and_schema() {
        let tool = LspTool;
        assert_eq!(tool.id(), "lsp");
        assert_eq!(tool.parameters()["required"][0], "operation");
        assert_eq!(tool.parameters()["required"][1], "filePath");
    }

    #[test]
    fn params_use_camel_case() {
        let params: LspParams = serde_json::from_value(serde_json::json!({
            "operation": "rename",
            "filePath": "src/main.rs",
            "line": 3,
            "character": 5,
            "newName": "renamed"
        }))
        .unwrap();
        assert!(matches!(params.operation, LspOperation::Rename));
        assert_eq!(params.file_path, "src/main.rs");
        assert_eq!(params.line, Some(3));
        assert_eq!(params.character, Some(5));
        assert_eq!(params.new_name.as_deref(), Some("renamed"));
    }

    #[test]
    fn all_operations_deserialize() {
        let operations = [
            "goToDefinition",
            "findReferences",
            "hover",
            "documentSymbol",
            "workspaceSymbol",
            "goToImplementation",
            "typeDefinition",
            "rename",
            "diagnostics",
            "prepareCallHierarchy",
            "incomingCalls",
            "outgoingCalls",
        ];
        for op in operations {
            let json = serde_json::json!({ "operation": op, "filePath": "a.rs" });
            serde_json::from_value::<LspParams>(json)
                .unwrap_or_else(|e| panic!("failed to deserialize {}: {}", op, e));
        }
    }

    #[tokio::test]
    async fn execute_rejects_missing_file() {
        let args = serde_json::json!({
            "operation": "hover",
            "filePath": "/nonexistent/lsp-file.rs"
        });
        let error = LspTool.execute(args, context()).await.unwrap_err();
        assert!(matches!(error, ToolError::FileNotFound(_)));
    }

    #[tokio::test]
    async fn execute_rejects_invalid_params() {
        let error = LspTool
            .execute(serde_json::json!({}), context())
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }

    #[cfg(not(feature = "lsp"))]
    #[tokio::test]
    async fn execute_returns_placeholder_without_lsp_feature() {
        let dir = std::env::temp_dir().join(format!("opencode-lsp-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let ctx = ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            dir.to_string_lossy().to_string(),
        );

        let result = LspTool
            .execute(
                serde_json::json!({
                    "operation": "hover",
                    "filePath": file.to_string_lossy(),
                    "line": 1,
                    "character": 1
                }),
                ctx,
            )
            .await
            .unwrap();

        assert!(result.output.contains("LSP hover"));
        assert!(result.output.contains("Enable 'lsp' feature"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zero_based_position_converts_and_rejects_zero() {
        assert_eq!(zero_based_position(Some(1)).unwrap(), 0);
        assert_eq!(zero_based_position(Some(10)).unwrap(), 9);
        assert_eq!(zero_based_position(None).unwrap(), 0);
        assert!(matches!(
            zero_based_position(Some(0)),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[tokio::test]
    async fn execute_rejects_zero_line_before_file_check() {
        let args = serde_json::json!({
            "operation": "hover",
            "filePath": "/nonexistent/lsp-file.rs",
            "line": 0
        });
        let error = LspTool.execute(args, context()).await.unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_rejects_zero_character_before_file_check() {
        let args = serde_json::json!({
            "operation": "hover",
            "filePath": "/nonexistent/lsp-file.rs",
            "character": 0
        });
        let error = LspTool.execute(args, context()).await.unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }

    #[test]
    fn resolve_new_name_defaults_and_rejects_blank() {
        let parse = |new_name: Option<&str>| {
            let mut value = serde_json::json!({
                "operation": "rename",
                "filePath": "a.rs"
            });
            if let Some(name) = new_name {
                value["newName"] = serde_json::json!(name);
            }
            let params: LspParams = serde_json::from_value(value).unwrap();
            resolve_new_name(&params).map(|cow| cow.into_owned())
        };

        assert_eq!(parse(None).unwrap(), "new_name");
        assert_eq!(parse(Some("renamed")).unwrap(), "renamed");
        assert_eq!(parse(Some("  foo  ")).unwrap(), "foo");
        assert!(matches!(
            parse(Some("")),
            Err(ToolError::InvalidArguments(_))
        ));
        assert!(matches!(
            parse(Some("   ")),
            Err(ToolError::InvalidArguments(_))
        ));
    }

    #[tokio::test]
    async fn execute_rejects_blank_rename_name() {
        let args = serde_json::json!({
            "operation": "rename",
            "filePath": "/nonexistent/lsp-file.rs",
            "newName": "   "
        });
        let error = LspTool.execute(args, context()).await.unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments(_)));
    }

    #[tokio::test]
    async fn execute_accepts_rename_without_new_name() {
        let dir =
            std::env::temp_dir().join(format!("opencode-lsp-rename-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "fn main() {}").unwrap();
        let ctx = ToolContext::new(
            "session".to_owned(),
            "message".to_owned(),
            dir.to_string_lossy().to_string(),
        );

        let result = LspTool
            .execute(
                serde_json::json!({
                    "operation": "rename",
                    "filePath": file.to_string_lossy(),
                    "line": 1,
                    "character": 1
                }),
                ctx,
            )
            .await;
        assert!(result.is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
