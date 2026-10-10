use super::*;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

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
                    flags: None,
                },
            );
            destination.insert(
                MailboxMessageKey::new("INBOX", &uid),
                ExtractedMessage {
                    message_id: Some(message_id),
                    uid: Some(uid),
                    size_bytes: Some(if index < changed { 1_001 } else { 1_000 }),
                    internal_date: Some("2024-01-01T00:00:00Z".to_owned()),
                    flags: None,
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
            flags: None,
        },
    );
    source.insert(
        key("2"),
        ExtractedMessage {
            message_id: Some("<b@example.com>".to_string()),
            uid: Some("2".to_string()),
            size_bytes: Some(2000),
            internal_date: Some("2024-01-02".to_string()),
            flags: None,
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
            flags: None,
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
        flags: None,
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
            flags: None,
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
            flags: None,
        },
    );
    dest.insert(
        key("2"),
        ExtractedMessage {
            message_id: Some("<c@example.com>".to_string()),
            uid: Some("2".to_string()),
            size_bytes: Some(3000),
            internal_date: Some("2024-01-03".to_string()),
            flags: None,
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
        flags: None,
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
fn content_verification_rejects_equal_count_fingerprint_key_substitution() {
    let source_key = key("source");
    let destination_key = key("destination");
    let orphan_key = key("orphan");
    let message = ExtractedMessage {
        message_id: Some("<same@example.test>".into()),
        uid: Some("1".into()),
        size_bytes: Some(123),
        internal_date: Some("2025-01-01".into()),
        flags: None,
    };
    let source = HashMap::from([(source_key.clone(), message.clone())]);
    let destination = HashMap::from([(destination_key.clone(), message)]);

    for orphan_source in [true, false] {
        let source_fingerprints = HashMap::from([(
            if orphan_source {
                orphan_key.clone()
            } else {
                source_key.clone()
            },
            "source-body-hash".to_owned(),
        )]);
        let destination_fingerprints = HashMap::from([(
            if orphan_source {
                destination_key.clone()
            } else {
                orphan_key.clone()
            },
            "destination-body-hash".to_owned(),
        )]);
        assert_eq!(source_fingerprints.len(), source.len());
        assert_eq!(destination_fingerprints.len(), destination.len());

        let error = MessageVerification::detect_mismatches_with_content_fingerprints(
            "job",
            "run",
            &source,
            &destination,
            &source_fingerprints,
            &destination_fingerprints,
            &HashMap::new(),
        )
        .unwrap_err();
        let expected_side = if orphan_source {
            "source"
        } else {
            "destination"
        };
        assert!(error.contains(expected_side), "{error}");
        assert!(error.contains("exact"), "{error}");
    }
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
            flags: None,
        },
    )]);
    let destination = HashMap::from([(
        dest_key.clone(),
        ExtractedMessage {
            message_id: Some("<same@example.com>".into()),
            uid: Some("99".into()),
            size_bytes: Some(200),
            internal_date: Some("2024-01-01".into()),
            flags: None,
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
        flags: None,
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

fn message(message_id: Option<&str>, uid: &str, size: u64, date: &str) -> ExtractedMessage {
    ExtractedMessage {
        message_id: message_id.map(str::to_owned),
        uid: Some(uid.into()),
        size_bytes: Some(size),
        internal_date: Some(date.into()),
        flags: None,
    }
}

#[test]
fn wrong_folder_message_with_different_body_is_classified_once() {
    let source_key = MailboxMessageKey::new("INBOX", "1");
    let dest_key = MailboxMessageKey::new("WrongFolder", "7");
    let source = HashMap::from([(
        source_key.clone(),
        message(Some("<123@example.com>"), "1", 100, "2024-01-01"),
    )]);
    let destination = HashMap::from([(
        dest_key.clone(),
        message(Some("<123@example.com>"), "7", 100, "2024-01-01"),
    )]);
    let (mismatches, summary) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "job1",
        "run1",
        &source,
        &destination,
        &HashMap::from([(source_key, "AAA".into())]),
        &HashMap::from([(dest_key, "BBB".into())]),
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(mismatches.len(), 1);
    assert_eq!(
        mismatches[0].mismatch_type,
        MismatchType::PresentWrongFolder
    );
    assert_eq!(summary.changed_count, 1);
    assert_eq!(summary.metadata_matches, 0);
}

#[test]
fn fingerprint_resolution_does_not_consume_probable_matches() {
    // Pair A has no Message-ID and differing metadata, so metadata
    // reconciliation reports Missing + Extra; a unique body fingerprint in the
    // expected folder resolves it. Pair B is an unrelated date/size probable
    // match whose count must be unaffected.
    let source_a = MailboxMessageKey::new("INBOX", "1");
    let dest_a = MailboxMessageKey::new("INBOX", "11");
    let source_b = MailboxMessageKey::new("INBOX", "2");
    let dest_b = MailboxMessageKey::new("INBOX", "12");
    let source = HashMap::from([
        (source_a.clone(), message(None, "1", 100, "2024-01-01")),
        (source_b.clone(), message(None, "2", 300, "2024-03-03")),
    ]);
    let destination = HashMap::from([
        (dest_a.clone(), message(None, "11", 200, "2024-02-02")),
        (dest_b.clone(), message(None, "12", 300, "2024-03-03")),
    ]);
    let (mismatches, summary) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "job1",
        "run1",
        &source,
        &destination,
        &HashMap::from([(source_a, "same".into()), (source_b, "b-src".into())]),
        &HashMap::from([(dest_a, "same".into()), (dest_b, "b-dst".into())]),
        &HashMap::new(),
    )
    .unwrap();
    assert!(mismatches.is_empty(), "{mismatches:?}");
    assert_eq!(summary.metadata_matches, 1);
    assert_eq!(summary.probable_matches, 1);
    assert_eq!(summary.missing_count, 0);
    assert_eq!(summary.extra_count, 0);
}

#[test]
fn fingerprint_resolution_respects_the_expected_folder() {
    let source_key = MailboxMessageKey::new("INBOX", "1");
    let dest_key = MailboxMessageKey::new("Archive", "11");
    let source = HashMap::from([(source_key.clone(), message(None, "1", 100, "2024-01-01"))]);
    let destination = HashMap::from([(dest_key.clone(), message(None, "11", 200, "2024-02-02"))]);
    let (_, summary) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "job1",
        "run1",
        &source,
        &destination,
        &HashMap::from([(source_key, "same".into())]),
        &HashMap::from([(dest_key, "same".into())]),
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(summary.metadata_matches, 0);
    assert_eq!(summary.missing_count, 1);
    assert_eq!(summary.extra_count, 1);
}

#[test]
fn summary_counters_must_equal_their_classification() {
    let source = HashMap::from([(key("1"), message(Some("<a@x>"), "1", 1, "2024-01-01"))]);
    let destination = HashMap::from([(key("1"), message(Some("<a@x>"), "1", 1, "2024-01-01"))]);
    let membership = VerificationMembership {
        matched_source: source.keys().collect(),
        matched_destination: destination.keys().collect(),
        ..Default::default()
    };
    let mut summary = VerificationSummary {
        total_source: 1,
        total_destination: 1,
        metadata_matches: 1,
        probable_matches: 0,
        missing_count: 0,
        extra_count: 0,
        duplicated_count: 0,
        changed_count: 0,
    };
    validate_verification_summary(&source, &destination, &[], &summary, &membership).unwrap();
    summary.changed_count = 1;
    let error = validate_verification_summary(&source, &destination, &[], &summary, &membership)
        .unwrap_err();
    assert!(error.contains("changed_count"), "{error}");
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
            flags: None,
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
            flags: None,
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
            flags: None,
        },
    )]);
    let destination = HashMap::from([(
        destination_key,
        ExtractedMessage {
            message_id: Some("<identity@example.com>".to_owned()),
            uid: Some("7742".to_owned()),
            size_bytes: Some(1_001),
            internal_date: Some("2024-01-02".to_owned()),
            flags: None,
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
        flags: None,
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
        flags: None,
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
        flags: None,
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
            flags: None,
        },
    )]);
    let destination = HashMap::from([(
        key("2"),
        ExtractedMessage {
            message_id: Some("<one-sided@example.com>".to_string()),
            uid: Some("2".to_string()),
            size_bytes: Some(100),
            internal_date: None,
            flags: None,
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
            flags: None,
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
        flags: None,
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
            flags: None,
        },
    )]);
    let destination = HashMap::from([(
        key("904"),
        ExtractedMessage {
            message_id: Some("<stable@example.com>".to_string()),
            uid: Some("904".to_string()),
            size_bytes: Some(4096),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            flags: None,
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
            flags: None,
        },
    )]);
    let destination = HashMap::from([(
        key("88"),
        ExtractedMessage {
            message_id: None,
            uid: Some("88".to_string()),
            size_bytes: Some(512),
            internal_date: Some("2024-01-01T00:00:00Z".to_string()),
            flags: None,
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
        flags: None,
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
        flags: None,
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
        flags: None,
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
        flags: None,
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
        flags: None,
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
        flags: None,
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
        flags: None,
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
        .map(|mismatch| mismatch.mismatch_type)
        .collect::<Vec<_>>();
    let mut staged_types = staged_mismatches
        .iter()
        .map(|mismatch| mismatch.mismatch_type)
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
        .unwrap()
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
fn staged_exact_pair_ranking_uses_compound_side_indexes() {
    let stage = MessageMetadataStage::open_in_memory().unwrap();
    for (sql, expected_index) in [
        (
            "EXPLAIN QUERY PLAN SELECT rowid,message_id,match_mailbox,date_key,size_bytes FROM staged_messages INDEXED BY staged_messages_exact_source WHERE side=0 AND message_id IS NOT NULL AND date_key IS NOT NULL AND size_bytes IS NOT NULL ORDER BY message_id,match_mailbox,date_key,size_bytes,rowid",
            "staged_messages_exact_source",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT rowid,message_id,mailbox,date_key,size_bytes,uidvalidity,uid FROM staged_messages INDEXED BY staged_messages_exact_destination WHERE side=1 AND message_id IS NOT NULL AND date_key IS NOT NULL AND size_bytes IS NOT NULL ORDER BY message_id,mailbox,date_key,size_bytes,uidvalidity,uid",
            "staged_messages_exact_destination",
        ),
    ] {
        let plan = stage
            .connection()
            .unwrap()
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
            .join(" ");
        assert!(
            plan.contains(expected_index),
            "exact-pair ranking did not use {expected_index}: {plan}"
        );
    }
}

#[test]
fn staged_reconciliation_matches_duplicate_and_mapping_cases() {
    let message = |id: Option<&str>, uid: &str, size: u64, date: &str| ExtractedMessage {
        message_id: id.map(str::to_owned),
        uid: Some(uid.to_owned()),
        size_bytes: Some(size),
        internal_date: Some(date.to_owned()),
        flags: None,
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
                    mismatch.mismatch_type,
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
                    mismatch.mismatch_type,
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
                    flags: None,
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
                    flags: None,
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

/// Input shape for the durable-stage benchmark
/// (`MAILSWIFTSYNC_RECONCILIATION_BENCH_SHAPE`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BenchShape {
    /// Unique Message-IDs; 2% of destination sizes differ.
    Typical,
    /// 20% of messages share a Message-ID in groups of five and 1% share one
    /// "hot" Message-ID; 2% of destination sizes differ.
    DuplicateIds,
    /// Half the messages have no Message-ID; a tenth of those also share one
    /// date/size bucket, so they cannot be paired at all.
    AbsentIds,
    /// 30% changed sizes, 10% missing, 10% extra, and 10% in another folder.
    MismatchHeavy,
}

impl BenchShape {
    fn parse(value: &str) -> Self {
        match value {
            "typical" => Self::Typical,
            "duplicate_ids" => Self::DuplicateIds,
            "absent_ids" => Self::AbsentIds,
            "mismatch_heavy" => Self::MismatchHeavy,
            other => panic!("unknown benchmark shape {other:?}"),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Typical => "typical",
            Self::DuplicateIds => "duplicate_ids",
            Self::AbsentIds => "absent_ids",
            Self::MismatchHeavy => "mismatch_heavy",
        }
    }

    /// The staged row for message `index` on one side, or `None` when the
    /// shape leaves it out of that side.
    fn message(
        self,
        destination: bool,
        index: usize,
    ) -> Option<(MailboxMessageKey, ExtractedMessage)> {
        let mut mailbox = "INBOX";
        let mut message_id = Some(format!("<m{index}@example>"));
        let mut size = 1_000 + index as u64;
        match self {
            Self::Typical => {
                if destination && index.is_multiple_of(50) {
                    size += 1;
                }
            }
            Self::DuplicateIds => {
                if index.is_multiple_of(100) {
                    message_id = Some("<hot@example>".to_owned());
                } else if index % 10 < 2 {
                    message_id = Some(format!("<group{}@example>", index / 50));
                }
                if destination && index % 50 == 1 {
                    size += 1;
                }
            }
            Self::AbsentIds => {
                if index.is_multiple_of(2) {
                    message_id = None;
                    if index.is_multiple_of(20) {
                        size = 777;
                    }
                }
            }
            Self::MismatchHeavy => {
                if destination {
                    match index % 10 {
                        0..=2 => size += 1,
                        3 => return None,
                        4 => mailbox = "Archive",
                        _ => {}
                    }
                }
            }
        }
        let flags = if index.is_multiple_of(3) {
            "\\Seen"
        } else {
            ""
        };
        Some((
            MailboxMessageKey::new(mailbox, format!("{index}")),
            ExtractedMessage {
                message_id,
                uid: Some(format!("{index}")),
                size_bytes: Some(size),
                internal_date: Some("01-Jan-2024 00:00:00 +0000".to_owned()),
                flags: Some(flags.to_owned()),
            },
        ))
    }

    /// Destination-only messages the shape adds.
    fn extra(self, index: usize) -> Option<(MailboxMessageKey, ExtractedMessage)> {
        (self == Self::MismatchHeavy && index % 10 == 5).then(|| {
            (
                MailboxMessageKey::new("INBOX", format!("x{index}")),
                ExtractedMessage {
                    message_id: Some(format!("<extra{index}@example>")),
                    uid: Some(format!("x{index}")),
                    size_bytes: Some(5_000_000 + index as u64),
                    internal_date: Some("02-Jan-2024 00:00:00 +0000".to_owned()),
                    flags: Some(String::new()),
                },
            )
        })
    }
}

/// Stage `messages` per side of `shape` in an in-memory stage.
fn bench_stage(shape: BenchShape, messages: usize) -> MessageMetadataStage {
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    for side in [StagedMessageSide::Source, StagedMessageSide::Destination] {
        let destination = side == StagedMessageSide::Destination;
        let mut page = (0..messages)
            .filter_map(|index| shape.message(destination, index))
            .collect::<ExtractedMessages>();
        if destination {
            page.extend((0..messages).filter_map(|index| shape.extra(index)));
        }
        stage.insert_messages(side, &page).unwrap();
    }
    stage
}

/// The query plans that keep staged reconciliation linear. Measured on a
/// durable stage at 100,000 messages per side, a shared date/size bucket
/// made probable pairing quadratic (62 s) and re-sorting a whole side for
/// every 512-row page made draining unmatched rows superlinear. A planner
/// change that reintroduces either fails here rather than at scale.
#[test]
fn staged_reconciliation_plans_stay_linear() {
    let mut plans = Vec::new();
    for shape in [
        BenchShape::AbsentIds,
        BenchShape::MismatchHeavy,
        BenchShape::DuplicateIds,
    ] {
        let stage = bench_stage(shape, 2_000);
        crate::core::stage_sql::plans::start();
        MessageVerification::detect_mismatches_from_stage("job", "run", &stage, &HashMap::new())
            .unwrap();
        MessageVerification::verify_staged_flags(&stage, &HashMap::new()).unwrap();
        plans.extend(crate::core::stage_sql::plans::finish());
    }
    let plan_for = |fragment: &str| {
        let matching = plans
            .iter()
            .filter(|(sql, _)| sql.contains(fragment))
            .map(|(_, plan)| plan.as_str())
            .collect::<Vec<_>>();
        assert!(
            !matching.is_empty(),
            "no statement containing {fragment:?} ran"
        );
        matching
    };
    for (sql, plan) in &plans {
        assert!(!plan.starts_with("unavailable"), "{sql}: {plan}");
        assert!(
            !plan
                .lines()
                .any(|line| line.trim() == "SCAN staged_messages"),
            "full stage scan in {sql}:\n{plan}"
        );
    }
    for plan in plan_for("FROM staged_messages NOT INDEXED WHERE side=?1 AND rowid>?2") {
        assert!(
            plan.contains("USING INTEGER PRIMARY KEY (rowid>?)"),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE FOR ORDER BY"), "{plan}");
    }
    for plan in plan_for(
        "FROM staged_messages d INDEXED BY staged_messages_exact_destination WHERE d.side=1 AND d.message_id=?1",
    ) {
        assert!(
            plan.contains("staged_messages_exact_destination (message_id=?"),
            "{plan}"
        );
    }
    for plan in plan_for("CREATE TABLE staged_probable_pairs") {
        assert!(
            plan.contains("USING INDEX staged_probable_source_key"),
            "{plan}"
        );
    }
}

/// `/proc/self/io` counters for the benchmark process. `*_bytes` are block
/// layer counters and `*_syscalls` count read/write calls, including page
/// cache and journal writes. Zero where unavailable.
#[derive(Clone, Copy, Default)]
struct BenchIo {
    read_bytes: u64,
    write_bytes: u64,
    read_syscalls: u64,
    write_syscalls: u64,
}

fn bench_io() -> BenchIo {
    let io = std::fs::read_to_string("/proc/self/io").unwrap_or_default();
    let field = |name: &str| {
        io.lines()
            .find_map(|line| line.strip_prefix(name)?.trim().parse().ok())
            .unwrap_or(0)
    };
    BenchIo {
        read_bytes: field("read_bytes:"),
        write_bytes: field("write_bytes:"),
        read_syscalls: field("syscr:"),
        write_syscalls: field("syscw:"),
    }
}

/// Reconciliation on a durable (FULL-synchronous) stage, as live
/// verification uses. Opt-in: `cargo test --release durable_stage_reconciliation_benchmark -- --ignored --nocapture`.
/// `scripts/benchmark-verification-scale.sh` runs each size and shape in its
/// own process so peak RSS is per run. Environment:
///
/// * `MAILSWIFTSYNC_RECONCILIATION_BENCH_MESSAGES` messages per side (20,000)
/// * `MAILSWIFTSYNC_RECONCILIATION_BENCH_SHAPE` a `BenchShape` (`typical`)
/// * `MAILSWIFTSYNC_RECONCILIATION_BENCH_DIR` stage parent directory; use a
///   real disk, since a tmpfs temp directory hides I/O cost
/// * `MAILSWIFTSYNC_RECONCILIATION_BENCH_PLANS` file to write every
///   statement's `EXPLAIN QUERY PLAN` to
///
/// Rows are staged in fetch-sized pages, as the live IMAP adapter does. After
/// the first pass the stage is closed, reopened, and reconciled again, which
/// is what a restarted verification does with a complete durable stage. A
/// reconciliation that stops at a safety limit is reported as its outcome.
#[test]
#[ignore = "opt-in durable-stage reconciliation benchmark"]
fn durable_stage_reconciliation_benchmark() {
    const PAGE: usize = 5_000;
    let messages = std::env::var("MAILSWIFTSYNC_RECONCILIATION_BENCH_MESSAGES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(20_000);
    assert!(messages > 0, "benchmark size must be positive");
    let shape = BenchShape::parse(
        &std::env::var("MAILSWIFTSYNC_RECONCILIATION_BENCH_SHAPE")
            .unwrap_or_else(|_| "typical".to_owned()),
    );
    let parent = std::env::var_os("MAILSWIFTSYNC_RECONCILIATION_BENCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let directory = parent.join(format!("mss-stage-bench-{}", uuid::Uuid::new_v4()));
    let builder = std::fs::DirBuilder::new();
    builder.create(&directory).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    let database = directory.join("stage.sqlite");
    let files_bytes = || -> u64 {
        std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(|entry| entry.ok()?.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    };
    let rss_start = bench_memory_kib("VmRSS:");
    let cpu_start = bench_cpu_seconds();
    let mut stage = MessageMetadataStage::open_durable(database.clone(), "bench").unwrap();
    let staging_started = std::time::Instant::now();
    let io_start = bench_io();
    let mut staged = [0_usize; 2];
    for (slot, side) in [StagedMessageSide::Source, StagedMessageSide::Destination]
        .into_iter()
        .enumerate()
    {
        let destination = side == StagedMessageSide::Destination;
        let mut start = 0;
        while start < messages {
            let end = (start + PAGE).min(messages);
            let mut page = (start..end)
                .filter_map(|index| shape.message(destination, index))
                .collect::<ExtractedMessages>();
            if destination {
                page.extend((start..end).filter_map(|index| shape.extra(index)));
            }
            staged[slot] += page.len();
            stage.insert_messages(side, &page).unwrap();
            start = end;
        }
    }
    let staging_ms = staging_started.elapsed().as_millis();
    let io_staged = bench_io();
    let rss_after_staging = bench_memory_kib("VmRSS:");
    let stage_bytes = files_bytes();
    // Sample the stage directory while reconciliation runs: the rollback
    // journal's peak size is the transient disk cost of one transaction.
    let sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sampler = {
        let sampling = std::sync::Arc::clone(&sampling);
        let directory = directory.clone();
        std::thread::spawn(move || {
            let (mut peak_total, mut peak_journal) = (0_u64, 0_u64);
            loop {
                let running = sampling.load(std::sync::atomic::Ordering::Relaxed);
                let mut total = 0;
                for entry in std::fs::read_dir(&directory)
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let length = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
                    total += length;
                    if entry.file_name().to_string_lossy().ends_with("-journal") {
                        peak_journal = peak_journal.max(length);
                    }
                }
                peak_total = peak_total.max(total);
                if !running {
                    return (peak_total, peak_journal);
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        })
    };
    let plans_path = std::env::var_os("MAILSWIFTSYNC_RECONCILIATION_BENCH_PLANS");
    if plans_path.is_some() {
        crate::core::stage_sql::plans::start();
    }
    let reconcile = |stage: &MessageMetadataStage| {
        let started = std::time::Instant::now();
        let result = MessageVerification::detect_mismatches_from_stage(
            "job-bench",
            "run-bench",
            stage,
            &HashMap::new(),
        );
        (result, started.elapsed().as_millis())
    };
    let (result, reconcile_ms) = reconcile(&stage);
    let io_reconciled = bench_io();
    let flags_started = std::time::Instant::now();
    let flags = MessageVerification::verify_staged_flags(&stage, &HashMap::new()).unwrap();
    let flags_ms = flags_started.elapsed().as_millis();
    sampling.store(false, std::sync::atomic::Ordering::Relaxed);
    let (peak_stage_bytes, peak_journal_bytes) = sampler.join().unwrap();
    if let Some(path) = plans_path {
        let plans = crate::core::stage_sql::plans::finish();
        let text = plans
            .iter()
            .map(|(sql, plan)| format!("-- {sql}\n{plan}\n"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(path, text).unwrap();
    }
    let final_stage_bytes = files_bytes();
    let outcome =
        |result: &Result<(Vec<MessageMismatch>, VerificationSummary), String>| match result {
            Ok(_) => "ok".to_owned(),
            Err(error) => match crate::core::VerificationLimit::from_detail(error) {
                Some(limit) => format!("limit:{}", limit.code()),
                None => panic!("reconciliation failed: {error}"),
            },
        };
    let first_outcome = outcome(&result);
    let (recorded, summary) = match &result {
        Ok((mismatches, summary)) => {
            assert_eq!(summary.total_source, staged[0] as u64);
            assert_eq!(summary.total_destination, staged[1] as u64);
            (mismatches.len(), Some(summary.clone()))
        }
        Err(_) => (0, None),
    };
    drop(result);

    // A restarted verification reopens the complete durable stage, discards
    // reconciliation intermediates, and reconciles again.
    drop(stage);
    let resume_io_start = bench_io();
    let resume_started = std::time::Instant::now();
    let mut stage = MessageMetadataStage::open_durable(database, "bench").unwrap();
    stage.reset_reconciliation().unwrap();
    let (resumed, _) = reconcile(&stage);
    let resumed_ms = resume_started.elapsed().as_millis();
    let resume_io = bench_io();
    assert_eq!(
        outcome(&resumed),
        first_outcome,
        "resumed pass changed outcome"
    );
    if let (Ok((_, resumed_summary)), Some(summary)) = (&resumed, &summary) {
        assert_eq!(resumed_summary, summary, "resumed pass changed the result");
    }
    drop(resumed);

    let cpu_seconds = bench_cpu_seconds() - cpu_start;
    let rate = |count: usize, millis: u128| (count as f64 * 1_000.0 / millis.max(1) as f64) as u64;
    let summary_fields = summary.map_or_else(String::new, |summary| {
        format!(
            " matched={} probable={} missing={} extra={} duplicated={} changed={}",
            summary.metadata_matches,
            summary.probable_matches,
            summary.missing_count,
            summary.extra_count,
            summary.duplicated_count,
            summary.changed_count
        )
    });
    eprintln!(
        "verification-scale shape={} messages_per_side={messages} outcome={first_outcome} staging_ms={staging_ms} staging_rows_per_sec={} staging_read_bytes={} staging_write_bytes={} staging_read_syscalls={} staging_write_syscalls={} reconcile_ms={reconcile_ms} reconcile_messages_per_sec={} reconcile_read_bytes={} reconcile_write_bytes={} reconcile_read_syscalls={} reconcile_write_syscalls={} flags_ms={flags_ms} flags_compared={} resumed_reconcile_ms={resumed_ms} resumed_write_bytes={} cpu_seconds={cpu_seconds:.1} peak_rss_mib={} rss_start_mib={} rss_after_staging_mib={} stage_bytes_after_staging={stage_bytes} peak_stage_bytes_during_reconcile={peak_stage_bytes} peak_journal_bytes={peak_journal_bytes} stage_bytes_after_reconcile={final_stage_bytes} mismatch_rows={recorded}{summary_fields}",
        shape.label(),
        rate(staged[0] + staged[1], staging_ms),
        io_staged.read_bytes - io_start.read_bytes,
        io_staged.write_bytes - io_start.write_bytes,
        io_staged.read_syscalls - io_start.read_syscalls,
        io_staged.write_syscalls - io_start.write_syscalls,
        rate(messages, reconcile_ms),
        io_reconciled.read_bytes - io_staged.read_bytes,
        io_reconciled.write_bytes - io_staged.write_bytes,
        io_reconciled.read_syscalls - io_staged.read_syscalls,
        io_reconciled.write_syscalls - io_staged.write_syscalls,
        flags.compared_messages,
        resume_io.write_bytes - resume_io_start.write_bytes,
        bench_memory_kib("VmHWM:") / 1024,
        rss_start / 1024,
        rss_after_staging / 1024,
    );
    drop(stage);
    let _ = std::fs::remove_dir_all(directory);
}

/// Linux `/proc/self/status` memory field in KiB; 0 where unavailable.
fn bench_memory_kib(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with(field))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// User plus system CPU seconds consumed by this process.
fn bench_cpu_seconds() -> f64 {
    #[cfg(unix)]
    {
        // SAFETY: getrusage writes only into the zeroed struct we pass.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
            let seconds = |time: libc::timeval| time.tv_sec as f64 + time.tv_usec as f64 / 1e6;
            return seconds(usage.ru_utime) + seconds(usage.ru_stime);
        }
    }
    0.0
}

#[test]
fn reset_discards_intermediates_left_by_an_aborted_reconciliation() {
    let message_at = |uid: &str| {
        (
            MailboxMessageKey::new("INBOX", uid),
            message(Some("<id>"), uid, 10, "01-Jan-2024 00:00:00 +0000"),
        )
    };
    let source = HashMap::from([message_at("1")]);
    let destination = HashMap::from([message_at("2")]);
    let mut stage = MessageMetadataStage::open_in_memory().unwrap();
    stage
        .insert_messages(StagedMessageSide::Source, &source)
        .unwrap();
    stage
        .insert_messages(StagedMessageSide::Destination, &destination)
        .unwrap();
    let tables = |stage: &MessageMetadataStage| {
        stage
            .connection()
            .unwrap()
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    };
    let baseline = tables(&stage);
    // A pass aborted under journal_mode=OFF keeps whatever DDL it ran.
    for table in [
        "staged_matched",
        "staged_folder_mapping",
        "staged_exact_source_ranked",
        "staged_exact_destination_ranked",
        "staged_exact_pairs",
        "staged_changed_source_ranked",
        "staged_changed_destination_ranked",
        "staged_changed_pairs",
        "staged_probable_source",
        "staged_probable_destination",
        "staged_probable_pairs",
        "staged_duplicate_ids",
    ] {
        stage
            .connection()
            .unwrap()
            .execute_batch(&format!("CREATE TABLE {table}(leftover INTEGER)"))
            .unwrap();
    }
    stage.reset_reconciliation().unwrap();
    assert_eq!(tables(&stage), baseline);
    let (_, summary) =
        MessageVerification::detect_mismatches_from_stage("job", "run", &stage, &HashMap::new())
            .unwrap();
    assert_eq!(summary.total_source, 1);
}

/// Acceptance matrix: every deliberately introduced integrity defect is either
/// detected by metadata reconciliation (level 2), or is outside that level
/// (identical metadata, different body) and then detected by bounded body
/// fingerprints (level 3). No defect may produce a clean result at the level
/// that claims to cover it.
#[test]
fn every_injected_integrity_defect_is_detected_or_outside_the_selected_level() {
    fn message(id: Option<&str>, uid: &str, size: u64, date: &str) -> ExtractedMessage {
        ExtractedMessage {
            message_id: id.map(str::to_owned),
            uid: Some(uid.to_owned()),
            size_bytes: Some(size),
            internal_date: Some(date.to_owned()),
            flags: None,
        }
    }
    let baseline = || {
        let mut messages = HashMap::new();
        messages.insert(
            key("1"),
            message(Some("<a@example.test>"), "1", 100, "2024-01-01"),
        );
        messages.insert(
            key("2"),
            message(Some("<b@example.test>"), "2", 200, "2024-01-02"),
        );
        // Two distinct source messages share a Message-ID.
        messages.insert(
            key("3"),
            message(Some("<dup@example.test>"), "3", 300, "2024-01-03"),
        );
        messages.insert(
            key("4"),
            message(Some("<dup@example.test>"), "4", 301, "2024-01-04"),
        );
        messages
    };
    let fingerprints = |messages: &HashMap<MailboxMessageKey, ExtractedMessage>| {
        messages
            .iter()
            .map(|(key, message)| {
                (
                    key.clone(),
                    format!("body-{}", message.message_id.clone().unwrap_or_default()),
                )
            })
            .collect::<HashMap<_, _>>()
    };
    let source = baseline();
    let clean = |summary: &VerificationSummary| {
        summary.metadata_matches == summary.total_source
            && summary.total_source == summary.total_destination
            && summary.missing_count == 0
            && summary.extra_count == 0
            && summary.duplicated_count == 0
            && summary.changed_count == 0
            && summary.probable_matches == 0
    };

    type Defect = fn(&mut HashMap<MailboxMessageKey, ExtractedMessage>);
    let metadata_defects: [(&str, Defect); 8] = [
        ("missing message", |dest| {
            dest.remove(&key("2"));
        }),
        ("unexpected message", |dest| {
            dest.insert(
                key("9"),
                message(Some("<new@example.test>"), "9", 900, "2024-02-01"),
            );
        }),
        ("duplicated copy", |dest| {
            dest.insert(
                key("9"),
                message(Some("<a@example.test>"), "9", 100, "2024-01-01"),
            );
        }),
        ("altered INTERNALDATE", |dest| {
            dest.get_mut(&key("1")).unwrap().internal_date = Some("2025-06-30".into());
        }),
        ("altered size", |dest| {
            dest.get_mut(&key("1")).unwrap().size_bytes = Some(101);
        }),
        ("message moved to another folder", |dest| {
            let moved = dest.remove(&key("2")).unwrap();
            dest.insert(MailboxMessageKey::new("Archive", "2"), moved);
        }),
        ("Message-ID stripped on destination", |dest| {
            dest.get_mut(&key("1")).unwrap().message_id = None;
        }),
        ("one of two duplicate Message-IDs lost", |dest| {
            dest.remove(&key("4"));
        }),
    ];
    for (name, inject) in metadata_defects {
        let mut destination = baseline();
        inject(&mut destination);
        let (_, summary) = MessageVerification::detect_mismatches_with_folder_mapping(
            "matrix-job",
            "matrix-run",
            &source,
            &destination,
            &HashMap::new(),
        )
        .unwrap();
        assert!(
            !clean(&summary),
            "{name}: metadata reconciliation reported clean: {summary:?}"
        );
    }

    // Identical metadata, different body: outside level 2 by construction.
    let destination = baseline();
    let (_, metadata_only) = MessageVerification::detect_mismatches_with_folder_mapping(
        "matrix-job",
        "matrix-run",
        &source,
        &destination,
        &HashMap::new(),
    )
    .unwrap();
    assert!(clean(&metadata_only), "{metadata_only:?}");
    let source_bodies = fingerprints(&source);
    let mut destination_bodies = fingerprints(&destination);
    destination_bodies.insert(key("2"), "tampered-body".into());
    let (_, with_bodies) = MessageVerification::detect_mismatches_with_content_fingerprints(
        "matrix-job",
        "matrix-run",
        &source,
        &destination,
        &source_bodies,
        &destination_bodies,
        &HashMap::new(),
    )
    .unwrap();
    assert!(
        !clean(&with_bodies),
        "identical metadata with a different body passed body-fingerprint verification: {with_bodies:?}"
    );
}

/// Populate a stage with 300 source messages; the destination lacks 100 of
/// them and holds 50 unrelated extras, so reconciliation finds 150 mismatches.
fn mismatching_stage(stage: &mut MessageMetadataStage) {
    let source = (0..300)
        .map(|index| {
            let uid = index.to_string();
            (
                MailboxMessageKey::new("INBOX", &uid),
                message(
                    Some(&format!("<m{index}@x>")),
                    &uid,
                    100 + index,
                    "2024-01-01",
                ),
            )
        })
        .collect::<ExtractedMessages>();
    let destination = (0..250)
        .map(|index| {
            let uid = index.to_string();
            if index < 200 {
                (
                    MailboxMessageKey::new("INBOX", &uid),
                    message(
                        Some(&format!("<m{index}@x>")),
                        &uid,
                        100 + index,
                        "2024-01-01",
                    ),
                )
            } else {
                (
                    MailboxMessageKey::new("INBOX", &uid),
                    message(
                        Some(&format!("<extra{index}@x>")),
                        &uid,
                        9_000 + index,
                        "2024-02-01",
                    ),
                )
            }
        })
        .collect::<ExtractedMessages>();
    stage
        .insert_messages(StagedMessageSide::Source, &source)
        .unwrap();
    stage
        .insert_messages(StagedMessageSide::Destination, &destination)
        .unwrap();
}

/// Commit mismatches for a fresh run and return the ledger rows. `build`
/// receives the committing job ID, which every mismatch row must carry.
/// A committed mismatch row: type, source UID, and destination UID.
type CommittedMismatchRow = (String, Option<String>, Option<String>);

fn committed_mismatch_rows(
    build: impl FnOnce(&str) -> MismatchSet,
) -> rusqlite::Result<Vec<CommittedMismatchRow>> {
    let store = crate::core::StateStore::in_memory().unwrap();
    let project = store
        .create_project("stream", "source", "destination")
        .unwrap();
    let job = store
        .add_mailbox(&project.id, "source", "destination")
        .unwrap();
    store
        .begin_run(&project.id, &job, "run", "imapsync")
        .unwrap();
    let evidence = crate::core::MailboxEvidence {
        verification_method: crate::core::VerificationMethod::MetadataReconciliation,
        verification_outcome: Some(crate::core::VerificationOutcome::Missing),
        source_messages: 300,
        destination_messages: 250,
        source_bytes: 0,
        destination_bytes: 0,
        unmatched_messages: Some(150),
        failed_messages: 0,
        source_folders: 1,
        destination_folders: 1,
        authoritative: false,
        missing_messages: 100,
        extra_messages: 50,
        modified_messages: 0,
        probable_messages: 0,
        flag_verification: None,
    };
    let mismatches = build(&job);
    store.finish_run_for_mailbox_with_evidence_and_mismatches_and_preflight_plan_and_checkpoint(
        &project.id,
        &job,
        "run",
        "completed",
        "verification_difference",
        "",
        &evidence,
        &mismatches,
        None,
        None,
    )?;
    let mut statement = store
        .connection
        .prepare("SELECT mismatch_type,source_uid,dest_uid FROM message_mismatches ORDER BY mismatch_type,source_uid,dest_uid")
        .unwrap();
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    Ok(rows)
}

#[test]
fn staged_mismatches_stream_into_the_ledger_exactly_like_in_memory_ones() {
    let mut in_memory_stage = MessageMetadataStage::open_in_memory().unwrap();
    mismatching_stage(&mut in_memory_stage);
    let (in_memory, summary) = MessageVerification::detect_mismatches_from_stage(
        "job",
        "run",
        &in_memory_stage,
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!((summary.missing_count, summary.extra_count), (100, 50));
    assert_eq!(in_memory.len(), 150);

    let directory = crate::credentials::create_secret_directory().unwrap();
    let path = directory.join("verification.sqlite");
    let mut durable = MessageMetadataStage::open_durable(path.clone(), "plan").unwrap();
    mismatching_stage(&mut durable);
    let (count, staged_summary) =
        MessageVerification::reconcile_stage("job", "run", &durable, &HashMap::new()).unwrap();
    assert_eq!(count, 150);
    assert_eq!(staged_summary.missing_count, summary.missing_count);
    assert_eq!(staged_summary.extra_count, summary.extra_count);
    let folders = durable.staged_mismatch_folders().unwrap();
    assert_eq!(folders, ["INBOX"]);
    drop(durable);

    let staged = |path: std::path::PathBuf, count: u64| {
        let folders = folders.clone();
        move |job: &str| MismatchSet::Staged {
            stage_path: path,
            job_id: Arc::from(job),
            run_id: Arc::from("run"),
            count,
            folders,
        }
    };
    let streamed = committed_mismatch_rows(staged(path.clone(), count)).unwrap();
    let held = committed_mismatch_rows(|job| {
        MismatchSet::InMemory(
            in_memory
                .iter()
                .cloned()
                .map(|mut mismatch| {
                    mismatch.job_id = Arc::from(job);
                    mismatch.run_id = Arc::from("run");
                    mismatch
                })
                .collect(),
        )
    })
    .unwrap();
    assert_eq!(streamed.len(), 150);
    assert_eq!(streamed, held);

    // A stage that no longer holds exactly the reconciled rows is not the
    // evidence being committed; the commit is rejected as a whole.
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("DELETE FROM staged_mismatches WHERE ordinal=1", [])
        .unwrap();
    let error = committed_mismatch_rows(staged(path.clone(), count)).unwrap_err();
    assert!(crate::core::is_ledger_rejection(&error), "{error}");
    let _ = std::fs::remove_dir_all(directory);
}
