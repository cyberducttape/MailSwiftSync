use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaResource {
    /// Provider-reported quota units. IMAP commonly reports STORAGE in KiB;
    /// the unit is intentionally preserved rather than guessed or converted.
    pub used: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCapabilities {
    pub values: BTreeSet<String>,
    pub inventory_complete: bool,
    pub mailbox_count: usize,
    pub special_use_mailboxes: usize,
    pub quota_observed: bool,
    pub quota_exceeded: bool,
    pub quota_resources: BTreeMap<String, QuotaResource>,
}

impl ServerCapabilities {
    #[cfg(test)]
    pub fn parse(response: &str) -> Self {
        Self::parse_with_inventory(response, "")
    }

    #[cfg(test)]
    pub fn parse_with_inventory(capability_response: &str, list_response: &str) -> Self {
        let (mailbox_count, special_use_mailboxes) = list_response
            .lines()
            .filter(|line| is_untagged_list_record(line))
            .fold((0, 0), |(mailbox_count, special_use_mailboxes), line| {
                let special_use = [
                    r"\ALL",
                    r"\ARCHIVE",
                    r"\DRAFTS",
                    r"\FLAGGED",
                    r"\JUNK",
                    r"\SENT",
                    r"\TRASH",
                ]
                .iter()
                .any(|attribute| crate::imap_protocol::list_has_attribute(line, attribute));
                (
                    mailbox_count + 1,
                    special_use_mailboxes + usize::from(special_use),
                )
            });
        Self::from_inventory_summary(capability_response, mailbox_count, special_use_mailboxes)
    }

    /// Build the capability model from a streaming LIST summary. The probe
    /// must not retain an entire folder inventory just to calculate these
    /// bounded facts.
    pub fn from_inventory_summary(
        capability_response: &str,
        mailbox_count: usize,
        special_use_mailboxes: usize,
    ) -> Self {
        let mut values = BTreeSet::new();
        for line in capability_response.lines() {
            let mut fields = line.split_whitespace();
            if fields.next() == Some("*")
                && fields
                    .next()
                    .is_some_and(|keyword| crate::imap_protocol::atom_eq(keyword, "CAPABILITY"))
            {
                for token in fields {
                    values.insert(token.to_ascii_uppercase());
                }
            }
        }
        Self {
            values,
            inventory_complete: mailbox_count > 0,
            mailbox_count,
            special_use_mailboxes,
            quota_observed: false,
            quota_exceeded: false,
            quota_resources: BTreeMap::new(),
        }
    }

    pub fn record_quota_response(&mut self, response: &str) {
        for line in response.lines() {
            let tokens = line.split_whitespace().collect::<Vec<_>>();
            if !crate::imap_protocol::is_untagged_response(line, "QUOTA") {
                continue;
            }
            let mut saw_valid_resource = false;
            let mut index = 2;
            while index + 2 < tokens.len() {
                let resource = tokens[index]
                    .trim_matches(['(', ')', '\r'])
                    .to_ascii_uppercase();
                if matches!(resource.as_str(), "STORAGE" | "MESSAGE" | "MESSAGES") {
                    let usage = tokens[index + 1].parse::<u64>();
                    let limit = tokens[index + 2]
                        .trim_matches(['(', ')', '\r'])
                        .parse::<u64>();
                    if let (Ok(usage), Ok(limit)) = (usage, limit) {
                        saw_valid_resource = true;
                        self.quota_resources
                            .insert(resource.clone(), QuotaResource { used: usage, limit });
                        if limit > 0 && usage >= limit {
                            self.quota_exceeded = true;
                        }
                    }
                    index += 3;
                } else {
                    index += 1;
                }
            }
            self.quota_observed |= saw_valid_resource;
        }
    }

    pub fn supports(&self, capability: &str) -> bool {
        self.values.contains(&capability.to_ascii_uppercase())
    }

    pub fn detected_capabilities(&self) -> Vec<&'static str> {
        let mut capabilities = vec!["UID support advertised"];
        if self.supports("QRESYNC") {
            capabilities.push("QRESYNC advertised");
        } else if self.supports("CONDSTORE") {
            capabilities.push("CONDSTORE advertised");
        }
        if self.supports("SPECIAL-USE") {
            capabilities.push("SPECIAL-USE advertised");
        }
        if self.supports("UIDPLUS") {
            capabilities.push("UIDPLUS advertised");
        }
        capabilities
    }
}

#[cfg(test)]
fn is_untagged_list_record(line: &str) -> bool {
    crate::imap_protocol::is_untagged_response(line, "LIST")
}
