/// Service name for the desktop OS keyring. Debug builds default to a distinct
/// service, while standalone worktree launches may request a scoped dev service.
fn dev_keyring_service(configured: Option<String>) -> String {
    configured
        .filter(|service| service.starts_with("buzz-desktop-dev."))
        .unwrap_or_else(|| "buzz-desktop-dev".to_string())
}

/// Compile-time build flavor (`BUZZ_DESKTOP_FLAVOR`, e.g. `rooh`).
///
/// A flavored release build (built with a matching Tauri config overlay that
/// changes the bundle identifier) gets its own keyring service and agent nest,
/// so it runs beside the stock app without sharing the owner identity, stored
/// agent keys or local agent state. Only `[a-z0-9-]{1,32}` is honored; any
/// other value is ignored and the build behaves as stock.
pub(crate) fn build_flavor() -> Option<&'static str> {
    option_env!("BUZZ_DESKTOP_FLAVOR").filter(|flavor| is_valid_flavor(flavor))
}

fn is_valid_flavor(flavor: &str) -> bool {
    !flavor.is_empty()
        && flavor.len() <= 32
        && flavor
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn release_keyring_service(flavor: Option<&str>) -> String {
    match flavor {
        Some(flavor) => format!("buzz-desktop.{flavor}"),
        None => "buzz-desktop".to_string(),
    }
}

pub(crate) fn keyring_service() -> &'static str {
    static SERVICE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SERVICE
        .get_or_init(|| {
            if cfg!(debug_assertions) {
                dev_keyring_service(std::env::var("BUZZ_DEV_KEYRING_SERVICE").ok())
            } else {
                release_keyring_service(build_flavor())
            }
        })
        .as_str()
}

pub(super) fn migration_marker_name(service: &str, default_name: &str) -> String {
    if service == "buzz-desktop" || service == "buzz-desktop-dev" {
        default_name.to_string()
    } else {
        format!("identity.{service}.migrated")
    }
}

#[cfg(test)]
mod tests {
    use super::{dev_keyring_service, is_valid_flavor, migration_marker_name, release_keyring_service};

    #[test]
    fn flavored_release_build_uses_its_own_service() {
        assert_eq!(release_keyring_service(None), "buzz-desktop");
        assert_eq!(release_keyring_service(Some("rooh")), "buzz-desktop.rooh");
        assert_eq!(
            migration_marker_name("buzz-desktop.rooh", "identity.migrated"),
            "identity.buzz-desktop.rooh.migrated"
        );
    }

    #[test]
    fn only_simple_flavors_are_honored() {
        assert!(is_valid_flavor("rooh"));
        assert!(is_valid_flavor("team-2"));
        assert!(!is_valid_flavor(""));
        assert!(!is_valid_flavor("Rooh"));
        assert!(!is_valid_flavor("../x"));
        assert!(!is_valid_flavor(&"a".repeat(33)));
    }

    #[test]
    fn standalone_scope_must_remain_under_dev_service() {
        assert_eq!(
            dev_keyring_service(Some("buzz-desktop-dev.example".to_string())),
            "buzz-desktop-dev.example"
        );
        assert_eq!(
            dev_keyring_service(Some("buzz-desktop".to_string())),
            "buzz-desktop-dev"
        );
    }

    #[test]
    fn standalone_scope_uses_its_own_migration_marker() {
        assert_eq!(
            migration_marker_name("buzz-desktop", "identity.migrated"),
            "identity.migrated"
        );
        assert_eq!(
            migration_marker_name("buzz-desktop-dev", "identity.migrated"),
            "identity.migrated"
        );
        assert_eq!(
            migration_marker_name("buzz-desktop-dev.example", "identity.migrated"),
            "identity.buzz-desktop-dev.example.migrated"
        );
    }
}
