use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpTool {
    pub server_name: String,
    pub name: String,
    pub full_name: String,
    pub description: Option<String>,
    pub input_schema: serde_json::Value,
}

impl McpTool {
    pub fn new(
        server_name: &str,
        name: &str,
        description: Option<String>,
        input_schema: serde_json::Value,
    ) -> Self {
        Self {
            server_name: server_name.to_string(),
            name: name.to_string(),
            full_name: format!("{}_{}", server_name, name),
            description,
            input_schema,
        }
    }
}

pub struct McpToolRegistry {
    tools: RwLock<HashMap<String, McpTool>>,
}

impl McpToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }

    pub async fn register(&self, tool: McpTool) {
        let mut tools = self.tools.write().await;
        tools.insert(tool.full_name.clone(), tool);
    }

    pub async fn register_batch(
        &self,
        server_name: &str,
        tools: Vec<crate::protocol::ToolDefinition>,
    ) {
        let mut registry = self.tools.write().await;
        for tool_def in tools {
            let mcp_tool = McpTool::new(
                server_name,
                &tool_def.name,
                tool_def.description,
                tool_def.input_schema,
            );
            registry.insert(mcp_tool.full_name.clone(), mcp_tool);
        }
    }

    pub async fn get(&self, full_name: &str) -> Option<McpTool> {
        let tools = self.tools.read().await;
        tools.get(full_name).cloned()
    }

    pub async fn list(&self) -> Vec<McpTool> {
        let tools = self.tools.read().await;
        tools.values().cloned().collect()
    }

    pub async fn list_for_server(&self, server_name: &str) -> Vec<McpTool> {
        let tools = self.tools.read().await;
        tools
            .values()
            .filter(|t| t.server_name == server_name)
            .cloned()
            .collect()
    }

    pub async fn clear_server(&self, server_name: &str) {
        let mut tools = self.tools.write().await;
        tools.retain(|_, t| t.server_name != server_name);
    }

    pub async fn clear(&self) {
        let mut tools = self.tools.write().await;
        tools.clear();
    }
}

impl Default for McpToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ToolDefinition;

    fn dummy_schema() -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": { "q": { "type": "string" } } })
    }

    #[test]
    fn new_builds_full_name_and_preserves_fields() {
        let with_description = McpTool::new(
            "server-a",
            "search",
            Some("search tool".to_string()),
            dummy_schema(),
        );
        assert_eq!(with_description.server_name, "server-a");
        assert_eq!(with_description.name, "search");
        assert_eq!(with_description.full_name, "server-a_search");
        assert_eq!(with_description.description.as_deref(), Some("search tool"));
        assert_eq!(with_description.input_schema, dummy_schema());

        let without_description = McpTool::new("server-b", "ping", None, serde_json::json!({}));
        assert_eq!(without_description.full_name, "server-b_ping");
        assert_eq!(without_description.description, None);
    }

    #[tokio::test]
    async fn register_then_get_round_trips_by_full_name() {
        let registry = McpToolRegistry::new();
        let tool = McpTool::new("server-a", "ping", Some("ping".to_string()), dummy_schema());

        registry.register(tool.clone()).await;

        let stored = registry
            .get("server-a_ping")
            .await
            .expect("registered tool must be retrievable by its full_name");
        assert_eq!(stored.server_name, tool.server_name);
        assert_eq!(stored.name, tool.name);
        assert_eq!(stored.full_name, tool.full_name);
        assert_eq!(stored.description, tool.description);
        assert_eq!(stored.input_schema, tool.input_schema);
        assert!(registry.get("ping").await.is_none());
        assert!(registry.get("server-a_missing").await.is_none());
    }

    #[tokio::test]
    async fn register_same_full_name_overwrites_previous_tool() {
        let registry = McpToolRegistry::new();
        registry
            .register(McpTool::new(
                "server-a",
                "ping",
                Some("old".to_string()),
                serde_json::json!({ "type": "object" }),
            ))
            .await;
        registry
            .register(McpTool::new(
                "server-a",
                "ping",
                Some("new".to_string()),
                serde_json::json!({ "type": "string" }),
            ))
            .await;

        let stored = registry
            .get("server-a_ping")
            .await
            .expect("tool must remain registered after overwrite");
        assert_eq!(stored.description.as_deref(), Some("new"));
        assert_eq!(stored.input_schema, serde_json::json!({ "type": "string" }));
        assert_eq!(registry.list().await.len(), 1);
    }

    #[tokio::test]
    async fn register_batch_converts_definitions_with_server_prefix() {
        let registry = McpToolRegistry::new();
        let definitions = vec![
            ToolDefinition {
                name: "alpha".to_string(),
                description: Some("alpha tool".to_string()),
                input_schema: serde_json::json!({ "type": "object" }),
            },
            ToolDefinition {
                name: "beta".to_string(),
                description: None,
                input_schema: serde_json::json!({ "type": "array" }),
            },
        ];

        registry.register_batch("server-b", definitions).await;

        let alpha = registry
            .get("server-b_alpha")
            .await
            .expect("alpha must be registered with server prefix");
        assert_eq!(alpha.server_name, "server-b");
        assert_eq!(alpha.name, "alpha");
        assert_eq!(alpha.description.as_deref(), Some("alpha tool"));
        assert_eq!(alpha.input_schema, serde_json::json!({ "type": "object" }));

        let beta = registry
            .get("server-b_beta")
            .await
            .expect("beta must be registered with server prefix");
        assert_eq!(beta.description, None);
        assert_eq!(beta.input_schema, serde_json::json!({ "type": "array" }));
        assert_eq!(registry.list().await.len(), 2);
    }

    #[tokio::test]
    async fn list_and_list_for_server_filter_by_server_name() {
        let registry = McpToolRegistry::new();
        registry
            .register(McpTool::new("server-a", "one", None, serde_json::json!({})))
            .await;
        registry
            .register(McpTool::new("server-a", "two", None, serde_json::json!({})))
            .await;
        registry
            .register(McpTool::new(
                "server-b",
                "three",
                None,
                serde_json::json!({}),
            ))
            .await;

        let mut all: Vec<String> = registry
            .list()
            .await
            .into_iter()
            .map(|tool| tool.full_name)
            .collect();
        all.sort_unstable();
        assert_eq!(
            all,
            vec![
                "server-a_one".to_string(),
                "server-a_two".to_string(),
                "server-b_three".to_string(),
            ]
        );

        let mut for_a: Vec<String> = registry
            .list_for_server("server-a")
            .await
            .into_iter()
            .map(|tool| tool.full_name)
            .collect();
        for_a.sort_unstable();
        assert_eq!(
            for_a,
            vec!["server-a_one".to_string(), "server-a_two".to_string()]
        );
        assert!(registry.list_for_server("server-missing").await.is_empty());
    }

    #[tokio::test]
    async fn clear_server_removes_only_the_matching_server() {
        let registry = McpToolRegistry::new();
        registry
            .register(McpTool::new("server-a", "one", None, serde_json::json!({})))
            .await;
        registry
            .register(McpTool::new("server-b", "two", None, serde_json::json!({})))
            .await;

        registry.clear_server("server-a").await;

        assert!(registry.get("server-a_one").await.is_none());
        assert!(registry.get("server-b_two").await.is_some());
        assert_eq!(registry.list().await.len(), 1);

        registry.clear_server("server-a").await;
        assert_eq!(registry.list().await.len(), 1);
    }

    #[tokio::test]
    async fn clear_removes_all_tools() {
        let registry = McpToolRegistry::new();
        registry
            .register(McpTool::new("server-a", "one", None, serde_json::json!({})))
            .await;
        registry
            .register(McpTool::new("server-b", "two", None, serde_json::json!({})))
            .await;

        registry.clear().await;

        assert!(registry.list().await.is_empty());
        assert!(registry.get("server-a_one").await.is_none());
        assert!(registry.get("server-b_two").await.is_none());

        registry.clear().await;
        assert!(registry.list().await.is_empty());
    }

    #[tokio::test]
    async fn default_registry_starts_empty_and_accepts_registration() {
        let registry = McpToolRegistry::default();
        assert!(registry.list().await.is_empty());

        registry
            .register(McpTool::new(
                "server-x",
                "tool",
                None,
                serde_json::json!({}),
            ))
            .await;
        assert_eq!(registry.list().await.len(), 1);
        assert!(registry.get("server-x_tool").await.is_some());
    }
}
