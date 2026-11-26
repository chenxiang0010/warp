use warpui::App;

use super::*;
use crate::terminal::model::session::SessionId;
use crate::terminal::model::session::SessionInfo;
use crate::terminal::model_events::ModelEventDispatcher;
use crate::terminal::shell::ShellType;
use crate::test_util::assert_eventually;

fn line_editor_status_for(
    app: &mut App,
    shell_type: ShellType,
) -> (
    ModelHandle<LineEditorStatus>,
    ModelHandle<ModelEventDispatcher>,
) {
    let (_tx, rx) = async_channel::unbounded();
    let sessions = app.add_model(|_| {
        let mut sessions = Sessions::new_for_test();
        sessions.register_session_for_test(SessionInfo::new_for_test().with_shell_type(shell_type));
        sessions
    });
    let model_events = app.add_model(|ctx| {
        let mut dispatcher = ModelEventDispatcher::new(rx, sessions.clone(), ctx);
        dispatcher.set_active_session_id(SessionInfo::new_for_test().session_id);
        dispatcher
    });
    let status = app.add_model(|ctx| LineEditorStatus::new(model_events.clone(), sessions, ctx));
    (status, model_events)
}

fn emit_precmd(model_events: &ModelHandle<ModelEventDispatcher>, ctx: &mut warpui::AppContext) {
    model_events.update(ctx, |_, ctx| {
        ctx.emit(ModelEvent::Handler(AnsiHandlerEvent::Precmd));
    });
}

#[test]
fn precmd_activates_the_line_editor_without_an_active_session() {
    App::test((), |mut app| async move {
        let (_tx, rx) = async_channel::unbounded();
        let sessions = app.add_model(|_| Sessions::new_for_test());
        // No active session id is ever set: on a slow WSL start the prompt hook can arrive before
        // the session is registered, and the line editor still has to become active or every
        // queued write is stranded behind it.
        let model_events =
            app.add_model(|ctx| ModelEventDispatcher::new(rx, sessions.clone(), ctx));
        let status =
            app.add_model(|ctx| LineEditorStatus::new(model_events.clone(), sessions, ctx));

        assert!(!status.read(&app, |status, _| status.is_line_editor_active()));
        app.update(|ctx| emit_precmd(&model_events, ctx));

        assert_eventually!(
            40 => status.read(&app, |status, _| status.is_line_editor_active()),
            "a precmd with no resolvable active session should still activate the line editor"
        );
    });
}

#[test]
fn precmd_activates_the_line_editor_when_the_session_is_not_registered() {
    App::test((), |mut app| async move {
        let (status, model_events) = line_editor_status_for(&mut app, ShellType::Bash);
        app.update(|ctx| {
            // Point the dispatcher at a session the `Sessions` model has never heard of.
            model_events.update(ctx, |dispatcher, _| {
                dispatcher.set_active_session_id(SessionId::from(999));
            });
            emit_precmd(&model_events, ctx);
        });

        assert_eventually!(
            40 => status.read(&app, |status, _| status.is_line_editor_active()),
            "an unregistered active session should not swallow the prompt hook"
        );
    });
}

#[test]
fn preexec_deactivates_the_line_editor() {
    App::test((), |mut app| async move {
        let (status, model_events) = line_editor_status_for(&mut app, ShellType::Bash);
        app.update(|ctx| emit_precmd(&model_events, ctx));
        assert_eventually!(
            40 => status.read(&app, |status, _| status.is_line_editor_active()),
            "the prompt hook should activate the line editor"
        );

        app.update(|ctx| {
            model_events.update(ctx, |_, ctx| {
                ctx.emit(ModelEvent::Handler(AnsiHandlerEvent::Preexec));
            });
        });
        assert!(!status.read(&app, |status, _| status.is_line_editor_active()));
    });
}

#[test]
fn zsh_waits_for_end_prompt_after_precmd_before_activating() {
    App::test((), |mut app| async move {
        let (status, model_events) = line_editor_status_for(&mut app, ShellType::Zsh);
        app.update(|ctx| emit_precmd(&model_events, ctx));

        // zsh emits precmd before the prompt markers, so precmd alone must not activate.
        warpui::r#async::Timer::after(Duration::from_millis(120)).await;
        assert!(!status.read(&app, |status, _| status.is_line_editor_active()));

        app.update(|ctx| {
            model_events.update(ctx, |_, ctx| {
                ctx.emit(ModelEvent::Handler(AnsiHandlerEvent::EndPrompt));
            });
        });
        assert_eventually!(
            40 => status.read(&app, |status, _| status.is_line_editor_active()),
            "zsh should activate once end prompt follows precmd"
        );
    });
}
