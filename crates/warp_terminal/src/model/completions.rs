use warp_completer::completer::{
    EngineFileType, Match, MatchedSuggestion, Suggestion, SuggestionType,
};

/// A completion result that was produced natively by the shell.
#[derive(Clone, Debug)]
pub struct ShellCompletion {
    name: String,
    description: Option<String>,
    suggestion_type: SuggestionType,
    is_directory: bool,
}

/// Enum indicating which field of a [`ShellCompletion`] should be updated.
pub enum ShellCompletionUpdate {
    Description { value: String },
    IsDirectory { value: bool },
}

impl ShellCompletion {
    pub fn new(name: String) -> Self {
        Self {
            name: name.trim().to_string(),
            description: None,
            suggestion_type: SuggestionType::Argument,
            is_directory: false,
        }
    }

    pub fn is_directory(&self) -> bool {
        self.is_directory
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn suggestion_type(&self) -> SuggestionType {
        self.suggestion_type
    }

    pub fn update(&mut self, completion_update: ShellCompletionUpdate) {
        match completion_update {
            ShellCompletionUpdate::Description { value } => {
                if !value.is_empty() {
                    self.description = Some(value.trim().to_string());
                }
            }
            ShellCompletionUpdate::IsDirectory { value } => {
                self.is_directory = value;
            }
        }
    }

    /// Rewrites the completion so it reads back the way the user typed the token.
    ///
    /// Shells report completions in their own canonical form: PowerShell returns
    /// `.\scoop\buckets\custom` for a token typed `scoop/buckets/`. The filtering step downstream
    /// fuzzy-matches the query against this text, so a `/` in the query can never match a
    /// `\`-only candidate and every result gets dropped. Normalize the separators to the ones the
    /// user typed, and strip the `.\`/`./` prefix PowerShell prepends for relative paths so the
    /// candidate stays relative to the token being completed.
    ///
    /// Directories get the typed separator appended (shells omit it), so accepting the
    /// completion keeps the token open for the next path level instead of terminating it.
    pub fn normalized_for_query(&self, query: &str, path_separators: &[char]) -> Self {
        let typed_separator = query
            .chars()
            .rev()
            .find(|character| path_separators.contains(character))
            .or_else(|| {
                query
                    .chars()
                    .find(|character| path_separators.contains(character))
            });

        let Some(typed_separator) = typed_separator else {
            return self.clone();
        };

        // `.\foo` / `./foo` are how PowerShell spells a path relative to the working directory.
        // Strip the prefix only when the user's own token does not already carry one, otherwise
        // the replacement would drop the `./` they typed.
        let name = if query.starts_with("./") || query.starts_with(".\\") {
            self.name.as_str()
        } else {
            self.name
                .strip_prefix(".\\")
                .or_else(|| self.name.strip_prefix("./"))
                .unwrap_or(&self.name)
        };

        let mut normalized: String = name
            .chars()
            .map(|character| {
                if path_separators.contains(&character) {
                    typed_separator
                } else {
                    character
                }
            })
            .collect();

        if self.is_directory
            && !normalized
                .chars()
                .next_back()
                .is_some_and(|character| path_separators.contains(&character))
        {
            normalized.push(typed_separator);
        }

        Self {
            name: normalized,
            description: self.description.clone(),
            suggestion_type: self.suggestion_type,
            is_directory: self.is_directory,
        }
    }
}

impl From<ShellCompletion> for MatchedSuggestion {
    fn from(value: ShellCompletion) -> Self {
        let mut suggestion = Suggestion::with_same_display_and_replacement(
            value.name,
            value.description,
            value.suggestion_type,
            Default::default(),
        );
        if value.is_directory {
            suggestion.file_type = Some(EngineFileType::Directory);
        }
        MatchedSuggestion::new(
            suggestion,
            Match::Prefix {
                is_case_sensitive: false,
            },
        )
    }
}

#[cfg(test)]
#[path = "completions_tests.rs"]
mod tests;
