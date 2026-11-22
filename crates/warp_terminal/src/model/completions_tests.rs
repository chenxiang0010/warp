use super::*;

const WINDOWS_SEPARATORS: &[char] = &['/', '\\'];
const UNIX_SEPARATORS: &[char] = &['/'];

fn directory(name: &str) -> ShellCompletion {
    let mut completion = ShellCompletion::new(name.to_owned());
    completion.update(ShellCompletionUpdate::IsDirectory { value: true });
    completion
}

#[test]
fn rewrites_backslashes_to_the_separator_the_user_typed() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query("scoop/buckets/", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), "scoop/buckets/custom");
}

#[test]
fn keeps_backslashes_when_the_user_typed_backslashes() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query(r"scoop\buckets\", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), r"scoop\buckets\custom");
}

#[test]
fn appends_the_typed_separator_to_directories() {
    let normalized =
        directory(r".\scoop\buckets").normalized_for_query("scoop/", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), "scoop/buckets/");
}

#[test]
fn does_not_double_up_a_trailing_separator() {
    let normalized =
        directory(r".\scoop\buckets\").normalized_for_query("scoop/", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), "scoop/buckets/");
}

#[test]
fn leaves_files_without_a_trailing_separator() {
    let completion = ShellCompletion::new(r".\scoop\buckets\README.md".to_owned());

    let normalized = completion.normalized_for_query("scoop/buckets/RE", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), "scoop/buckets/README.md");
}

/// A user who typed `./` must keep it: the replacement span covers their text, so dropping the
/// prefix would silently delete characters they wrote.
#[test]
fn keeps_an_explicit_relative_prefix_the_user_typed() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query("./scoop/", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), "./scoop/buckets/custom");
}

/// Without a separator in the query there is nothing to mirror, so the completion is left alone.
#[test]
fn leaves_completions_untouched_when_the_query_has_no_separator() {
    let completion = ShellCompletion::new(r".\scoop\buckets\custom".to_owned());

    let normalized = completion.normalized_for_query("sco", WINDOWS_SEPARATORS);

    assert_eq!(normalized.name(), r".\scoop\buckets\custom");
}

/// A Unix session never accepts `\`, so a stray backslash is a literal character and must not be
/// rewritten into a separator.
#[test]
fn does_not_treat_backslashes_as_separators_in_a_unix_session() {
    let completion = ShellCompletion::new("a/b".to_owned());

    let normalized = completion.normalized_for_query("a/", UNIX_SEPARATORS);

    assert_eq!(normalized.name(), "a/b");
}

#[test]
fn preserves_the_description_and_suggestion_type() {
    let mut completion = ShellCompletion::new(r".\scoop\buckets".to_owned());
    completion.update(ShellCompletionUpdate::Description {
        value: "a bucket".to_owned(),
    });
    completion.update(ShellCompletionUpdate::IsDirectory { value: true });

    let normalized = completion.normalized_for_query("scoop/", WINDOWS_SEPARATORS);

    assert_eq!(normalized.description(), Some("a bucket"));
    assert_eq!(normalized.suggestion_type(), SuggestionType::Argument);
    assert!(normalized.is_directory());
}
