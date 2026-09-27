use super::*;

#[test]
fn aggregate_verifier_budget_fails_closed_before_reconciliation() {
    assert!(enforce_verifier_state_budget(MAX_ESTIMATED_VERIFIER_STATE_BYTES).is_ok());
    assert!(enforce_verifier_state_budget(MAX_ESTIMATED_VERIFIER_STATE_BYTES + 1).is_err());
}

fn key(uid: &str) -> MailboxMessageKey {
    MailboxMessageKey::new("INBOX", uid)
}

#[test]
fn synthetic_verifier_scale_covers_balanced_mismatch_rates() {
    for &(count, mismatch_percent) in &[
        (10_000_usize, 0_usize),
        (10_000, 10),
        (10_000, 100),
        (100_000, 0),
        (100_000, 10),
        (100_000, 100),
    ] {
        let changed = count * mismatch_percent / 100;
        let mut source = ExtractedMessages::with_capacity(count);
        let mut destination = ExtractedMessages::with_capacity(count);
        for index in 0..count {
            let uid = index.to_string();
            let message_id = format!("<synthetic-{index}@example.test>");
            source.insert(
                MailboxMessageKey::new("INBOX", &uid),
                ExtractedMessage {
                    message_id: Some(message_id.clone()),
                    uid: Some(uid.clone()),
                    size_bytes: Some(1_000),
                    internal_date: Some("2024-01-01T00:00:00Z".to_owned()),
                },
            );
            destination.insert(
                MailboxMessageKey::new("INBOX", &uid),
                ExtractedMessage {
                    message_id: Some(message_id),
                    uid: Some(uid),
                    size_bytes: Some(if index < changed { 1_001 } else { 1_000 }),
                    internal_date: Some("2024-01-01T00:00:00Z".to_owned()),
                },
            );
        }

        let (_, summary) = MessageVerification::detect_mismatches_with_folder_mapping(
            "synthetic-job",
            "synthetic-run",
            &source,
            &destination,
            &HashMap::new(),
        )
        .unwrap();
        assert_eq!(summary.total_source, count as u64);
        assert_eq!(summary.total_destination, count as u64);
        assert_eq!(summary.metadata_matches, (count - changed) as u64);
        assert_eq!(summary.changed_count, changed as u64);
    }
}

#[test]
fn detects_missing_messages() {
    let mut source = HashMap::new();
    source.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(1000),
            internal_date: Some("2024-01-01".to_string()),
        },
    );
    source.insert(
        key("2"),
        ExtractedMessage {
            message_id: Some("<b@example.com>".to_string()),
            uid: Some("2".to_string()),
            size_bytes: Some(2000),
            internal_date: Some("2024-01-02".to_string()),
        },
    );

    let mut dest = HashMap::new();
    dest.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(1000),
            internal_date: Some("2024-01-01".to_string()),
        },
    );

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

    assert_eq!(summary.missing_count, 1);
    assert_eq!(summary.extra_count, 0);
    assert_eq!(summary.metadata_matches, 1);

    let missing = mismatches
        .iter()
        .find(|m| m.mismatch_type == MismatchType::Missing)
        .unwrap();
    assert_eq!(missing.source_uid, Some("2".to_string()));
}

#[test]
fn equivalent_internal_date_offsets_match_semantically() {
    let message = |date: &str| ExtractedMessage {
        message_id: Some("<same@example.com>".into()),
        uid: Some("1".into()),
        size_bytes: Some(100),
        internal_date: Some(date.into()),
    };
    let source = HashMap::from([(key("1"), message("01-Jan-2024 12:00:00 +0000"))]);
    let destination = HashMap::from([(key("1"), message("01-Jan-2024 07:00:00 -0500"))]);

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run1", &source, &destination).unwrap();

    assert!(mismatches.is_empty());
    assert_eq!(summary.metadata_matches, 1);
}

#[test]
fn detects_extra_messages() {
    let mut source = HashMap::new();
    source.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(1000),
            internal_date: Some("2024-01-01".to_string()),
        },
    );

    let mut dest = HashMap::new();
    dest.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(1000),
            internal_date: Some("2024-01-01".to_string()),
        },
    );
    dest.insert(
        key("2"),
        ExtractedMessage {
            message_id: Some("<c@example.com>".to_string()),
            uid: Some("2".to_string()),
            size_bytes: Some(3000),
            internal_date: Some("2024-01-03".to_string()),
        },
    );

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

    assert_eq!(summary.extra_count, 1);
    assert_eq!(summary.missing_count, 0);

    let extra = mismatches
        .iter()
        .find(|m| m.mismatch_type == MismatchType::Extra)
        .unwrap();
    assert_eq!(extra.dest_uid, Some("2".to_string()));
}

#[test]
fn detects_same_metadata_with_different_content_fingerprints() {
    let source_key = key("1");
    let dest_key = key("99");
    let message = ExtractedMessage {
        message_id: Some("<same@example.com>".into()),
        uid: Some("1".into()),
        size_bytes: Some(100),
        internal_date: Some("2024-01-01".into()),
    };
    let mut source = HashMap::new();
    source.insert(source_key.clone(), message.clone());
    let mut destination = HashMap::new();
    destination.insert(
        dest_key.clone(),
        ExtractedMessage {
            uid: Some("99".into()),
            ..message
        },
    );
    let mut source_fingerprints = HashMap::new();
    source_fingerprints.insert(source_key, "aaaa".into());
    let mut destination_fingerprints = HashMap::new();
    destination_fingerprints.insert(dest_key, "bbbb".into());

    let (mismatches, summary) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "job1",
        "run1",
        &source,
        &destination,
        &source_fingerprints,
        &destination_fingerprints,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.changed_count, 1);
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].source_fingerprint.as_deref(), Some("aaaa"));
    assert_eq!(
        mismatches[0].destination_fingerprint.as_deref(),
        Some("bbbb")
    );
}

#[test]
fn content_match_does_not_erase_existing_metadata_mismatch() {
    let source_key = key("1");
    let dest_key = key("99");
    let source = HashMap::from([(
        source_key.clone(),
        ExtractedMessage {
            message_id: Some("<same@example.com>".into()),
            uid: Some("1".into()),
            size_bytes: Some(100),
            internal_date: Some("2024-01-01".into()),
        },
    )]);
    let destination = HashMap::from([(
        dest_key.clone(),
        ExtractedMessage {
            message_id: Some("<same@example.com>".into()),
            uid: Some("99".into()),
            size_bytes: Some(200),
            internal_date: Some("2024-01-01".into()),
        },
    )]);
    let source_fingerprints = HashMap::from([(source_key, "same-body".into())]);
    let destination_fingerprints = HashMap::from([(dest_key, "same-body".into())]);

    let (mismatches, summary) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "job1",
        "run1",
        &source,
        &destination,
        &source_fingerprints,
        &destination_fingerprints,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.changed_count, 1);
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].mismatch_type, MismatchType::MessageIdOnly);
}

#[test]
fn duplicate_message_ids_match_content_as_a_multiset() {
    let source_key_a = key("1");
    let source_key_b = key("2");
    let dest_key_a = key("99");
    let dest_key_b = key("100");
    let message = ExtractedMessage {
        message_id: Some("<duplicate@example.com>".into()),
        uid: None,
        size_bytes: Some(100),
        internal_date: Some("2024-01-01".into()),
    };
    let source = HashMap::from([
        (source_key_a.clone(), message.clone()),
        (source_key_b.clone(), message.clone()),
    ]);
    let destination = HashMap::from([
        (dest_key_a.clone(), message.clone()),
        (dest_key_b.clone(), message),
    ]);
    let source_fingerprints =
        HashMap::from([(source_key_a, "x".into()), (source_key_b, "y".into())]);
    let destination_fingerprints =
        HashMap::from([(dest_key_a, "y".into()), (dest_key_b, "x".into())]);

    let (mismatches, summary) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "job1",
        "run1",
        &source,
        &destination,
        &source_fingerprints,
        &destination_fingerprints,
        &HashMap::new(),
    )
    .unwrap();
    assert!(mismatches.is_empty());
    assert_eq!(summary.metadata_matches, 2);
    assert_eq!(summary.changed_count, 0);
}

#[test]
fn detects_changed_messages() {
    let mut source = HashMap::new();
    source.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(1000),
            internal_date: Some("2024-01-01".to_string()),
        },
    );

    let mut dest = HashMap::new();
    dest.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(2000), // Different size
            internal_date: Some("2024-01-01".to_string()),
        },
    );

    let (_mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

    assert_eq!(summary.changed_count, 1);
    assert!(!summary.is_perfect_metadata_match());
}

#[test]
fn mismatches_preserve_complete_mailbox_local_identity() {
    let source_key = MailboxMessageKey::with_uidvalidity("INBOX", 101, "42");
    let destination_key = MailboxMessageKey::with_uidvalidity("Migrated/INBOX", 909, "7742");
    let source = HashMap::from([(
        source_key,
        ExtractedMessage {
            message_id: Some("<identity@example.com>".to_owned()),
            uid: Some("42".to_owned()),
            size_bytes: Some(1_000),
            internal_date: Some("2024-01-01".to_owned()),
        },
    )]);
    let destination = HashMap::from([(
        destination_key,
        ExtractedMessage {
            message_id: Some("<identity@example.com>".to_owned()),
            uid: Some("7742".to_owned()),
            size_bytes: Some(1_001),
            internal_date: Some("2024-01-02".to_owned()),
        },
    )]);

    let (mismatches, _) = MessageVerification::detect_mismatches_with_folder_mapping(
        "job1",
        "run-local-identity",
        &source,
        &destination,
        &HashMap::from([("INBOX".to_owned(), "Migrated/INBOX".to_owned())]),
    )
    .unwrap();

    assert_eq!(mismatches.len(), 1);
    let mismatch = &mismatches[0];
    assert_eq!(mismatch.source_folder.as_deref(), Some("INBOX"));
    assert_eq!(
        mismatch.destination_folder.as_deref(),
        Some("Migrated/INBOX")
    );
    assert_eq!(mismatch.source_uidvalidity, Some(101));
    assert_eq!(mismatch.destination_uidvalidity, Some(909));
    assert_eq!(mismatch.source_uid.as_deref(), Some("42"));
    assert_eq!(mismatch.dest_uid.as_deref(), Some("7742"));
}

#[test]
fn matching_message_in_wrong_folder_is_not_counted_as_metadata_match() {
    let message = ExtractedMessage {
        message_id: Some("<wrong-folder@example.com>".to_owned()),
        uid: Some("1".to_owned()),
        size_bytes: Some(1_000),
        internal_date: Some("2024-01-01".to_owned()),
    };
    let source = HashMap::from([(MailboxMessageKey::new("INBOX", "1"), message.clone())]);
    let destination = HashMap::from([(MailboxMessageKey::new("WrongFolder", "9"), message)]);

    let (mismatches, summary) = MessageVerification::detect_mismatches_with_folder_mapping(
        "job1",
        "run-wrong-folder",
        &source,
        &destination,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.changed_count, 1);
    assert_eq!(summary.missing_count, 0);
    assert_eq!(summary.extra_count, 0);
    assert_eq!(mismatches.len(), 1);
    assert_eq!(
        mismatches[0].mismatch_type,
        MismatchType::PresentWrongFolder
    );
    assert_eq!(
        MismatchType::PresentWrongFolder.as_str(),
        "message_present_wrong_folder"
    );
}

#[test]
fn wrong_folder_candidate_selection_is_deterministic() {
    let message = ExtractedMessage {
        message_id: Some("<deterministic@example.com>".to_owned()),
        uid: Some("1".to_owned()),
        size_bytes: Some(1_000),
        internal_date: Some("2024-01-01".to_owned()),
    };
    let source = HashMap::from([(MailboxMessageKey::new("INBOX", "1"), message.clone())]);
    let destination = HashMap::from([
        (MailboxMessageKey::new("Z-Folder", "9"), message.clone()),
        (MailboxMessageKey::new("A-Folder", "8"), message),
    ]);

    let (mismatches, _) = MessageVerification::detect_mismatches_with_folder_mapping(
        "job1",
        "run-deterministic-folder",
        &source,
        &destination,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(
        mismatches[0].mismatch_type,
        MismatchType::PresentWrongFolder
    );
    assert_eq!(
        mismatches[0].destination_folder.as_deref(),
        Some("A-Folder")
    );
    assert_eq!(mismatches[0].dest_uid.as_deref(), Some("8"));
}

#[test]
fn message_id_without_metadata_is_not_exact_proof() {
    let message = ExtractedMessage {
        message_id: Some("<metadata-absent@example.com>".to_string()),
        uid: Some("1".to_string()),
        size_bytes: None,
        internal_date: None,
    };
    let source = HashMap::from([(key("1"), message.clone())]);
    let destination = HashMap::from([(key("99"), message)]);

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run-id-only", &source, &destination)
            .unwrap();

    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.changed_count, 1);
    assert!(!mismatches.is_empty());
    assert!(!summary.is_perfect_metadata_match());
}

#[test]
fn one_missing_metadata_field_is_not_treated_as_matching() {
    let source = HashMap::from([(
        key("1"),
        ExtractedMessage {
            message_id: Some("<one-sided@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(100),
            internal_date: None,
        },
    )]);
    let destination = HashMap::from([(
        key("2"),
        ExtractedMessage {
            message_id: Some("<one-sided@example.com>".to_string()),
            uid: Some("2".to_string()),
            size_bytes: Some(100),
            internal_date: None,
        },
    )]);

    let (_, summary) =
        MessageVerification::detect_mismatches("job1", "run-one-sided", &source, &destination)
            .unwrap();

    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.changed_count, 1);
    assert!(!summary.is_perfect_metadata_match());
}

#[test]
fn perfect_match_when_identical() {
    let mut source = HashMap::new();
    source.insert(
        key("1"),
        ExtractedMessage {
            message_id: Some("<a@example.com>".to_string()),
            uid: Some("1".to_string()),
            size_bytes: Some(1000),
            internal_date: Some("2024-01-01".to_string()),
        },
    );

    let dest = source.clone();

    let (_mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run1", &source, &dest).unwrap();

    assert!(summary.is_perfect_metadata_match());
    assert_eq!(summary.metadata_matches, 1);
}

#[test]
fn validation_rejects_duplicate_classification_and_omitted_identity() {
    let empty_message = || ExtractedMessage {
        message_id: None,
        uid: None,
        size_bytes: None,
        internal_date: None,
    };
    let source = HashMap::from([(key("1"), empty_message()), (key("2"), empty_message())]);
    let destination = source.clone();
    let first_source = source.keys().find(|key| key.uid == "1").unwrap();
    let first_destination = destination.keys().find(|key| key.uid == "1").unwrap();
    let membership = VerificationMembership {
        matched_source: HashSet::from([first_source]),
        matched_destination: HashSet::from([first_destination]),
        probable_source: HashSet::from([first_source]),
        probable_destination: HashSet::from([first_destination]),
        ..Default::default()
    };
    let summary = VerificationSummary {
        total_source: 2,
        total_destination: 2,
        metadata_matches: 1,
        probable_matches: 1,
        missing_count: 0,
        extra_count: 0,
        duplicated_count: 0,
        changed_count: 0,
    };

    let error = validate_verification_summary(&source, &destination, &[], &summary, &membership)
        .unwrap_err();
    assert!(error.contains("unclassified") || error.contains("classifications"));

    let second_destination = destination.keys().find(|key| key.uid == "2").unwrap();
    let missing_membership = VerificationMembership {
        matched_source: HashSet::from([first_source]),
        matched_destination: HashSet::from([first_destination, second_destination]),
        ..Default::default()
    };
    let error =
        validate_verification_summary(&source, &destination, &[], &summary, &missing_membership)
            .unwrap_err();
    assert!(error.contains("source message") && error.contains("unclassified"));
}

#[test]
fn destination_uid_rewrite_is_not_reported_as_missing_and_extra() {
    let source = HashMap::from([(
        key("17"),
        ExtractedMessage {
            message_id: Some("<stable@example.com>".to_string()),
            uid: Some("17".to_string()),
            size_bytes: Some(4096),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        },
    )]);
    let destination = HashMap::from([(
        key("904"),
        ExtractedMessage {
            message_id: Some("<stable@example.com>".to_string()),
            uid: Some("904".to_string()),
            size_bytes: Some(4096),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        },
    )]);

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run-uid-rewrite", &source, &destination)
            .unwrap();

    assert!(mismatches.is_empty());
    assert!(summary.is_perfect_metadata_match());
    assert_eq!(summary.metadata_matches, 1);
}

#[test]
fn date_and_size_fallback_is_probable_not_exact() {
    let source = HashMap::from([(
        key("1"),
        ExtractedMessage {
            message_id: None,
            uid: Some("1".to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        },
    )]);
    let destination = HashMap::from([(
        key("88"),
        ExtractedMessage {
            message_id: None,
            uid: Some("88".to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
        },
    )]);

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run-fingerprint", &source, &destination)
            .unwrap();

    assert!(mismatches.is_empty());
    assert!(!summary.is_perfect_metadata_match());
    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.probable_matches, 1);
}

#[test]
fn ambiguous_fingerprint_fails_closed_instead_of_pairing_by_uid() {
    let message = |uid: &str| ExtractedMessage {
        message_id: None,
        uid: Some(uid.to_string()),
        size_bytes: Some(512),
        internal_date: Some("2024-01-01T00:00:00Z".to_string()),
    };
    let source = HashMap::from([(key("1"), message("1")), (key("2"), message("2"))]);
    let destination = HashMap::from([(key("88"), message("88")), (key("89"), message("89"))]);

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run-ambiguous", &source, &destination)
            .unwrap();

    assert_eq!(summary.missing_count, 2);
    assert_eq!(summary.extra_count, 2);
    assert!(!summary.is_perfect_metadata_match());
    assert_eq!(mismatches.len(), 4);
}

#[test]
fn duplicate_message_id_is_reported_without_using_uid_as_identity() {
    let message = |uid: &str| ExtractedMessage {
        message_id: Some("<duplicate@example.com>".to_string()),
        uid: Some(uid.to_string()),
        size_bytes: Some(512),
        internal_date: Some("2024-01-01T00:00:00Z".to_string()),
    };
    let source = HashMap::from([(key("1"), message("1"))]);
    let destination = HashMap::from([(key("88"), message("88")), (key("89"), message("89"))]);

    let (mismatches, summary) =
        MessageVerification::detect_mismatches("job1", "run-duplicate", &source, &destination)
            .unwrap();

    assert_eq!(summary.metadata_matches, 1);
    assert_eq!(summary.extra_count, 0);
    assert!(
        mismatches
            .iter()
            .any(|m| m.mismatch_type == MismatchType::Duplicated)
    );
}

#[test]
fn preserved_source_duplicates_are_not_reported_as_created_duplicates() {
    let message = |uid: &str| ExtractedMessage {
        message_id: Some("<preserved@example.com>".to_string()),
        uid: Some(uid.to_string()),
        size_bytes: Some(512),
        internal_date: Some("2024-01-01T00:00:00Z".to_string()),
    };
    let source = HashMap::from([
        (MailboxMessageKey::new("INBOX", "1"), message("1")),
        (MailboxMessageKey::new("Archive", "1"), message("1")),
    ]);
    let destination = HashMap::from([
        (MailboxMessageKey::new("INBOX", "81"), message("81")),
        (MailboxMessageKey::new("Archive", "92"), message("92")),
    ]);

    let (mismatches, summary) = MessageVerification::detect_mismatches(
        "job1",
        "run-preserved-duplicate",
        &source,
        &destination,
    )
    .unwrap();

    assert!(mismatches.is_empty());
    assert_eq!(summary.metadata_matches, 2);
    assert!(summary.is_perfect_metadata_match());
}

#[test]
fn duplicate_message_id_groups_match_metadata_multisets_not_key_order() {
    let message = |uid: &str, size: u64, date: &str| ExtractedMessage {
        message_id: Some("<same-id@example.com>".to_string()),
        uid: Some(uid.to_string()),
        size_bytes: Some(size),
        internal_date: Some(date.to_string()),
    };
    let source = HashMap::from([
        (key("1"), message("1", 10_000, "2024-01-01")),
        (key("2"), message("2", 30_000, "2024-01-02")),
    ]);
    // Destination key order is deliberately the inverse of the source
    // metadata order: UID 10 contains source B and UID 20 contains A.
    let destination = HashMap::from([
        (key("10"), message("10", 30_000, "2024-01-02")),
        (key("20"), message("20", 10_000, "2024-01-01")),
    ]);

    let (mismatches, summary) = MessageVerification::detect_mismatches(
        "job1",
        "run-reordered-duplicates",
        &source,
        &destination,
    )
    .unwrap();

    assert!(mismatches.is_empty());
    assert_eq!(summary.metadata_matches, 2);
    assert_eq!(summary.changed_count, 0);
    assert!(summary.is_perfect_metadata_match());
}

#[test]
fn duplicate_message_id_groups_report_only_genuine_changed_leftovers() {
    let message = |uid: &str, size: u64, date: &str| ExtractedMessage {
        message_id: Some("<same-id@example.com>".to_string()),
        uid: Some(uid.to_string()),
        size_bytes: Some(size),
        internal_date: Some(date.to_string()),
    };
    let source = HashMap::from([
        (key("1"), message("1", 10_000, "2024-01-01")),
        (key("2"), message("2", 30_000, "2024-01-02")),
    ]);
    let destination = HashMap::from([
        (key("10"), message("10", 30_000, "2024-01-02")),
        (key("20"), message("20", 31_000, "2024-01-03")),
    ]);

    let (mismatches, summary) = MessageVerification::detect_mismatches(
        "job1",
        "run-changed-duplicate",
        &source,
        &destination,
    )
    .unwrap();

    assert_eq!(summary.metadata_matches, 1);
    assert_eq!(summary.changed_count, 1);
    assert_eq!(summary.missing_count, 0);
    assert_eq!(summary.extra_count, 0);
    assert_eq!(summary.duplicated_count, 0);
    assert_eq!(mismatches.len(), 1);
    assert_eq!(mismatches[0].mismatch_type, MismatchType::MessageIdOnly);
    assert!(!summary.is_perfect_metadata_match());
}

#[test]
fn repeated_destination_message_id_without_source_is_extra_not_duplicate() {
    let message = |uid: &str| ExtractedMessage {
        message_id: Some("<destination-only@example.com>".to_string()),
        uid: Some(uid.to_string()),
        size_bytes: Some(512),
        internal_date: Some("2024-01-01T00:00:00Z".to_string()),
    };
    let source = HashMap::new();
    let destination = HashMap::from([
        (MailboxMessageKey::new("INBOX", "81"), message("81")),
        (MailboxMessageKey::new("Archive", "92"), message("92")),
    ]);

    let (mismatches, summary) = MessageVerification::detect_mismatches(
        "job1",
        "run-destination-only",
        &source,
        &destination,
    )
    .unwrap();

    assert_eq!(summary.extra_count, 2);
    assert_eq!(
        mismatches
            .iter()
            .filter(|m| m.mismatch_type == MismatchType::Duplicated)
            .count(),
        0
    );
}

#[test]
fn evidence_level_is_fail_closed_and_named() {
    let summary = |metadata_matches,
                   probable_matches,
                   missing_count,
                   extra_count,
                   duplicated_count,
                   changed_count| VerificationSummary {
        total_source: 1_000,
        total_destination: 1_000,
        metadata_matches,
        probable_matches,
        missing_count,
        extra_count,
        duplicated_count,
        changed_count,
    };

    assert_eq!(
        summary(1_000, 0, 0, 0, 0, 0).evidence_level(),
        EvidenceLevel::MetadataMatched
    );
    assert_eq!(
        summary(1_000, 0, 0, 0, 0, 0).evidence_level().as_str(),
        "metadata_matched"
    );
    assert_eq!(
        summary(950, 0, 0, 0, 0, 0).evidence_level(),
        EvidenceLevel::StrongMetadataMatch
    );
    assert_eq!(
        summary(0, 1, 0, 0, 0, 0).evidence_level(),
        EvidenceLevel::ProbableMatch
    );
    assert_eq!(
        summary(950, 0, 10_000, 0, 0, 0).evidence_level(),
        EvidenceLevel::Missing
    );
    assert_eq!(
        summary(950, 0, 0, 0, 0, 1).evidence_level(),
        EvidenceLevel::Changed
    );
    assert_eq!(
        summary(950, 0, 0, 1, 0, 0).evidence_level(),
        EvidenceLevel::Unexpected
    );
}

#[test]
fn staged_reconciliation_matches_in_memory_accounting() {
    let message = |id: Option<&str>, uid: &str, size: u64, date: &str| ExtractedMessage {
        message_id: id.map(str::to_owned),
        uid: Some(uid.to_owned()),
        size_bytes: Some(size),
        internal_date: Some(date.to_owned()),
    };
    let source = ExtractedMessages::from([
        (
            MailboxMessageKey::new("INBOX", "1"),
            message(Some("<a>"), "1", 10, "01-Jan-2024 00:00:00 +0000"),
        ),
        (
            MailboxMessageKey::new("INBOX", "2"),
            message(Some("<b>"), "2", 20, "01-Jan-2024 00:00:00 +0000"),
        ),
        (
            MailboxMessageKey::new("INBOX", "3"),
            message(None, "3", 30, "01-Jan-2024 00:00:00 +0000"),
        ),
        (
            MailboxMessageKey::new("INBOX", "4"),
            message(Some("<missing>"), "4", 40, "01-Jan-2024 00:00:00 +0000"),
        ),
    ]);
    let destination = ExtractedMessages::from([
        (
            MailboxMessageKey::new("INBOX", "11"),
            message(Some("<a>"), "11", 10, "01-Jan-2024 00:00:00 +0000"),
        ),
        (
            MailboxMessageKey::new("INBOX", "12"),
            message(Some("<b>"), "12", 21, "01-Jan-2024 00:00:00 +0000"),
        ),
        (
            MailboxMessageKey::new("INBOX", "13"),
            message(None, "13", 30, "01-Jan-2024 00:00:00 +0000"),
        ),
        (
            MailboxMessageKey::new("INBOX", "14"),
            message(Some("<extra>"), "14", 50, "01-Jan-2024 00:00:00 +0000"),
        ),
    ]);
    let folder_mapping = HashMap::new();
    let (expected_mismatches, expected_summary) = MessageVerification::detect_mismatches(
        "job-staged",
        "run-staged-map",
        &source,
        &destination,
    )
    .unwrap();
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    stage
        .insert_messages(StagedMessageSide::Source, &source)
        .unwrap();
    stage
        .insert_messages(StagedMessageSide::Destination, &destination)
        .unwrap();
    let (staged_mismatches, staged_summary) = MessageVerification::detect_mismatches_from_stage(
        "job-staged",
        "run-staged-stage",
        &stage,
        &folder_mapping,
    )
    .unwrap();
    assert_eq!(staged_summary.total_source, expected_summary.total_source);
    assert_eq!(
        staged_summary.total_destination,
        expected_summary.total_destination
    );
    assert_eq!(
        staged_summary.metadata_matches,
        expected_summary.metadata_matches
    );
    assert_eq!(
        staged_summary.probable_matches,
        expected_summary.probable_matches
    );
    assert_eq!(staged_summary.missing_count, expected_summary.missing_count);
    assert_eq!(staged_summary.extra_count, expected_summary.extra_count);
    assert_eq!(
        staged_summary.duplicated_count,
        expected_summary.duplicated_count
    );
    assert_eq!(staged_summary.changed_count, expected_summary.changed_count);
    let mut expected_types = expected_mismatches
        .iter()
        .map(|mismatch| mismatch.mismatch_type.clone())
        .collect::<Vec<_>>();
    let mut staged_types = staged_mismatches
        .iter()
        .map(|mismatch| mismatch.mismatch_type.clone())
        .collect::<Vec<_>>();
    expected_types.sort_by_key(MismatchType::as_str);
    staged_types.sort_by_key(MismatchType::as_str);
    assert_eq!(staged_types, expected_types);
}

#[test]
fn staged_fingerprint_lookups_seek_on_mapped_folder_date_and_size() {
    let stage = MessageMetadataStage::open_in_memory().unwrap();
    let mut statement = stage
        .connection()
        .prepare(
            "EXPLAIN QUERY PLAN SELECT COUNT(*) FROM staged_messages WHERE side=?1 AND match_mailbox=?2 AND date_key=?3 AND size_bytes=?4",
        )
        .unwrap();
    let plan = statement
        .query_map(
            rusqlite::params![
                StagedMessageSide::Source.as_i64(),
                "INBOX",
                "2024-01-01T00:00:00Z",
                4096
            ],
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join(" ");
    assert!(
        plan.contains("staged_messages_match_metadata"),
        "fallback lookup did not use the folder/date/size index: {plan}"
    );
}

#[test]
fn staged_reconciliation_matches_duplicate_and_mapping_cases() {
    let message = |id: Option<&str>, uid: &str, size: u64, date: &str| ExtractedMessage {
        message_id: id.map(str::to_owned),
        uid: Some(uid.to_owned()),
        size_bytes: Some(size),
        internal_date: Some(date.to_owned()),
    };
    let cases = [
        (
            ExtractedMessages::from([
                (
                    MailboxMessageKey::new("INBOX", "1"),
                    message(Some("<duplicate>"), "1", 10, "01-Jan-2024 00:00:00 +0000"),
                ),
                (
                    MailboxMessageKey::new("INBOX", "2"),
                    message(Some("<duplicate>"), "2", 20, "01-Jan-2024 00:00:00 +0000"),
                ),
            ]),
            ExtractedMessages::from([
                (
                    MailboxMessageKey::new("Migrated", "9"),
                    message(Some("<duplicate>"), "9", 10, "01-Jan-2024 00:00:00 +0000"),
                ),
                (
                    MailboxMessageKey::new("Migrated", "10"),
                    message(Some("<duplicate>"), "10", 20, "01-Jan-2024 00:00:00 +0000"),
                ),
                (
                    MailboxMessageKey::new("Migrated", "11"),
                    message(Some("<duplicate>"), "11", 20, "01-Jan-2024 00:00:00 +0000"),
                ),
            ]),
            HashMap::from([(String::from("INBOX"), String::from("Migrated"))]),
        ),
        (
            ExtractedMessages::from([
                (
                    MailboxMessageKey::new("INBOX", "1"),
                    message(
                        Some("<wrong-folder>"),
                        "1",
                        30,
                        "01-Jan-2024 00:00:00 +0000",
                    ),
                ),
                (
                    MailboxMessageKey::new("Sent", "2"),
                    message(Some("<probable>"), "2", 40, "02-Jan-2024 00:00:00 +0000"),
                ),
            ]),
            ExtractedMessages::from([
                (
                    MailboxMessageKey::new("Archive", "7"),
                    message(
                        Some("<wrong-folder>"),
                        "7",
                        30,
                        "01-Jan-2024 00:00:00 +0000",
                    ),
                ),
                (
                    MailboxMessageKey::new("Sent", "8"),
                    message(None, "8", 40, "02-Jan-2024 00:00:00 +0000"),
                ),
            ]),
            HashMap::new(),
        ),
        (
            ExtractedMessages::from([(
                MailboxMessageKey::new("INBOX", "1"),
                message(Some("<changed>"), "1", 50, "03-Jan-2024 00:00:00 +0000"),
            )]),
            ExtractedMessages::from([(
                MailboxMessageKey::new("INBOX", "9"),
                message(Some("<changed>"), "9", 51, "03-Jan-2024 00:00:00 +0000"),
            )]),
            HashMap::new(),
        ),
        (
            ExtractedMessages::from([(
                MailboxMessageKey::new("M", "1"),
                message(
                    Some("<multiple-lower>"),
                    "1",
                    60,
                    "04-Jan-2024 00:00:00 +0000",
                ),
            )]),
            ExtractedMessages::from([
                (
                    MailboxMessageKey::new("A", "7"),
                    message(
                        Some("<multiple-lower>"),
                        "7",
                        60,
                        "04-Jan-2024 00:00:00 +0000",
                    ),
                ),
                (
                    MailboxMessageKey::new("B", "8"),
                    message(
                        Some("<multiple-lower>"),
                        "8",
                        60,
                        "04-Jan-2024 00:00:00 +0000",
                    ),
                ),
                (
                    MailboxMessageKey::new("Z", "9"),
                    message(
                        Some("<multiple-lower>"),
                        "9",
                        60,
                        "04-Jan-2024 00:00:00 +0000",
                    ),
                ),
            ]),
            HashMap::new(),
        ),
    ];

    for (index, (source, destination, folder_mapping)) in cases.into_iter().enumerate() {
        let (expected_mismatches, expected_summary) =
            MessageVerification::detect_mismatches_with_folder_mapping(
                "job-staged-cases",
                &format!("run-map-{index}"),
                &source,
                &destination,
                &folder_mapping,
            )
            .unwrap();
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage
            .insert_messages(StagedMessageSide::Source, &source)
            .unwrap();
        stage
            .insert_messages(StagedMessageSide::Destination, &destination)
            .unwrap();
        let (staged_mismatches, staged_summary) =
            MessageVerification::detect_mismatches_from_stage(
                "job-staged-cases",
                &format!("run-stage-{index}"),
                &stage,
                &folder_mapping,
            )
            .unwrap();
        assert_eq!(staged_summary, expected_summary, "case {index}");
        let mut expected = expected_mismatches
            .iter()
            .map(|mismatch| {
                (
                    mismatch.mismatch_type.clone(),
                    mismatch.source_folder.clone(),
                    mismatch.source_uid.clone(),
                    mismatch.destination_folder.clone(),
                    mismatch.dest_uid.clone(),
                )
            })
            .collect::<Vec<_>>();
        let mut staged = staged_mismatches
            .iter()
            .map(|mismatch| {
                (
                    mismatch.mismatch_type.clone(),
                    mismatch.source_folder.clone(),
                    mismatch.source_uid.clone(),
                    mismatch.destination_folder.clone(),
                    mismatch.dest_uid.clone(),
                )
            })
            .collect::<Vec<_>>();
        expected.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
        staged.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
        assert_eq!(staged, expected, "case {index}");
        if index == 3 {
            let selected = staged_mismatches
                .iter()
                .find(|mismatch| mismatch.source_uid.as_deref() == Some("1"))
                .expect("multiple-candidate case should classify its source message");
            assert_eq!(selected.destination_folder.as_deref(), Some("B"));
        }
    }
}

#[test]
fn generated_staged_reconciliation_matches_every_semantic_mismatch_field() {
    fn next(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        *state
    }

    fn signatures(mismatches: &[MessageMismatch]) -> Vec<String> {
        let mut result = mismatches
            .iter()
            .map(|mismatch| {
                format!(
                    "{:?}|{:?}|{:?}",
                    &mismatch.mismatch_type,
                    (
                        &mismatch.source_folder,
                        mismatch.source_uidvalidity,
                        &mismatch.source_uid,
                        &mismatch.source_message_id,
                        mismatch.source_size_bytes,
                        &mismatch.source_date,
                        &mismatch.source_fingerprint,
                    ),
                    (
                        &mismatch.destination_folder,
                        mismatch.destination_uidvalidity,
                        &mismatch.dest_uid,
                        &mismatch.dest_message_id,
                        mismatch.dest_size_bytes,
                        &mismatch.dest_date,
                        &mismatch.destination_fingerprint,
                    )
                )
            })
            .collect::<Vec<_>>();
        result.sort();
        result
    }

    let folders = ["INBOX", "Sent", "Archive", "A", "B", "M", "Z"];
    let dates = [
        "01-Jan-2024 00:00:00 +0000",
        "02-Jan-2024 00:00:00 +0000",
        "03-Jan-2024 00:00:00 +0000",
    ];
    let folder_mapping = HashMap::from([(String::from("INBOX"), String::from("Archive"))]);
    let mut state = 0x5354_4147_4544_u64;

    for case in 0..48 {
        let mut source = ExtractedMessages::new();
        let mut destination = ExtractedMessages::new();
        for index in 0..10 {
            let seed = next(&mut state);
            let source_folder = folders[(seed as usize) % folders.len()];
            let source_uid = format!("s-{case}-{index}");
            let message_id = (!seed.is_multiple_of(5)).then(|| format!("<id-{}>", (seed >> 8) % 5));
            let size = Some(100 + ((seed >> 16) % 4));
            let date = Some(dates[((seed >> 24) as usize) % dates.len()].to_owned());
            source.insert(
                MailboxMessageKey::new(source_folder, &source_uid),
                ExtractedMessage {
                    message_id: message_id.clone(),
                    uid: Some(source_uid),
                    size_bytes: size,
                    internal_date: date.clone(),
                },
            );

            if seed.is_multiple_of(7) {
                continue;
            }
            let destination_seed = next(&mut state);
            let expected_folder = if source_folder == "INBOX" {
                "Archive"
            } else {
                source_folder
            };
            let destination_folder = if destination_seed.is_multiple_of(3) {
                expected_folder
            } else {
                folders[((destination_seed >> 8) as usize) % folders.len()]
            };
            let destination_uid = format!("d-{case}-{index}");
            destination.insert(
                MailboxMessageKey::new(destination_folder, &destination_uid),
                ExtractedMessage {
                    message_id: if destination_seed.is_multiple_of(6) {
                        Some(format!("<other-{}>", destination_seed % 4))
                    } else {
                        message_id
                    },
                    uid: Some(destination_uid),
                    size_bytes: Some(100 + ((destination_seed >> 16) % 4)),
                    internal_date: Some(
                        dates[((destination_seed >> 24) as usize) % dates.len()].to_owned(),
                    ),
                },
            );
        }

        let (reference_mismatches, reference_summary) =
            MessageVerification::detect_mismatches_with_folder_mapping(
                "generated-parity",
                &format!("reference-{case}"),
                &source,
                &destination,
                &folder_mapping,
            )
            .unwrap();
        let mut stage = MessageMetadataStage::open_in_memory().unwrap();
        stage
            .insert_messages(StagedMessageSide::Source, &source)
            .unwrap();
        stage
            .insert_messages(StagedMessageSide::Destination, &destination)
            .unwrap();
        let (staged_mismatches, staged_summary) =
            MessageVerification::detect_mismatches_from_stage(
                "generated-parity",
                &format!("staged-{case}"),
                &stage,
                &folder_mapping,
            )
            .unwrap();

        assert_eq!(staged_summary, reference_summary, "case {case}");
        assert_eq!(
            signatures(&staged_mismatches),
            signatures(&reference_mismatches),
            "semantic mismatch records differed in generated case {case}"
        );
    }
}
