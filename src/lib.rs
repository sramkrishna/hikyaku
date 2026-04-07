// Library façade — exposes plugin modules so that auxiliary binaries
// (e.g. src/bin/health_test.rs) can import them without pulling in
// the full GTK application stack.

#[cfg(any(feature = "ai", feature = "ai-flatpak", feature = "community-health"))]
pub mod intelligence {
    pub mod watcher;
}

#[cfg(feature = "community-health")]
pub mod plugins {
    pub mod community_health;
}
