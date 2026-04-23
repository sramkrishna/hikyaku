// Plugin modules — optional features that extend the core client.
//
// Each plugin:
//   - Lives in its own subdirectory
//   - Has a corresponding Cargo feature flag
//   - Defines its own data types and local storage
//   - Integrates with the core via MatrixEvent/MatrixCommand variants
//     wrapped in #[cfg(feature = "<plugin>")] guards

#[cfg(feature = "ai")]
pub mod ai;
#[cfg(feature = "motd")]
pub mod motd;
#[cfg(feature = "pinning")]
pub mod pinning;
#[cfg(feature = "rolodex")]
pub mod rolodex;
#[cfg(feature = "community-health")]
pub mod community_health;
#[cfg(feature = "community-safety")]
pub mod community_safety;
