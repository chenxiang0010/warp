use super::{
    ExplicitTabCompletion, MatchedSuggestion, Priority, Suggestion, SuggestionResults,
    SuggestionType,
};
use crate::completer::{EngineFileType, MatchStrategy, MatchType, TopLevelCommandCaseSensitivity};
use crate::meta::Span;

const PATH_SEPARATORS: &[char] = &['/'];

fn matched_suggestion(display: &str, query: &str) -> MatchedSuggestion {
    let suggestion = Suggestion::with_same_display_and_replacement(
        display,
        None,
        SuggestionType::Command(TopLevelCommandCaseSensitivity::CaseSensitive),
        Priority::default(),
    );
    let match_type = MatchStrategy::Fuzzy
        .get_match_type(query, display)
        .expect("test suggestion should match its query");
    MatchedSuggestion::new(suggestion, match_type)
}

fn results(query: &str, displays: &[&str]) -> SuggestionResults {
    SuggestionResults {
        replacement_span: Span::new(0, query.len()),
        suggestions: displays
            .iter()
            .map(|display| matched_suggestion(display, query))
            .collect(),
        match_strategy: MatchStrategy::Fuzzy,
    }
}

// Mirrors native shell completions: the pre-stored match type is computed against the
// tilde-stripped query because the candidates are absolute paths.
fn absolute_path_results(stripped_query: &str, displays: &[&str]) -> SuggestionResults {
    SuggestionResults {
        replacement_span: Span::new(0, 0),
        suggestions: displays
            .iter()
            .map(|display| matched_suggestion(display, stripped_query))
            .collect(),
        match_strategy: MatchStrategy::Fuzzy,
    }
}

fn file_results(stripped_query: &str, files: &[(&str, &str)]) -> SuggestionResults {
    SuggestionResults {
        replacement_span: Span::new(0, 0),
        suggestions: files
            .iter()
            .map(|&(display, replacement)| {
                let mut suggestion = Suggestion::with_same_display_and_replacement(
                    display,
                    None,
                    SuggestionType::Argument,
                    Priority::default(),
                );
                suggestion.replacement = replacement.into();
                suggestion.file_type = Some(EngineFileType::Directory);
                let match_type = MatchStrategy::Fuzzy
                    .get_match_type(stripped_query, display)
                    .expect("test file suggestion should match its stripped query");
                MatchedSuggestion::new(suggestion, match_type)
            })
            .collect(),
        match_strategy: MatchStrategy::Fuzzy,
    }
}

#[test]
fn prepared_suggestions_follow_presentation_order() {
    let results = results("git", &["graft-it", "git-status", "GIT", "git"]);

    let prepared = results.prepare_for_query("git", PATH_SEPARATORS);
    let displays = prepared
        .iter()
        .map(|suggestion| suggestion.suggestion.display.as_str())
        .collect::<Vec<_>>();

    assert_eq!(displays, ["git", "GIT", "git-status", "graft-it"]);
    assert!(matches!(
        prepared[0].match_type,
        MatchType::Exact {
            is_case_sensitive: true
        }
    ));
    assert_eq!(prepared[0].matching_indices, [0, 1, 2]);
    assert!(matches!(prepared[3].match_type, MatchType::Fuzzy));
}

#[test]
fn explicit_tab_inserts_the_single_prefix_suggestion() {
    let results = results("st", &["sitar", "status"]);

    let ExplicitTabCompletion::InsertSingle {
        suggestion,
        replacement_span,
    } = results.explicit_tab_completion("st", PATH_SEPARATORS)
    else {
        panic!("one prefix suggestion should be inserted");
    };

    assert_eq!(suggestion.suggestion.replacement, "status");
    assert_eq!(replacement_span, Span::new(0, 2));
}

#[test]
fn explicit_tab_opens_an_ordered_menu_with_a_common_prefix() {
    let results = results("s", &["stork", "stash", "status"]);

    let ExplicitTabCompletion::InsertCommonPrefixAndOpen {
        common_prefix,
        suggestions,
        replacement_span,
    } = results.explicit_tab_completion("s", PATH_SEPARATORS)
    else {
        panic!("multiple prefix suggestions should open the menu");
    };
    assert_eq!(common_prefix, "st");
    assert_eq!(replacement_span, Span::new(0, 1));
    assert_eq!(
        suggestions
            .iter()
            .map(|suggestion| suggestion.suggestion.display.as_str())
            .collect::<Vec<_>>(),
        ["stork", "stash", "status"]
    );
}

#[test]
fn explicit_tab_has_no_action_when_the_query_filters_every_candidate() {
    let results = results("a", &["alpha"]);

    assert!(matches!(
        results.explicit_tab_completion("z", PATH_SEPARATORS),
        ExplicitTabCompletion::NoAction
    ));
}

#[test]
fn explicit_tab_does_not_insert_a_case_insensitive_common_prefix() {
    let results = results("ab", &["Abacus", "Abandon"]);

    let ExplicitTabCompletion::Open { suggestions, .. } =
        results.explicit_tab_completion("ab", PATH_SEPARATORS)
    else {
        panic!("case-insensitive prefixes should only open the menu");
    };
    assert_eq!(suggestions.len(), 2);
}

#[test]
fn explicit_tab_computes_common_prefixes_on_utf8_boundaries() {
    let results = results("", &["éclair", "école"]);

    let ExplicitTabCompletion::InsertCommonPrefixAndOpen { common_prefix, .. } =
        results.explicit_tab_completion("", PATH_SEPARATORS)
    else {
        panic!("the shared Unicode prefix should be inserted");
    };
    assert_eq!(common_prefix, "éc");
}

#[test]
fn explicit_tab_opens_the_menu_for_a_tilde_query_on_absolute_paths() {
    let results = absolute_path_results("D", &[r"C:\Users\user\Docs", r"C:\Users\user\Desktop"]);

    let ExplicitTabCompletion::Open { suggestions, .. } =
        results.explicit_tab_completion("~/D", PATH_SEPARATORS)
    else {
        panic!("a tilde-prefixed query should open the menu for absolute path candidates");
    };
    let mut displays = suggestions
        .iter()
        .map(|suggestion| suggestion.suggestion.display.as_str())
        .collect::<Vec<_>>();
    displays.sort_unstable();
    assert_eq!(displays, [r"C:\Users\user\Desktop", r"C:\Users\user\Docs"]);
}

#[test]
fn tilde_query_matching_indices_track_the_stripped_query() {
    let results = absolute_path_results("D", &[r"C:\Users\user\Desktop"]);

    let prepared = results.prepare_for_query("~/D", PATH_SEPARATORS);

    assert_eq!(prepared.len(), 1);
    // The stripped query "D" matches the first letter of the basename.
    assert_eq!(prepared[0].matching_indices, [14]);
}

#[test]
fn tilde_query_inserts_the_expanded_file_candidate() {
    let results = file_results("Desktop", &[("Desktop", r"C:\Users\user\Desktop")]);

    let ExplicitTabCompletion::InsertSingle { suggestion, .. } =
        results.explicit_tab_completion("~/Desktop", PATH_SEPARATORS)
    else {
        panic!("the expanded file candidate should be inserted for the tilde query");
    };
    assert_eq!(suggestion.suggestion.replacement, r"C:\Users\user\Desktop");
}
