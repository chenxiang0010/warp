use std::path::PathBuf;

use super::*;

fn known_local(command: &str, shell_type: ShellType) -> AvailableShell {
    AvailableShell::new_local_executable(
        command.to_string(),
        PathBuf::from(format!(r"C:\fake\{command}")),
        shell_type,
    )
}

fn msys2(command: &str, shell_type: ShellType) -> AvailableShell {
    AvailableShell::new_msys2(
        command.to_string(),
        PathBuf::from(format!(r"C:\fake\msys\{command}")),
        shell_type,
    )
}

#[test]
fn only_pwsh_is_offered_among_windows_shells() {
    assert!(known_local("pwsh.exe", ShellType::PowerShell).selector_visible());
    assert!(!known_local("powershell.exe", ShellType::PowerShell).selector_visible());
    assert!(!known_local("bash.exe", ShellType::Bash).selector_visible());
    assert!(!known_local("zsh.exe", ShellType::Zsh).selector_visible());
    assert!(!known_local("fish.exe", ShellType::Fish).selector_visible());
}

#[test]
fn msys2_shells_are_never_offered() {
    assert!(!msys2("bash.exe", ShellType::Bash).selector_visible());
    assert!(!msys2("zsh.exe", ShellType::Zsh).selector_visible());
    assert!(!msys2("fish.exe", ShellType::Fish).selector_visible());
}

#[test]
fn wsl_and_default_shells_are_offered() {
    assert!(AvailableShell::default().selector_visible());
    assert!(AvailableShell::new_wsl("archlinux".to_string()).selector_visible());
}
