use crate::term::{CommandInfo, ShellState};

#[derive(Debug, Clone)]
pub enum DelayedEvent {
    EndPrompt,
    NewCmd,
}

/// Terminal event.
///
/// These events instruct the TODO socket over changes that can't be handled by the terminal
/// emulation layer itself.
#[derive(Debug, Clone)]
pub enum Event<'a> {
    Prompt,
    PreExec,
    ShellChanged,
    CommandInfo(&'a CommandInfo),
}

/// Event Loop for sending info about terminal events.
pub trait EventListener {
    fn send_event(&self, _event: Event<'_>, _shell_state: &ShellState) {}
    fn log_level_event(&self, _level: Option<String>) {}
}

/// Null sink for events.
pub struct VoidListener;

impl EventListener for VoidListener {}
