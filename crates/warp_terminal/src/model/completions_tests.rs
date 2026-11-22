use super::*;

fn windows_separators() -> PathSeparators {
    PathSeparators::for_windows()
}

fn unix_separators() -> PathSeparators {
    PathSeparators::for_unix()
}

fn directory(name: &str) -> ShellCompletion {
    let mut completion = ShellCompletion::new(name.to_owned());
    completion.update(ShellCompletionUpdate::IsDirectory { value: true });
    completion
}

#[test]
fn rewrites_backslashes_to_the_separator_the_user_typed() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query("scoop/buckets/", &windows_separators());

    assert_eq!(normalized.name(), "scoop/buckets/custom");
}

#[test]
fn keeps_backslashes_when_the_user_typed_backslashes() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query(r"scoop\buckets\", &windows_separators());

    assert_eq!(normalized.name(), r"scoop\buckets\custom");
}

#[test]
fn appends_the_typed_separator_to_directories() {
    let normalized =
        directory(r".\scoop\buckets").normalized_for_query("scoop/", &windows_separators());

    assert_eq!(normalized.name(), "scoop/buckets/");
}

#[test]
fn does_not_double_up_a_trailing_separator() {
    let normalized =
        directory(r".\scoop\buckets\").normalized_for_query("scoop/", &windows_separators());

    assert_eq!(normalized.name(), "scoop/buckets/");
}

#[test]
fn leaves_files_without_a_trailing_separator() {
    let completion = ShellCompletion::new(r".\scoop\buckets\README.md".to_owned());

    let normalized = completion.normalized_for_query("scoop/buckets/RE", &windows_separators());

    assert_eq!(normalized.name(), "scoop/buckets/README.md");
}

/// A user who typed `./` must keep it: the replacement span covers their text, so dropping the
/// prefix would silently delete characters they wrote.
#[test]
fn keeps_an_explicit_relative_prefix_the_user_typed() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query("./scoop/", &windows_separators());

    assert_eq!(normalized.name(), "./scoop/buckets/custom");
}

/// A bare token like `scoop` carries no separator to mirror. The `.\` prefix still has to come
/// off, and directories still need the session's main separator appended, otherwise accepting
/// the completion yields `.\scoop ` and the next Tab cannot descend into it.
#[test]
fn a_bare_token_drops_the_relative_prefix_and_keeps_directories_open() {
    let normalized = directory(r".\scoop").normalized_for_query("scoop", &windows_separators());

    assert_eq!(normalized.name(), r"scoop\");
}

/// The same bare token in a Unix session gets `/`, never `\`.
#[test]
fn a_bare_token_uses_the_session_main_separator() {
    let normalized = directory("./scoop").normalized_for_query("scoop", &unix_separators());

    assert_eq!(normalized.name(), "scoop/");
}

/// A bare file token still loses the prefix, but gains no trailing separator.
#[test]
fn a_bare_file_token_drops_the_relative_prefix() {
    let completion = ShellCompletion::new(r".\README.md".to_owned());

    let normalized = completion.normalized_for_query("REA", &windows_separators());

    assert_eq!(normalized.name(), "README.md");
}

/// A Unix session never accepts `\`, so a stray backslash is a literal character and must not be
/// rewritten into a separator.
#[test]
fn does_not_treat_backslashes_as_separators_in_a_unix_session() {
    let completion = ShellCompletion::new("a/b".to_owned());

    let normalized = completion.normalized_for_query("a/", &unix_separators());

    assert_eq!(normalized.name(), "a/b");
}

#[test]
fn preserves_the_description_and_suggestion_type() {
    let mut completion = ShellCompletion::new(r".\scoop\buckets".to_owned());
    completion.update(ShellCompletionUpdate::Description {
        value: "a bucket".to_owned(),
    });
    completion.update(ShellCompletionUpdate::IsDirectory { value: true });

    let normalized = completion.normalized_for_query("scoop/", &windows_separators());

    assert_eq!(normalized.description(), Some("a bucket"));
    assert_eq!(normalized.suggestion_type(), SuggestionType::Argument);
    assert!(normalized.is_directory());
}
