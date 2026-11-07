use super::LLMProvider;

#[test]
fn no_first_party_provider_is_offered_in_the_settings_ui() {
    // This build resolves models only through user-defined custom endpoints, so the
    // settings UI must not render a first-party API-key editor for any provider.
    assert!(LLMProvider::API_KEY_PROVIDERS.is_empty());
}

#[test]
fn cli_provider_slugs_still_parse() {
    // `API_KEY_PROVIDER_VALUE_NAME` documents the slugs accepted by the TUI's
    // `--set-provider-api-key` flag, which is independent of the settings UI list.
    for slug in LLMProvider::API_KEY_PROVIDER_VALUE_NAME.split('|') {
        assert!(
            LLMProvider::from_api_key_slug(slug).is_ok(),
            "{slug} should parse"
        );
    }
}

#[test]
fn only_pasted_key_providers_report_pasted_key_support() {
    for provider in [
        LLMProvider::OpenAI,
        LLMProvider::Anthropic,
        LLMProvider::Google,
        LLMProvider::Xai,
    ] {
        assert_eq!(
            provider.supports_pasted_api_key(),
            matches!(
                provider,
                LLMProvider::OpenAI | LLMProvider::Anthropic | LLMProvider::Google
            )
        );
    }
}
