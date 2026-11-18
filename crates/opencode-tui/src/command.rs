use std::collections::HashMap;
use std::hash::Hash;

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub fn fuzzy_match(query: &str, target: &str) -> Option<i32> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Some(0);
    }

    let pattern = Pattern::parse(trimmed, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut utf32_buf = Vec::new();
    pattern
        .score(Utf32Str::new(target, &mut utf32_buf), &mut matcher)
        .map(|score| score.min(i32::MAX as u32) as i32)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Session,
    ModelAgent,
    Display,
    Navigation,
    System,
    Prompt,
}

#[derive(Clone, Debug)]
pub struct SlashCommand {
    pub name: String,
    pub aliases: Vec<String>,
    pub title: String,
    pub description: String,
    pub category: CommandCategory,
    pub keybind: Option<String>,
    pub suggested: bool,
    pub action: CommandAction,
}

#[derive(Clone, Debug)]
pub enum CommandAction {
    // Session
    NewSession,
    ListSessions,
    ShareSession,
    UnshareSession,
    RenameSession,
    ForkSession,
    CompactSession,
    Timeline,
    Undo,
    Redo,
    CopySession,
    ExportSession,
    // Model/Agent
    SwitchModel,
    SwitchAgent,
    ManageMcp,
    ConnectProvider,
    CycleVariant,
    // Display
    SwitchTheme,
    ToggleAppearance,
    ShowStatus,
    ViewStatus,
    ShowHelp,
    ExternalEditor,
    ToggleTimestamps,
    ToggleThinking,
    ToggleToolDetails,
    ToggleDensity,
    ToggleSemanticHighlight,
    ToggleHeader,
    ToggleScrollbar,
    ToggleSidebar,
    ToggleMcp,
    ToggleTips,
    ToggleCommandPalette,
    // Navigation
    OpenSessionList,
    SwitchSession,
    OpenModelList,
    OpenAgentList,
    OpenMcpList,
    OpenThemeList,
    OpenStash,
    OpenSkills,
    // Prompt
    SubmitPrompt,
    ClearPrompt,
    PasteClipboard,
    CopyPrompt,
    CutPrompt,
    HistoryPrevious,
    HistoryNext,
    PromptStashPush,
    PromptStashList,
    PromptSkillList,
    // System
    Exit,
}

pub struct CommandRegistry {
    commands: HashMap<String, SlashCommand>,
    by_category: HashMap<CommandCategory, Vec<String>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            commands: HashMap::new(),
            by_category: HashMap::new(),
        };
        registry.register_all();
        registry
    }

    fn register(&mut self, cmd: SlashCommand) {
        let name = cmd.name.clone();
        self.commands.insert(name.clone(), cmd.clone());

        self.by_category
            .entry(cmd.category.clone())
            .or_default()
            .push(name);

        for alias in &cmd.aliases {
            self.commands.insert(alias.clone(), cmd.clone());
        }
    }

    fn register_all(&mut self) {
        self.register(SlashCommand {
            name: "/new".to_string(),
            aliases: vec!["/clear".to_string()],
            title: "New Session".to_string(),
            description: "Start a new conversation".to_string(),
            category: CommandCategory::Session,
            keybind: Some("ctrl_n".to_string()),
            suggested: true,
            action: CommandAction::NewSession,
        });

        self.register(SlashCommand {
            name: "/sessions".to_string(),
            aliases: vec!["/resume".to_string(), "/continue".to_string()],
            title: "Switch Session".to_string(),
            description: "Switch to another session".to_string(),
            category: CommandCategory::Session,
            keybind: Some("ctrl_s".to_string()),
            suggested: true,
            action: CommandAction::OpenSessionList,
        });

        self.register(SlashCommand {
            name: "/share".to_string(),
            aliases: vec![],
            title: "Share Session".to_string(),
            description: "Generate a shareable link".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::ShareSession,
        });

        self.register(SlashCommand {
            name: "/unshare".to_string(),
            aliases: vec![],
            title: "Unshare Session".to_string(),
            description: "Revoke sharing link".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::UnshareSession,
        });

        self.register(SlashCommand {
            name: "/rename".to_string(),
            aliases: vec![],
            title: "Rename Session".to_string(),
            description: "Rename current session".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::RenameSession,
        });

        self.register(SlashCommand {
            name: "/fork".to_string(),
            aliases: vec![],
            title: "Fork Session".to_string(),
            description: "Create a fork from a message".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::ForkSession,
        });

        self.register(SlashCommand {
            name: "/compact".to_string(),
            aliases: vec!["/summarize".to_string()],
            title: "Compact Session".to_string(),
            description: "Summarize and compress session history".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::CompactSession,
        });

        self.register(SlashCommand {
            name: "/timeline".to_string(),
            aliases: vec![],
            title: "Timeline".to_string(),
            description: "Navigate to a message in timeline".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::Timeline,
        });

        self.register(SlashCommand {
            name: "/undo".to_string(),
            aliases: vec![],
            title: "Undo".to_string(),
            description: "Revert last message".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::Undo,
        });

        self.register(SlashCommand {
            name: "/redo".to_string(),
            aliases: vec![],
            title: "Redo".to_string(),
            description: "Restore reverted message".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::Redo,
        });

        self.register(SlashCommand {
            name: "/copy".to_string(),
            aliases: vec![],
            title: "Copy Session".to_string(),
            description: "Copy session to clipboard".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::CopySession,
        });

        self.register(SlashCommand {
            name: "/export".to_string(),
            aliases: vec![],
            title: "Export Session".to_string(),
            description: "Export session as markdown".to_string(),
            category: CommandCategory::Session,
            keybind: None,
            suggested: false,
            action: CommandAction::ExportSession,
        });

        self.register(SlashCommand {
            name: "/models".to_string(),
            aliases: vec![],
            title: "Switch Model".to_string(),
            description: "Choose a different model".to_string(),
            category: CommandCategory::ModelAgent,
            keybind: Some("ctrl_m".to_string()),
            suggested: true,
            action: CommandAction::OpenModelList,
        });

        self.register(SlashCommand {
            name: "/agents".to_string(),
            aliases: vec![],
            title: "Switch Agent".to_string(),
            description: "Choose a different agent".to_string(),
            category: CommandCategory::ModelAgent,
            keybind: None,
            suggested: false,
            action: CommandAction::OpenAgentList,
        });

        self.register(SlashCommand {
            name: "/mcps".to_string(),
            aliases: vec![],
            title: "Manage MCP".to_string(),
            description: "Manage MCP servers".to_string(),
            category: CommandCategory::ModelAgent,
            keybind: None,
            suggested: false,
            action: CommandAction::OpenMcpList,
        });

        self.register(SlashCommand {
            name: "/connect".to_string(),
            aliases: vec![],
            title: "Connect Provider".to_string(),
            description: "Connect to a new LLM provider".to_string(),
            category: CommandCategory::ModelAgent,
            keybind: None,
            suggested: false,
            action: CommandAction::ConnectProvider,
        });

        self.register(SlashCommand {
            name: "/themes".to_string(),
            aliases: vec![],
            title: "Switch Theme".to_string(),
            description: "Choose a color theme".to_string(),
            category: CommandCategory::Display,
            keybind: Some("ctrl_t".to_string()),
            suggested: true,
            action: CommandAction::OpenThemeList,
        });

        self.register(SlashCommand {
            name: "/status".to_string(),
            aliases: vec![],
            title: "Status".to_string(),
            description: "Show system status".to_string(),
            category: CommandCategory::System,
            keybind: None,
            suggested: false,
            action: CommandAction::ShowStatus,
        });

        self.register(SlashCommand {
            name: "/help".to_string(),
            aliases: vec!["/commands".to_string()],
            title: "Help".to_string(),
            description: "Show help and shortcuts".to_string(),
            category: CommandCategory::System,
            keybind: Some("f1".to_string()),
            suggested: true,
            action: CommandAction::ShowHelp,
        });

        self.register(SlashCommand {
            name: "/editor".to_string(),
            aliases: vec![],
            title: "External Editor".to_string(),
            description: "Open in external editor".to_string(),
            category: CommandCategory::System,
            keybind: None,
            suggested: false,
            action: CommandAction::ExternalEditor,
        });

        self.register(SlashCommand {
            name: "/exit".to_string(),
            aliases: vec!["/quit".to_string(), "/q".to_string()],
            title: "Exit".to_string(),
            description: "Exit the application".to_string(),
            category: CommandCategory::System,
            keybind: Some("ctrl_c".to_string()),
            suggested: true,
            action: CommandAction::Exit,
        });

        self.register(SlashCommand {
            name: "/timestamps".to_string(),
            aliases: vec!["/toggle-timestamps".to_string()],
            title: "Toggle Timestamps".to_string(),
            description: "Show/hide message timestamps".to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleTimestamps,
        });

        self.register(SlashCommand {
            name: "/tips.toggle".to_string(),
            aliases: vec!["/tips".to_string()],
            title: "Toggle Tips".to_string(),
            description: "Show/hide home page tips".to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleTips,
        });

        self.register(SlashCommand {
            name: "/thinking".to_string(),
            aliases: vec!["/toggle-thinking".to_string()],
            title: "Toggle Thinking".to_string(),
            description: "Show/hide thinking blocks".to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleThinking,
        });

        self.register(SlashCommand {
            name: "/density".to_string(),
            aliases: vec!["/toggle-density".to_string()],
            title: "Toggle Density".to_string(),
            description: "Switch between compact and cozy message layout".to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleDensity,
        });

        self.register(SlashCommand {
            name: "/highlight".to_string(),
            aliases: vec!["/toggle-highlight".to_string(), "/semantic".to_string()],
            title: "Toggle Semantic Highlight".to_string(),
            description: "Enable/disable semantic highlighting of paths, errors, commands"
                .to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleSemanticHighlight,
        });

        self.register(SlashCommand {
            name: "/sidebar".to_string(),
            aliases: vec![],
            title: "Toggle Sidebar".to_string(),
            description: "Show/hide sidebar".to_string(),
            category: CommandCategory::Display,
            keybind: Some("ctrl_b".to_string()),
            suggested: false,
            action: CommandAction::ToggleSidebar,
        });

        self.register(SlashCommand {
            name: "/header".to_string(),
            aliases: vec![],
            title: "Toggle Header".to_string(),
            description: "Show/hide session header".to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleHeader,
        });

        self.register(SlashCommand {
            name: "/scrollbar".to_string(),
            aliases: vec![],
            title: "Toggle Scrollbar".to_string(),
            description: "Show/hide session scrollbar".to_string(),
            category: CommandCategory::Display,
            keybind: None,
            suggested: false,
            action: CommandAction::ToggleScrollbar,
        });

        self.register(SlashCommand {
            name: "/command".to_string(),
            aliases: vec!["/cmd".to_string(), "/palette".to_string()],
            title: "Command Palette".to_string(),
            description: "Open command palette".to_string(),
            category: CommandCategory::Navigation,
            keybind: Some("ctrl_p".to_string()),
            suggested: true,
            action: CommandAction::ToggleCommandPalette,
        });

        self.register(SlashCommand {
            name: "/stash".to_string(),
            aliases: vec![],
            title: "Prompt Stash".to_string(),
            description: "Save current prompt to stash".to_string(),
            category: CommandCategory::Prompt,
            keybind: None,
            suggested: false,
            action: CommandAction::OpenStash,
        });

        self.register(SlashCommand {
            name: "/skills".to_string(),
            aliases: vec![],
            title: "Skills".to_string(),
            description: "Browse available skills".to_string(),
            category: CommandCategory::Navigation,
            keybind: None,
            suggested: false,
            action: CommandAction::OpenSkills,
        });
    }

    pub fn get(&self, name: &str) -> Option<&SlashCommand> {
        self.commands.get(name)
    }

    pub fn search(&self, query: &str) -> Vec<&SlashCommand> {
        let mut scored: Vec<(&SlashCommand, i32)> = self
            .commands
            .values()
            .filter_map(|cmd| {
                let name_score = fuzzy_match(query, &cmd.name);
                let alias_score = cmd
                    .aliases
                    .iter()
                    .filter_map(|a| fuzzy_match(query, a))
                    .max();
                let title_score = fuzzy_match(query, &cmd.title);
                let best = name_score
                    .into_iter()
                    .chain(alias_score)
                    .chain(title_score)
                    .max();
                best.map(|s| (cmd, s))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| b.0.suggested.cmp(&a.0.suggested))
        });

        scored.into_iter().map(|(cmd, _)| cmd).collect()
    }

    pub fn get_by_category(&self, category: &CommandCategory) -> Vec<&SlashCommand> {
        self.by_category
            .get(category)
            .map(|names| {
                names
                    .iter()
                    .filter_map(|name| self.commands.get(name))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn all_commands(&self) -> Vec<&SlashCommand> {
        let mut seen = std::collections::HashSet::new();
        self.commands
            .values()
            .filter(|cmd| seen.insert(cmd.name.clone()))
            .collect()
    }

    pub fn suggested_commands(&self) -> Vec<&SlashCommand> {
        self.all_commands()
            .into_iter()
            .filter(|cmd| cmd.suggested)
            .collect()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_command(name: &str, aliases: &[&str], category: CommandCategory) -> SlashCommand {
        SlashCommand {
            name: name.to_string(),
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            title: "Dummy Title".to_string(),
            description: "Dummy description".to_string(),
            category,
            keybind: None,
            suggested: false,
            action: CommandAction::ShowHelp,
        }
    }

    fn dummy_registry() -> CommandRegistry {
        let mut registry = CommandRegistry {
            commands: HashMap::new(),
            by_category: HashMap::new(),
        };
        registry.register(dummy_command(
            "/dummy",
            &["/dummy-alias"],
            CommandCategory::Display,
        ));
        registry.register(dummy_command("/other", &[], CommandCategory::Prompt));
        registry
    }

    #[test]
    fn fuzzy_match_treats_blank_query_as_neutral() {
        assert_eq!(fuzzy_match("", "/new"), Some(0));
        assert_eq!(fuzzy_match("   \t", "/new"), Some(0));
    }

    #[test]
    fn fuzzy_match_distinguishes_hit_and_miss() {
        assert!(fuzzy_match("new", "/new").is_some());
        assert!(fuzzy_match("zzz_qqq_xxx", "/new").is_none());
    }

    #[test]
    fn register_indexes_primary_name_and_aliases_without_duplicating_category() {
        let registry = dummy_registry();

        let primary = registry.get("/dummy").expect("primary name");
        assert_eq!(primary.title, "Dummy Title");

        let alias = registry.get("/dummy-alias").expect("alias");
        assert_eq!(alias.name, "/dummy");
        assert_eq!(alias.title, primary.title);

        let names = registry
            .by_category
            .get(&CommandCategory::Display)
            .expect("category index");
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].as_str(), "/dummy");
    }

    #[test]
    fn get_returns_none_for_unregistered_name() {
        let registry = dummy_registry();
        assert!(registry.get("/not-registered").is_none());
    }

    #[test]
    fn all_commands_deduplicates_aliases() {
        let registry = dummy_registry();
        let all = registry.all_commands();

        let mut names: Vec<&str> = all.iter().map(|cmd| cmd.name.as_str()).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();

        assert_eq!(total, 2);
        assert_eq!(
            names.len(),
            total,
            "aliases must not create duplicate entries"
        );
        assert!(names.contains(&"/dummy"));
        assert!(names.contains(&"/other"));
    }

    #[test]
    fn suggested_commands_only_returns_flagged_primary_commands() {
        let registry = dummy_registry();
        assert!(registry.suggested_commands().is_empty());

        let mut registry = dummy_registry();
        for cmd in registry.commands.values_mut() {
            if cmd.name == "/dummy" {
                cmd.suggested = true;
            }
        }

        let suggested = registry.suggested_commands();
        assert_eq!(suggested.len(), 1);
        assert_eq!(suggested[0].name, "/dummy");
        assert!(suggested.iter().all(|cmd| cmd.suggested));
    }

    #[test]
    fn get_by_category_returns_only_primary_commands_of_that_category() {
        let registry = dummy_registry();

        let display = registry.get_by_category(&CommandCategory::Display);
        assert_eq!(display.len(), 1);
        assert_eq!(display[0].name, "/dummy");
        assert!(display.iter().all(|cmd| cmd.name != "/dummy-alias"));

        let prompt = registry.get_by_category(&CommandCategory::Prompt);
        assert_eq!(prompt.len(), 1);
        assert_eq!(prompt[0].name, "/other");
    }

    #[test]
    fn get_by_category_on_empty_registry_is_empty() {
        let registry = CommandRegistry {
            commands: HashMap::new(),
            by_category: HashMap::new(),
        };
        assert!(registry
            .get_by_category(&CommandCategory::System)
            .is_empty());
    }

    #[test]
    fn search_finds_command_by_name_and_alias() {
        let registry = dummy_registry();

        assert!(registry
            .search("/dummy")
            .iter()
            .any(|cmd| cmd.name == "/dummy"));
        assert!(registry
            .search("/dummy-alias")
            .iter()
            .any(|cmd| cmd.name == "/dummy"));
    }

    #[test]
    fn builtin_registry_resolves_known_name_and_alias() {
        let registry = CommandRegistry::new();

        let primary = registry.get("/new").expect("built-in /new");
        assert!(matches!(primary.action, CommandAction::NewSession));

        let alias = registry.get("/clear").expect("built-in /clear alias");
        assert_eq!(alias.name, "/new");

        assert!(registry.get("/definitely-not-a-command").is_none());
        assert!(registry
            .suggested_commands()
            .iter()
            .any(|cmd| cmd.name == "/new"));
    }
}
