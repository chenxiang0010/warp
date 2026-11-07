use settings::{PrivatePreferences, PublicPreferences};
use warpui::{AddSingletonModel, App};
use warpui_extras::user_preferences;

use super::*;
use crate::ai::credit_availability::{AICreditAvailability, AICreditDenialReason};
use crate::ai::llms::{AvailableLLMs, LLMId, LLMInfo, LLMPreferences, ModelsByFeature};
use crate::auth::AuthManager;
use crate::cloud_object::model::actions::ObjectActions;
use crate::cloud_object::model::persistence::CloudModel;
use crate::server::ids::ServerId;
use crate::server::server_api::team::MockTeamClient;
use crate::server::server_api::workspace::{MockWorkspaceClient, WorkspaceClient};
use crate::server::sync_queue::SyncQueue;
use crate::server::telemetry::context_provider::AppTelemetryContextProvider;
use crate::settings::{AISettings, CodeSettings, PrivacySettings};
use crate::system::SystemStats;
use crate::workspaces::team::Team;
use crate::workspaces::user_profiles::UserProfiles;
use crate::workspaces::user_workspaces::UserWorkspacesEvent;
use crate::workspaces::workspace::{
    ChargeUnit, PurchaseAddOnCreditsPolicy, UserTier, Workspace, WorkspaceUid,
};

fn initialize_app(
    team_client: Arc<dyn TeamClient>,
    workspace_client: Arc<dyn WorkspaceClient>,
    workspaces: Vec<Workspace>,
    app: &mut App,
) {
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(|_| SystemStats::new());
    app.add_singleton_model(TeamTesterStatus::new);
    app.add_singleton_model(|ctx| {
        UserWorkspaces::mock(
            team_client.clone(),
            workspace_client.clone(),
            workspaces,
            ctx,
        )
    });
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(|_| ObjectActions::new(vec![]));
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(|_| UserProfiles::new(vec![]));
    app.add_singleton_model(|_| ServerApiProvider::new_for_test());
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(AppTelemetryContextProvider::new_context_provider);
    app.add_singleton_model(AuthManager::new_for_test);
}

#[test]
fn out_of_band_metadata_refresh_applies_metadata_without_waiting_for_the_receiver() {
    App::test((), |mut app| async move {
        let mut attempts = 0;
        let mut team_client = MockTeamClient::new();
        team_client
            .expect_workspaces_metadata()
            .times(2)
            .returning(move || {
                attempts += 1;
                if attempts == 1 {
                    return Err(anyhow::anyhow!("workspace metadata unavailable"));
                }
                Ok(WorkspacesMetadataWithPricing {
                    metadata: WorkspacesMetadataResponse {
                        workspaces: vec![],
                        joinable_teams: vec![],
                        experiments: None,
                        ai_credit_availability: None,
                        user_tier: UserTier {
                            charge_unit: ChargeUnit::Cents,
                            ..Default::default()
                        },
                    },
                    pricing_info: None,
                })
            });
        let team_client = Arc::new(team_client);
        initialize_app(
            team_client.clone(),
            Arc::new(MockWorkspaceClient::new()),
            vec![],
            &mut app,
        );
        let manager = app.add_singleton_model(|ctx| TeamUpdateManager::new(team_client, None, ctx));
        let (sender, applied) = oneshot::channel();
        app.update(|ctx| {
            let mut sender = Some(sender);
            ctx.subscribe_to_model(&UserWorkspaces::handle(ctx), move |_, event, _| {
                if matches!(event, UserWorkspacesEvent::TeamsChanged)
                    && let Some(sender) = sender.take()
                {
                    let _ = sender.send(());
                }
            });
        });

        manager.update(&mut app, |manager, ctx| {
            drop(manager.refresh_workspace_metadata(ctx));
        });
        applied.await.unwrap();

        app.read(|ctx| {
            assert_eq!(UserWorkspaces::as_ref(ctx).charge_unit(), ChargeUnit::Cents);
        });
    });
}

#[test]
fn test_workspace_metadata_piggyback_feeds_ai_credit_availability() {
    App::test((), |mut app| async move {
        let team_client = Arc::new(MockTeamClient::new());
        initialize_app(
            team_client.clone(),
            Arc::new(MockWorkspaceClient::new()),
            vec![],
            &mut app,
        );
        if app
            .models_of_type::<settings::PrivatePreferences>()
            .is_empty()
        {
            app.update(crate::settings::init_and_register_user_preferences);
        }
        app.add_singleton_model(|ctx| {
            AIRequestUsageModel::new_for_test(ServerApiProvider::as_ref(ctx).get_ai_client(), ctx)
        });
        let team_update_manager =
            app.add_singleton_model(|ctx| TeamUpdateManager::new(team_client, None, ctx));

        let availability = AICreditAvailability::unavailable(AICreditDenialReason::OutOfCredits);
        team_update_manager.update(&mut app, |manager, ctx| {
            manager.on_workspaces_updated(
                Ok(WorkspacesMetadataResponse {
                    workspaces: vec![],
                    joinable_teams: vec![],
                    experiments: None,
                    ai_credit_availability: Some(availability),
                    user_tier: Default::default(),
                }),
                ctx,
            );
        });

        AIRequestUsageModel::handle(&app).read(&app, |model, _| {
            assert_eq!(model.server_availability(), Some(availability));
        });
    });
}

#[test]
fn test_poll_path_apply_refreshes_user_tier() {
    App::test((), |mut app| async move {
        let team_client = Arc::new(MockTeamClient::new());
        let workspace_client = Arc::new(MockWorkspaceClient::new());
        initialize_app(team_client.clone(), workspace_client, vec![], &mut app);

        let team_update_manager =
            app.add_singleton_model(|ctx| TeamUpdateManager::new(team_client, None, ctx));

        // The periodic poll applies metadata through TeamUpdateManager's
        // own on_workspaces_updated; it must refresh the stored user-level
        // tier.
        let response_with_tier = WorkspacesMetadataResponse {
            workspaces: vec![],
            joinable_teams: vec![],
            experiments: None,
            ai_credit_availability: None,
            user_tier: UserTier {
                purchase_policy: Some(PurchaseAddOnCreditsPolicy {
                    enabled: false,
                    premium_enabled: true,
                    price_premium_bps: 1000,
                }),
                charge_unit: ChargeUnit::Cents,
            },
        };
        team_update_manager.update(&mut app, |manager, ctx| {
            manager.on_workspaces_updated(Ok(response_with_tier), ctx);
        });
        app.read(|ctx| {
            let user_workspaces = UserWorkspaces::as_ref(ctx);
            assert!(
                user_workspaces
                    .purchase_policy()
                    .is_some_and(|policy| policy.allows_purchases()),
                "a poll-path apply should store the user-level policy"
            );
            assert_eq!(
                user_workspaces.charge_unit(),
                ChargeUnit::Cents,
                "a poll-path apply should store the user-level charge unit"
            );
        });

        // A later poll without the tier terms must clear the stored fallback
        // so it can't go stale.
        let response_without_tier = WorkspacesMetadataResponse {
            workspaces: vec![],
            joinable_teams: vec![],
            experiments: None,
            ai_credit_availability: None,
            user_tier: Default::default(),
        };
        team_update_manager.update(&mut app, |manager, ctx| {
            manager.on_workspaces_updated(Ok(response_without_tier), ctx);
        });
        app.read(|ctx| {
            let user_workspaces = UserWorkspaces::as_ref(ctx);
            assert!(
                user_workspaces.purchase_policy().is_none(),
                "a poll-path apply without the policy should clear the stored fallback"
            );
            assert_eq!(
                user_workspaces.charge_unit(),
                ChargeUnit::Credits,
                "a poll-path apply without the tier should fall back to credits"
            );
        });
    });
}

fn models_by_feature_with_model(model_id: &str) -> ModelsByFeature {
    let available =
        AvailableLLMs::new(model_id.into(), vec![LLMInfo::new_for_test(model_id)], None)
            .expect("choices are non-empty");
    ModelsByFeature {
        agent_mode: available.clone(),
        coding: available.clone(),
        cli_agent: Some(available.clone()),
        computer_use: Some(available),
    }
}

fn initialize_llm_preferences_dependencies(app: &mut App) {
    app.add_singleton_model(|_| {
        PublicPreferences::new(Box::<user_preferences::in_memory::InMemoryPreferences>::default())
    });
    app.add_singleton_model(|_| {
        PrivatePreferences::new(Box::<user_preferences::in_memory::InMemoryPreferences>::default())
    });
    app.add_singleton_model(CodeSettings::new_with_defaults);
    app.add_singleton_model(AISettings::new_with_defaults);
    app.update(|ctx| {
        warpui_extras::secure_storage::register_noop("test", ctx);
    });
    app.add_singleton_model(ai::api_keys::ApiKeyManager::new);
    app.add_singleton_model(|_| crate::ai::mcp::TemplatableMCPServerManager::default());
    app.add_singleton_model(|ctx| {
        crate::ai::execution_profiles::profiles::AIExecutionProfilesModel::new(
            &crate::LaunchMode::new_for_unit_test(),
            ctx,
        )
    });
}

#[test]
fn on_workspaces_updated_keeps_teams_distinct_and_prunes_a_team_the_response_omits() {
    App::test((), |mut app| async move {
        let team_client = Arc::new(MockTeamClient::new());
        initialize_app(
            team_client.clone(),
            Arc::new(MockWorkspaceClient::new()),
            vec![],
            &mut app,
        );
        initialize_llm_preferences_dependencies(&mut app);
        app.add_singleton_model(LLMPreferences::new);
        let team_update_manager =
            app.add_singleton_model(|ctx| TeamUpdateManager::new(team_client, None, ctx));

        let workspace_uid = WorkspaceUid::from(ServerId::from(999));
        let team_a = ServerId::from(1);
        let team_b = ServerId::from(2);
        let model_a = LLMId::from("team-a-only");
        let model_b = LLMId::from("team-b-only");

        let workspace_with_teams = |teams: Vec<Team>| {
            Workspace::from_local_cache(
                workspace_uid,
                "Test Workspace".to_owned(),
                Some(teams),
                None,
            )
        };
        let team_with_model = |uid: ServerId, model_id: &str| {
            Team::from_local_cache(
                uid,
                format!("Team {uid}"),
                None,
                None,
                None,
                Some(models_by_feature_with_model(model_id)),
            )
        };

        team_update_manager.update(&mut app, |manager, ctx| {
            manager.on_workspaces_updated(
                Ok(WorkspacesMetadataResponse {
                    workspaces: vec![workspace_with_teams(vec![
                        team_with_model(team_a, model_a.as_str()),
                        team_with_model(team_b, model_b.as_str()),
                    ])],
                    joinable_teams: vec![],
                    experiments: None,
                    ai_credit_availability: None,
                    user_tier: Default::default(),
                }),
                ctx,
            );
        });

        app.read(|ctx| {
            let workspaces = UserWorkspaces::as_ref(ctx);
            assert!(
                workspaces
                    .team_from_uid(team_a)
                    .is_some_and(|team| team.feature_model_choice.info_for_id(&model_a).is_some()),
                "team A's own model should be visible in its own bucket"
            );
            assert!(
                workspaces
                    .team_from_uid(team_b)
                    .is_some_and(|team| team.feature_model_choice.info_for_id(&model_b).is_some()),
                "team B's own model should be visible in its own bucket"
            );
            assert!(
                workspaces
                    .team_from_uid(team_a)
                    .is_some_and(|team| team.feature_model_choice.info_for_id(&model_b).is_none()),
                "team A's bucket must not contain team B's model"
            );
            assert!(
                workspaces
                    .team_from_uid(team_b)
                    .is_some_and(|team| team.feature_model_choice.info_for_id(&model_a).is_none()),
                "team B's bucket must not contain team A's model"
            );
        });

        team_update_manager.update(&mut app, |manager, ctx| {
            manager.on_workspaces_updated(
                Ok(WorkspacesMetadataResponse {
                    workspaces: vec![workspace_with_teams(vec![team_with_model(
                        team_b,
                        model_b.as_str(),
                    )])],
                    joinable_teams: vec![],
                    experiments: None,
                    ai_credit_availability: None,
                    user_tier: Default::default(),
                }),
                ctx,
            );
        });

        app.read(|ctx| {
            let workspaces = UserWorkspaces::as_ref(ctx);
            assert!(
                workspaces.team_from_uid(team_a).is_none(),
                "team A's catalog bucket should have been evicted once the response stopped \
                 naming it, not left stale"
            );
            assert!(
                workspaces
                    .team_from_uid(team_b)
                    .is_some_and(|team| team.feature_model_choice.info_for_id(&model_b).is_some()),
                "team B's catalog should remain"
            );
        });

        team_update_manager.update(&mut app, |manager, ctx| {
            manager.on_workspaces_updated(
                Ok(WorkspacesMetadataResponse {
                    workspaces: vec![workspace_with_teams(vec![])],
                    joinable_teams: vec![],
                    experiments: None,
                    ai_credit_availability: None,
                    user_tier: Default::default(),
                }),
                ctx,
            );
        });

        app.read(|ctx| {
            let workspaces = UserWorkspaces::as_ref(ctx);
            assert!(
                workspaces.team_from_uid(team_b).is_none(),
                "an authoritative empty catalog must prune every remaining bucket, not be \
                 skipped as a no-op"
            );
            LLMPreferences::as_ref(ctx).get_default_base_model_for_team_uid(None, ctx);
        });
    });
}
