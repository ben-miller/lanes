pub mod activate;
pub mod add;
pub mod deactivate;
pub mod doctor;
pub mod focus;
pub mod ls;
pub mod logs;
pub mod remove;
pub mod scope;
pub mod shell_init;

/// Tests that point HOME at a fake one (add, remove, scope) take this
/// first: HOME is the whole process's, so one lock for all of them, or
/// one module's test reads another's fake HOME.
#[cfg(test)]
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
