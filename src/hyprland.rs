//! Hyprland compositor detection (env probe only — no HIS socket).

/// `true` when `HYPRLAND_INSTANCE_SIGNATURE` is set in the environment.
pub fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn hyprland_when_hyprland_instance_signature_set() {
        let _guard = env_lock();
        const TOKEN: &str = "HYPRLAND_INSTANCE_SIGNATURE";
        assert_eq!(TOKEN, "HYPRLAND_INSTANCE_SIGNATURE");
        // SAFETY: test-only; serialized via ENV_LOCK.
        unsafe { std::env::set_var(TOKEN, "probe") };
        assert!(is_hyprland());
        unsafe { std::env::remove_var(TOKEN) };
        assert!(!is_hyprland());
    }
}
