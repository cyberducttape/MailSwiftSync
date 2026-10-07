//! Flag and keyword fidelity: a separate pass over the stage that compares
//! the IMAP FLAGS of messages whose identity is unambiguous.

use super::*;
use crate::core::FlagVerification;

/// Classification of one source/destination flag comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlagComparison {
    Match,
    /// Flags differ only by flags the destination folder's PERMANENTFLAGS
    /// say it cannot store, which is a documented provider limitation.
    Excepted,
    Mismatch,
}

/// Compare two canonical flag sets (see `canonical_flag_set`). A difference
/// is excepted only when the destination lost flags, gained none, and its
/// SELECT advertised PERMANENTFLAGS that cannot store any of the lost ones.
/// Without PERMANENTFLAGS a client must assume every flag is storable, so a
/// difference is a mismatch.
pub(crate) fn compare_flag_sets(
    source: &str,
    destination: &str,
    destination_permanent_flags: Option<&str>,
) -> FlagComparison {
    if source == destination {
        return FlagComparison::Match;
    }
    let source = source.split_whitespace().collect::<HashSet<_>>();
    let destination = destination.split_whitespace().collect::<HashSet<_>>();
    if source == destination {
        return FlagComparison::Match;
    }
    if !destination.is_subset(&source) {
        return FlagComparison::Mismatch;
    }
    let Some(permanent) = destination_permanent_flags else {
        return FlagComparison::Mismatch;
    };
    let permanent = permanent.split_whitespace().collect::<HashSet<_>>();
    // `\*` lets a client create new keywords; system flags must be listed.
    let storable = |flag: &str| {
        permanent.contains(flag) || (!flag.starts_with('\\') && permanent.contains("\\*"))
    };
    if source.difference(&destination).any(|flag| storable(flag)) {
        FlagComparison::Mismatch
    } else {
        FlagComparison::Excepted
    }
}

impl MessageVerification {
    /// Compare FLAGS for every message pair whose identity is unambiguous:
    /// exactly one source and one destination message share the Message-ID,
    /// expected destination folder, INTERNALDATE, and RFC822.SIZE. Duplicates
    /// and probable pairings are left uncompared rather than guessed, so the
    /// compared count is the flag-verification coverage. The pass leaves the
    /// stage unchanged and can be rerun.
    pub(crate) fn verify_staged_flags(
        stage: &MessageMetadataStage,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<FlagVerification, String> {
        let connection = stage.connection()?;
        // A private stage table, not TEMP, for the same reason as staged
        // reconciliation: TEMP tables can spill outside the stage lifecycle.
        // Ephemeral stages run with journal_mode=OFF, where a rollback cannot
        // undo DDL, so the table is dropped explicitly instead; dropping it
        // first also clears one left by an interrupted durable pass.
        crate::core::stage_sql::execute_batch(connection,
                "DROP TABLE IF EXISTS staged_flag_folder_mapping;
                 CREATE TABLE staged_flag_folder_mapping(source TEXT PRIMARY KEY, destination TEXT NOT NULL);",
            )
            .map_err(|error| format!("could not initialize flag verification: {error}"))?;
        for (source, destination) in folder_mapping {
            crate::core::stage_sql::execute(
                connection,
                "INSERT INTO staged_flag_folder_mapping(source,destination) VALUES(?1,?2)",
                params![source, destination],
            )
            .map_err(|error| format!("could not stage folder mapping: {error}"))?;
        }
        let result = Self::compare_staged_flag_pairs(connection);
        crate::core::stage_sql::execute_batch(connection, "DROP TABLE staged_flag_folder_mapping;")
            .map_err(|error| format!("could not release flag verification rows: {error}"))?;
        result
    }

    fn compare_staged_flag_pairs(
        connection: &rusqlite::Connection,
    ) -> Result<FlagVerification, String> {
        let mut statement = crate::core::stage_sql::prepare(
            connection,
            "WITH source AS (
                    SELECT s.message_id,COALESCE(m.destination,s.mailbox) AS expected_mailbox,
                           s.date_key,s.size_bytes,MIN(s.flags) AS flags
                    FROM staged_messages s
                    LEFT JOIN staged_flag_folder_mapping m ON m.source=s.mailbox
                    WHERE s.side=0 AND s.message_id IS NOT NULL
                      AND s.date_key IS NOT NULL AND s.size_bytes IS NOT NULL
                    GROUP BY s.message_id,expected_mailbox,s.date_key,s.size_bytes
                    HAVING COUNT(*)=1
                 ), destination AS (
                    SELECT message_id,mailbox,date_key,size_bytes,MIN(flags) AS flags
                    FROM staged_messages
                    WHERE side=1 AND message_id IS NOT NULL
                      AND date_key IS NOT NULL AND size_bytes IS NOT NULL
                    GROUP BY message_id,mailbox,date_key,size_bytes
                    HAVING COUNT(*)=1
                 )
                 SELECT s.flags,d.flags,p.flags
                 FROM source s
                 JOIN destination d
                   ON d.message_id=s.message_id AND d.mailbox=s.expected_mailbox
                  AND d.date_key=s.date_key AND d.size_bytes=s.size_bytes
                 LEFT JOIN stage_permanent_flags p ON p.side=1 AND p.mailbox=d.mailbox
                 WHERE s.flags IS NOT NULL AND d.flags IS NOT NULL",
        )
        .map_err(|error| format!("could not prepare flag verification: {error}"))?;
        let mut rows = statement
            .query([])
            .map_err(|error| format!("could not query flag verification pairs: {error}"))?;
        let mut result = FlagVerification::default();
        while let Some(row) = rows
            .next()
            .map_err(|error| format!("could not read flag verification pair: {error}"))?
        {
            let source: String = row.get(0).map_err(|error| error.to_string())?;
            let destination: String = row.get(1).map_err(|error| error.to_string())?;
            let permanent: Option<String> = row.get(2).map_err(|error| error.to_string())?;
            result.compared_messages = result.compared_messages.saturating_add(1);
            match compare_flag_sets(&source, &destination, permanent.as_deref()) {
                FlagComparison::Match => {}
                FlagComparison::Excepted => {
                    result.excepted_messages = result.excepted_messages.saturating_add(1);
                }
                FlagComparison::Mismatch => {
                    result.mismatched_messages = result.mismatched_messages.saturating_add(1);
                }
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, flags: &str) -> ExtractedMessage {
        ExtractedMessage {
            message_id: Some(id.to_owned()),
            uid: None,
            size_bytes: Some(10),
            internal_date: Some("01-Jan-2024 00:00:00 +0000".to_owned()),
            flags: Some(flags.to_owned()),
        }
    }

    fn stage_side(
        stage: &mut MessageMetadataStage,
        side: StagedMessageSide,
        rows: &[(&str, u64, &str, &str)],
    ) {
        let messages = rows
            .iter()
            .map(|(mailbox, uid, id, flags)| {
                (
                    MailboxMessageKey::with_uidvalidity(*mailbox, 1, uid.to_string()),
                    message(id, flags),
                )
            })
            .collect::<ExtractedMessages>();
        stage.insert_messages(side, &messages).unwrap();
    }

    #[test]
    fn folder_mapping_pairs_messages_in_their_expected_destination_folder() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage_side(
            &mut stage,
            StagedMessageSide::Source,
            &[("Sent Items", 1, "<a@x>", "\\Seen")],
        );
        stage_side(
            &mut stage,
            StagedMessageSide::Destination,
            &[("Sent", 4, "<a@x>", "")],
        );
        let unmapped = MessageVerification::verify_staged_flags(&stage, &HashMap::new()).unwrap();
        assert_eq!(unmapped.compared_messages, 0);
        let mapping = HashMap::from([("Sent Items".to_owned(), "Sent".to_owned())]);
        let mapped = MessageVerification::verify_staged_flags(&stage, &mapping).unwrap();
        assert_eq!(mapped.compared_messages, 1);
        assert_eq!(mapped.mismatched_messages, 1);
    }

    #[test]
    fn duplicate_identities_are_not_compared_and_the_stage_is_unchanged() {
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage_side(
            &mut stage,
            StagedMessageSide::Source,
            &[
                ("INBOX", 1, "<dup@x>", "\\Seen"),
                ("INBOX", 2, "<dup@x>", ""),
                ("INBOX", 3, "<one@x>", "\\Flagged"),
            ],
        );
        stage_side(
            &mut stage,
            StagedMessageSide::Destination,
            &[
                ("INBOX", 1, "<dup@x>", ""),
                ("INBOX", 2, "<dup@x>", "\\Seen"),
                ("INBOX", 3, "<one@x>", "\\Flagged"),
            ],
        );
        let result = MessageVerification::verify_staged_flags(&stage, &HashMap::new()).unwrap();
        assert_eq!(
            result,
            FlagVerification {
                compared_messages: 1,
                mismatched_messages: 0,
                excepted_messages: 0,
            }
        );
        // The scratch mapping table was rolled back, so the pass can rerun.
        assert_eq!(
            MessageVerification::verify_staged_flags(&stage, &HashMap::new()).unwrap(),
            result
        );
    }

    #[test]
    fn flag_set_comparison_policy() {
        use FlagComparison::*;
        assert_eq!(compare_flag_sets("\\Seen", "\\Seen", None), Match);
        assert_eq!(compare_flag_sets("\\Seen", "", None), Mismatch);
        assert_eq!(compare_flag_sets("", "\\Seen", Some("")), Mismatch);
        assert_eq!(
            compare_flag_sets("\\Seen work", "\\Seen", Some("\\Seen")),
            Excepted
        );
        assert_eq!(
            compare_flag_sets("\\Seen work", "\\Seen", Some("\\Seen \\*")),
            Mismatch
        );
        assert_eq!(
            compare_flag_sets("\\Draft", "", Some("\\Seen \\*")),
            Excepted
        );
    }
}
