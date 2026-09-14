//! Durable transfer-engine selection and operator-facing descriptions.

use serde::{Deserialize, Serialize};

/// The transfer engine is a policy decision, not an implementation detail.
/// Dovecot destinations should use the destination server's own dsync engine;
/// imapsync remains available for arbitrary IMAP destinations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Engine {
    #[default]
    Auto,
    Dovecot,
    ImapSync,
}

impl Engine {
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Conservative default",
            Self::Dovecot => "Dovecot native",
            Self::ImapSync => "imapsync fallback",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Auto => {
                "Use imapsync as the conservative default; select Dovecot native explicitly when appropriate."
            }
            Self::Dovecot => {
                "Use destination-side doveadm/dsync when the destination is Dovecot and admin access is available."
            }
            Self::ImapSync => {
                "Use imapsync when both ends are arbitrary IMAP servers or no destination admin stack is available."
            }
        }
    }
}
