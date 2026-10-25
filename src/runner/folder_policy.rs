//! Destination-folder policy and deterministic automapping invariants.

use std::collections::{HashMap, HashSet};

use crate::imap_probe::MailboxDescriptor;
use crate::migration_plan::FolderMappingRule;

pub(crate) fn validate_destination_folder_policy(
    required: &HashSet<String>,
    actual: &HashSet<String>,
    strict: bool,
) -> Result<(), String> {
    let mut missing = required.difference(actual).cloned().collect::<Vec<_>>();
    missing.sort();
    if !missing.is_empty() {
        return Err(format!(
            "folder verification failed: required mapped destination folders are missing: {}",
            missing.join(", ")
        ));
    }
    if strict && required != actual {
        let mut unexpected = actual.difference(required).cloned().collect::<Vec<_>>();
        unexpected.sort();
        return Err(format!(
            "strict folder verification failed: destination contains unexpected selectable folders: {}",
            unexpected.join(", ")
        ));
    }
    Ok(())
}

/// Folder totals for the reconciled scope, as `(source, destination)`.
///
/// The source side counts the folders that were verified (exclusions left
/// out). The destination side counts the folders that receive them plus any
/// destination-only folder that holds mail. An empty destination-only folder
/// is left out: the additive policy keeps folders the source never had, such
/// as a provider's own folders or a parent imapsync creates for a nested
/// folder whose parent is not selectable on the source, and such a folder
/// carries nothing to reconcile. Counting it made exactly matching mail
/// look like a folder-count difference, which the ledger refuses to record
/// under an exact label. Mail in a destination-only folder is still counted
/// here and reported as extra messages.
pub(crate) fn reconciled_folder_counts(
    source_folders: &HashSet<String>,
    excluded_source_folders: &HashSet<String>,
    expected_destination_folders: &HashSet<String>,
    destination_folders: &HashSet<String>,
    destination_messages: impl Fn(&str) -> u64,
) -> (u64, u64) {
    let source = source_folders
        .iter()
        .filter(|folder| !excluded_source_folders.contains(*folder))
        .count();
    let destination = destination_folders
        .iter()
        .filter(|folder| {
            expected_destination_folders.contains(*folder) || destination_messages(folder) > 0
        })
        .count();
    (source as u64, destination as u64)
}

pub(crate) fn infer_automap_folder_mapping(
    source: &[MailboxDescriptor],
    destination: &[MailboxDescriptor],
    automap: bool,
) -> Result<HashMap<String, String>, String> {
    if !automap {
        return Ok(HashMap::new());
    }
    let mut mapping = HashMap::new();
    for source_folder in source {
        if !source_folder.selectable {
            continue;
        }
        let kind = source_folder
            .special_use
            .first()
            .map(String::as_str)
            .or_else(|| automap_folder_kind(&source_folder.wire_name));
        let Some(kind) = kind else {
            continue;
        };
        let mut candidates = destination
            .iter()
            .filter(|folder| {
                folder.selectable
                    && (folder.special_use.iter().any(|value| value == kind)
                        || (folder.special_use.is_empty()
                            && automap_folder_kind(&folder.wire_name) == Some(kind)))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
        match candidates.as_slice() {
            [] => {}
            [destination_folder] => {
                mapping.insert(
                    source_folder.wire_name.clone(),
                    destination_folder.wire_name.clone(),
                );
            }
            _ => {
                let names = candidates
                    .iter()
                    .map(|folder| folder.wire_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "ambiguous automap for {kind} folder {}: candidates {names}; resolve the folder mapping before migration",
                    source_folder.wire_name
                ));
            }
        }
    }
    Ok(mapping)
}

pub(crate) fn apply_explicit_folder_mapping(
    mapping: &mut HashMap<String, String>,
    excluded: &mut HashSet<String>,
    rules: &[FolderMappingRule],
) {
    for rule in rules {
        if rule.exclude {
            excluded.insert(rule.source.clone());
        } else {
            mapping.insert(rule.source.clone(), rule.destination.clone());
        }
    }
}

pub(crate) fn automap_folder_kind(folder: &str) -> Option<&'static str> {
    let name = folder
        .rsplit('/')
        .next()
        .unwrap_or(folder)
        .to_ascii_lowercase();
    let kind = match name.as_str() {
        "sent" | "sent mail" | "sent items" | "sent messages" => "sent",
        "trash" | "bin" | "deleted items" | "deleted messages" | "recycle bin" => "trash",
        "junk" | "junk email" | "spam" => "junk",
        "draft" | "drafts" => "drafts",
        "archive" | "all mail" => "archive",
        _ => return None,
    };
    Some(kind)
}
