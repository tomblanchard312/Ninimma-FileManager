// Ninimma modification: independent project branding and integration.
//! Protecting the search index: where it lives, who can read it, and the
//! keys that encrypt it.
//!
//! - [`permissions`]: the per-user index folder and its access rules.
//! - [`key_store`]: the index key in the OS credential store, which the
//!   desktop login unlocks.
//! - [`vault`]: a passphrase-protected key, for systems without one.
//!
//! The index key is random. It is never derived from, and never touches,
//! the user's login password.

pub mod key_store;
pub mod permissions;
pub mod secret;
pub mod vault;
