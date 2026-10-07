pub mod core {
    use std::{collections::HashMap, sync::Arc};

    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    pub struct MailboxMessageKey {
        pub mailbox: Arc<str>,
        pub uidvalidity: Option<u64>,
        pub uid: String,
    }

    impl MailboxMessageKey {
        pub fn with_shared_mailbox(
            mailbox: Arc<str>,
            uidvalidity: Option<u64>,
            uid: impl Into<String>,
        ) -> Self {
            Self {
                mailbox,
                uidvalidity,
                uid: uid.into(),
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ExtractedMessage {
        pub message_id: Option<String>,
        pub uid: Option<String>,
        pub size_bytes: Option<u64>,
        pub internal_date: Option<String>,
        pub flags: Option<String>,
    }

    pub type ExtractedMessages = HashMap<MailboxMessageKey, ExtractedMessage>;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct NamespaceEntry {
        pub prefix: String,
        pub delimiter: Option<char>,
    }

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct NamespaceInfo {
        pub personal: Vec<NamespaceEntry>,
        pub shared: Vec<NamespaceEntry>,
        pub other_users: Vec<NamespaceEntry>,
    }
}

pub mod imap_protocol {
    #[allow(dead_code)]
    pub fn atom_eq(actual: &str, expected: &str) -> bool {
        actual.eq_ignore_ascii_case(expected)
    }
}
