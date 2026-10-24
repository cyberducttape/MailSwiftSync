//! Propose explicit folder mappings from the special-use roles both servers
//! advertise.
//!
//! imapsync's `--automap` maps special folders by its own heuristics at run
//! time, which the independent verifier cannot replay, so automap plans are
//! never certified. Explicit mappings are replayable. Proposing them from the
//! RFC 6154 roles of both accounts gives the same convenience (for example
//! Gmail's `[Gmail]/Sent Mail` → Microsoft 365's `Sent Items`) while keeping
//! certification possible. Every proposal is shown to the operator before it
//! changes the plan.

use super::FolderMappingRule;
use crate::imap_probe::MailboxDescriptor;

/// Special-folder roles that map one-to-one between providers.
const MAPPED_ROLES: [FolderRole; 5] = [
    FolderRole::Sent,
    FolderRole::Drafts,
    FolderRole::Trash,
    FolderRole::Junk,
    FolderRole::Archive,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FolderRole {
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    /// Gmail's "All Mail": every message, including archived mail that has
    /// no other label.
    All,
}

impl FolderRole {
    pub(crate) fn label_key(self) -> &'static str {
        match self {
            Self::Sent => "ui.folder-role-sent",
            Self::Drafts => "ui.folder-role-drafts",
            Self::Trash => "ui.folder-role-trash",
            Self::Junk => "ui.folder-role-junk",
            Self::Archive => "ui.folder-role-archive",
            Self::All => "ui.folder-role-all",
        }
    }

    fn from_special_use(attribute: &str) -> Option<Self> {
        let attribute = attribute.trim_start_matches('\\');
        [
            ("sent", Self::Sent),
            ("drafts", Self::Drafts),
            ("trash", Self::Trash),
            ("junk", Self::Junk),
            ("archive", Self::Archive),
            ("all", Self::All),
        ]
        .into_iter()
        .find(|(name, _)| attribute.eq_ignore_ascii_case(name))
        .map(|(_, role)| role)
    }

    /// Name-based fallback for a server that advertises no roles at all.
    fn from_name(folder: &str) -> Option<Self> {
        match crate::runner::automap_folder_kind(folder)? {
            "sent" => Some(Self::Sent),
            "drafts" => Some(Self::Drafts),
            "trash" => Some(Self::Trash),
            "junk" => Some(Self::Junk),
            // "All Mail" is not an archive folder; leave it to the operator.
            "archive" if !folder.to_ascii_lowercase().ends_with("all mail") => Some(Self::Archive),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum FolderRoleNote {
    /// More than one folder on one side claims the role; nothing is mapped.
    Ambiguous { role: FolderRole, source_side: bool },
    /// The source has the role but the destination does not; imapsync will
    /// create the source name on the destination.
    MissingOnDestination { role: FolderRole, source: String },
    /// The source exposes an All Mail folder. Migrating it duplicates labeled
    /// mail; excluding it drops archived mail that has no other label. The
    /// operator must decide.
    AllMail { source: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpecialFolderProposal {
    pub(crate) rules: Vec<FolderMappingRule>,
    pub(crate) notes: Vec<FolderRoleNote>,
}

fn folders_with_role(folders: &[MailboxDescriptor], role: FolderRole) -> Vec<&str> {
    let advertises_roles = folders.iter().any(|folder| !folder.special_use.is_empty());
    folders
        .iter()
        .filter(|folder| folder.selectable)
        .filter(|folder| {
            if advertises_roles {
                folder
                    .special_use
                    .iter()
                    .any(|attribute| FolderRole::from_special_use(attribute) == Some(role))
            } else {
                FolderRole::from_name(&folder.wire_name) == Some(role)
            }
        })
        .map(|folder| folder.wire_name.as_str())
        .collect()
}

/// Pair each special-folder role across the two accounts. Rules are proposed
/// only for unambiguous pairs whose names differ and whose source folder has
/// no mapping or exclusion yet.
pub(crate) fn propose_special_folder_mappings(
    source: &[MailboxDescriptor],
    destination: &[MailboxDescriptor],
    existing: &[FolderMappingRule],
) -> SpecialFolderProposal {
    let mut proposal = SpecialFolderProposal::default();
    for role in MAPPED_ROLES {
        let source_folders = folders_with_role(source, role);
        let destination_folders = folders_with_role(destination, role);
        match (source_folders.as_slice(), destination_folders.as_slice()) {
            ([], _) => {}
            ([_, _, ..], _) => proposal.notes.push(FolderRoleNote::Ambiguous {
                role,
                source_side: true,
            }),
            ([source_folder], []) => proposal.notes.push(FolderRoleNote::MissingOnDestination {
                role,
                source: (*source_folder).to_owned(),
            }),
            ([_], [_, _, ..]) => proposal.notes.push(FolderRoleNote::Ambiguous {
                role,
                source_side: false,
            }),
            ([source_folder], [destination_folder]) => {
                let already_ruled = existing.iter().any(|rule| rule.source == *source_folder);
                if source_folder != destination_folder && !already_ruled {
                    proposal.rules.push(FolderMappingRule {
                        source: (*source_folder).to_owned(),
                        destination: (*destination_folder).to_owned(),
                        exclude: false,
                    });
                }
            }
        }
    }
    for folder in folders_with_role(source, FolderRole::All) {
        if !existing.iter().any(|rule| rule.source == folder) {
            proposal.notes.push(FolderRoleNote::AllMail {
                source: folder.to_owned(),
            });
        }
    }
    proposal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str, roles: &[&str]) -> MailboxDescriptor {
        MailboxDescriptor {
            wire_name: name.to_owned(),
            delimiter: Some("/".to_owned()),
            special_use: roles.iter().map(|role| (*role).to_owned()).collect(),
            selectable: true,
        }
    }

    fn rule(source: &str, destination: &str) -> FolderMappingRule {
        FolderMappingRule {
            source: source.to_owned(),
            destination: destination.to_owned(),
            exclude: false,
        }
    }

    #[test]
    fn gmail_to_microsoft_365_maps_roles_and_flags_all_mail() {
        let gmail = [
            folder("INBOX", &[]),
            folder("[Gmail]/Sent Mail", &["\\Sent"]),
            folder("[Gmail]/Drafts", &["\\Drafts"]),
            folder("[Gmail]/Trash", &["\\Trash"]),
            folder("[Gmail]/Spam", &["\\Junk"]),
            folder("[Gmail]/All Mail", &["\\All"]),
            folder("Projects", &[]),
        ];
        let microsoft = [
            folder("INBOX", &[]),
            folder("Sent Items", &["\\Sent"]),
            folder("Drafts", &["\\Drafts"]),
            folder("Deleted Items", &["\\Trash"]),
            folder("Junk Email", &["\\Junk"]),
            folder("Archive", &["\\Archive"]),
        ];
        let proposal = propose_special_folder_mappings(&gmail, &microsoft, &[]);
        assert_eq!(
            proposal.rules,
            [
                rule("[Gmail]/Sent Mail", "Sent Items"),
                rule("[Gmail]/Drafts", "Drafts"),
                rule("[Gmail]/Trash", "Deleted Items"),
                rule("[Gmail]/Spam", "Junk Email"),
            ]
        );
        assert_eq!(
            proposal.notes,
            [FolderRoleNote::AllMail {
                source: "[Gmail]/All Mail".to_owned()
            }]
        );
    }

    #[test]
    fn same_names_existing_rules_and_ambiguity_produce_no_rule() {
        let source = [
            folder("Sent", &["\\Sent"]),
            folder("Trash", &["\\Trash"]),
            folder("Old Trash", &["\\Trash"]),
            folder("Junk", &["\\Junk"]),
            folder("Drafts", &["\\Drafts"]),
        ];
        let destination = [
            folder("Sent", &["\\Sent"]),
            folder("Deleted Items", &["\\Trash"]),
            folder("Junk Email", &["\\Junk"]),
        ];
        let existing = [rule("Junk", "Spam")];
        let proposal = propose_special_folder_mappings(&source, &destination, &existing);
        assert!(proposal.rules.is_empty(), "{:?}", proposal.rules);
        assert_eq!(
            proposal.notes,
            [
                FolderRoleNote::MissingOnDestination {
                    role: FolderRole::Drafts,
                    source: "Drafts".to_owned()
                },
                FolderRoleNote::Ambiguous {
                    role: FolderRole::Trash,
                    source_side: true
                },
            ]
        );
    }

    #[test]
    fn servers_without_roles_fall_back_to_folder_names() {
        let source = [
            folder("INBOX", &[]),
            folder("Sent Messages", &[]),
            folder("Spam", &[]),
        ];
        let destination = [
            folder("INBOX", &[]),
            folder("Sent Items", &[]),
            folder("Junk Email", &[]),
        ];
        let proposal = propose_special_folder_mappings(&source, &destination, &[]);
        assert_eq!(
            proposal.rules,
            [
                rule("Sent Messages", "Sent Items"),
                rule("Spam", "Junk Email")
            ]
        );
    }
}
