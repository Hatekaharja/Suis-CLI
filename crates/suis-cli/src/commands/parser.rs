//! Parsing chat input into slash commands.
//!
//! Any input whose first non-whitespace character is `/` is a command. The
//! word after the slash is the command name; the remainder (trimmed) is its
//! argument string. Everything else is an ordinary chat message.

/// A recognized slash command, parsed from user input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/model` — open the model selection screen.
    Model,
    /// `/clear` — clear the conversation history.
    Clear,
    /// `/tasks` — toggle the task panel.
    Tasks,
    /// `/permissions` — list stored project permissions.
    Permissions,
    /// `/providers` — enable or disable discovered providers.
    Providers,
    /// `/plan` — switch to Plan mode (read-only analysis).
    Plan,
    /// `/agent` — switch to Agent mode (full execution).
    Agent,
    /// `/chat` — switch to Chat mode (read-only discussion).
    Chat,
    /// `/plans` — list stored plans with their progress.
    Plans,
    /// `/implement` — pick a plan (or one step) and start a focused
    /// implementation session.
    Implement,
    /// `/compact` — summarize the conversation and replace history with it.
    Compact,
    /// `/profile [refresh]` — view the cached project profile, or re-detect and
    /// persist it when `refresh` is `true` (`/profile refresh`).
    Profile { refresh: bool },
    /// `/usage` — open the per-provider token-usage popup.
    Usage,
    /// `/developer` — toggle the raw tool-call display under each tool card.
    Developer,
    /// `/help` — list available commands.
    Help,
    /// `/exit` (also `/quit`) — quit the program.
    Exit,
    /// A `/name` that matches no known command.
    Unknown(String),
}

/// The canonical list of command names, in display order, paired with a
/// one-line description (used by `/help` and tab completion).
pub const COMMANDS: &[(&str, &str)] = &[
    ("model", "Open the model selection screen"),
    ("clear", "Clear conversation history"),
    ("tasks", "Toggle the task panel"),
    ("permissions", "Show current project permissions"),
    ("providers", "Enable or disable providers"),
    ("plan", "Switch to Plan mode (read-only analysis)"),
    ("agent", "Switch to Agent mode (full execution)"),
    ("chat", "Switch to Chat mode (read-only discussion)"),
    ("plans", "List stored plans and their progress"),
    (
        "implement",
        "Start a focused implementation session for a plan",
    ),
    ("compact", "Summarize the conversation to free up context"),
    (
        "profile",
        "Show the project profile (/profile refresh to re-detect)",
    ),
    ("usage", "Show token usage this session"),
    ("developer", "Toggle raw tool-call display (debug)"),
    ("help", "Show available commands"),
    ("exit", "Exit Suis"),
];

/// Whether `input` is a slash command (rather than a chat message).
pub fn is_command(input: &str) -> bool {
    input.trim_start().starts_with('/')
}

/// Parse `input` into a [`Command`]. Returns `None` if `input` is not a command
/// (i.e. does not start with `/`).
pub fn parse(input: &str) -> Option<Command> {
    let trimmed = input.trim_start();
    let rest = trimmed.strip_prefix('/')?;
    let mut words = rest.split_whitespace();
    let name = words.next().unwrap_or("");
    let arg = words.next();
    let command = match name {
        "model" => Command::Model,
        "clear" => Command::Clear,
        "tasks" => Command::Tasks,
        "permissions" => Command::Permissions,
        "providers" => Command::Providers,
        "plan" => Command::Plan,
        "agent" => Command::Agent,
        "chat" => Command::Chat,
        "plans" => Command::Plans,
        "implement" => Command::Implement,
        "compact" => Command::Compact,
        "profile" => Command::Profile {
            refresh: arg.is_some_and(|a| a.eq_ignore_ascii_case("refresh")),
        },
        "usage" => Command::Usage,
        "developer" => Command::Developer,
        "help" => Command::Help,
        // `/quit` is an alias so muscle memory from other tools still works.
        "exit" | "quit" => Command::Exit,
        other => Command::Unknown(other.to_string()),
    };
    Some(command)
}

/// Complete a partial command name. Given the current input (which must start
/// with `/` and contain no spaces yet), returns the completed input if exactly
/// one command matches the typed prefix.
pub fn complete(input: &str) -> Option<String> {
    let trimmed = input.trim_start();
    let prefix = trimmed.strip_prefix('/')?;
    // Only complete a bare command name, not its arguments.
    if prefix.contains(char::is_whitespace) {
        return None;
    }
    let matches: Vec<&str> = COMMANDS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| name.starts_with(prefix))
        .collect();
    match matches.as_slice() {
        [only] => Some(format!("/{only}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_commands() {
        assert!(is_command("/help"));
        assert!(is_command("  /model"));
        assert!(!is_command("hello"));
        assert!(!is_command("what is /tmp"));
    }

    #[test]
    fn parses_known_commands() {
        assert_eq!(parse("/help"), Some(Command::Help));
        assert_eq!(parse("/clear"), Some(Command::Clear));
        assert_eq!(parse("/model"), Some(Command::Model));
        assert_eq!(parse("/tasks"), Some(Command::Tasks));
        assert_eq!(parse("/permissions"), Some(Command::Permissions));
        assert_eq!(parse("/providers"), Some(Command::Providers));
        assert_eq!(parse("/plan"), Some(Command::Plan));
        assert_eq!(parse("/agent"), Some(Command::Agent));
        assert_eq!(parse("/chat"), Some(Command::Chat));
        assert_eq!(parse("/plans"), Some(Command::Plans));
        assert_eq!(parse("/implement"), Some(Command::Implement));
        assert_eq!(parse("/compact"), Some(Command::Compact));
        assert_eq!(parse("/developer"), Some(Command::Developer));
        assert_eq!(parse("/exit"), Some(Command::Exit));
    }

    #[test]
    fn quit_is_an_alias_for_exit() {
        assert_eq!(parse("/quit"), Some(Command::Exit));
    }

    #[test]
    fn profile_view_versus_refresh() {
        assert_eq!(parse("/profile"), Some(Command::Profile { refresh: false }));
        assert_eq!(
            parse("/profile refresh"),
            Some(Command::Profile { refresh: true })
        );
        assert_eq!(
            parse("/profile REFRESH"),
            Some(Command::Profile { refresh: true })
        );
        // An unrecognized argument just shows the profile.
        assert_eq!(
            parse("/profile foo"),
            Some(Command::Profile { refresh: false })
        );
    }

    #[test]
    fn ignores_trailing_arguments() {
        assert_eq!(parse("/model qwen3"), Some(Command::Model));
    }

    #[test]
    fn unknown_command_carries_name() {
        assert_eq!(parse("/foo"), Some(Command::Unknown("foo".into())));
    }

    #[test]
    fn non_command_is_none() {
        assert_eq!(parse("just chatting"), None);
    }

    #[test]
    fn completes_unique_prefix() {
        assert_eq!(complete("/he").as_deref(), Some("/help"));
        assert_eq!(complete("/mod").as_deref(), Some("/model"));
        assert_eq!(complete("/permi").as_deref(), Some("/permissions"));
        assert_eq!(complete("/imp").as_deref(), Some("/implement"));
        // `/plan` is a prefix of `/plans`, so neither completes from "/plan".
        assert_eq!(complete("/plan"), None);
    }

    #[test]
    fn ambiguous_or_empty_prefix_does_not_complete() {
        // Several commands begin with the empty prefix, so `/` is ambiguous.
        assert_eq!(complete("/"), None);
        // `t` is unique here (only "tasks"), so it should complete...
        assert_eq!(complete("/t").as_deref(), Some("/tasks"));
        // ...but a prefix matching nothing yields nothing.
        assert_eq!(complete("/zzz"), None);
    }

    #[test]
    fn does_not_complete_with_arguments() {
        assert_eq!(complete("/model foo"), None);
    }
}
