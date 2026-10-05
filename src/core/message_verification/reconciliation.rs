//! In-memory metadata reconciliation: exact Message-ID, wrong-folder, and fingerprint passes.

use super::*;

pub(super) fn index_by_message_id(
    messages: &ExtractedMessages,
) -> HashMap<&str, Vec<&MailboxMessageKey>> {
    let mut index = HashMap::new();
    for (uid, message) in messages {
        if let Some(message_id) = message
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|message_id| !message_id.is_empty())
        {
            index.entry(message_id).or_insert_with(Vec::new).push(uid);
        }
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

pub(super) fn unique_fingerprint_index(
    fingerprints: &HashMap<MailboxMessageKey, String>,
) -> HashMap<&String, &MailboxMessageKey> {
    let mut index = HashMap::new();
    let mut duplicated = HashSet::new();
    for key in fingerprints.keys() {
        let fingerprint = &fingerprints[key];
        if duplicated.contains(fingerprint) {
            continue;
        }
        if index.insert(fingerprint, key).is_some() {
            index.remove(fingerprint);
            duplicated.insert(fingerprint);
        }
    }
    index
}

pub(super) fn index_by_fingerprint<'a>(
    messages: &'a ExtractedMessages,
    eligible: &HashSet<&'a MailboxMessageKey>,
    folder_mapping: &'a HashMap<String, String>,
    source_side: bool,
) -> HashMap<(&'a str, MetadataFingerprint<'a>), Vec<&'a MailboxMessageKey>> {
    let mut index = HashMap::new();
    for uid in eligible {
        let message = &messages[*uid];
        let Some(fingerprint) = metadata_fingerprint(message) else {
            continue;
        };
        let folder: &'a str = if source_side {
            expected_destination_folder(uid, folder_mapping)
        } else {
            uid.mailbox.as_ref()
        };
        index
            .entry((folder, fingerprint))
            .or_insert_with(Vec::new)
            .push(*uid);
    }
    index.values_mut().for_each(|uids| uids.sort());
    index
}

pub(super) fn expected_destination_folder<'a>(
    source_key: &'a MailboxMessageKey,
    folder_mapping: &'a HashMap<String, String>,
) -> &'a str {
    folder_mapping
        .get(source_key.mailbox.as_ref())
        .map(String::as_str)
        .unwrap_or_else(|| source_key.mailbox.as_ref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum NormalizedInternalDate<'a> {
    Epoch(i64),
    Raw(&'a str),
}

/// Borrowed metadata key used only while reconciling one in-memory batch.
/// Parsed IMAP dates use their UTC epoch; non-IMAP synthetic dates retain
/// their original representation for compatibility with extracted fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct MetadataFingerprint<'a> {
    internal_date: NormalizedInternalDate<'a>,
    size_bytes: u64,
}

pub(super) fn metadata_fingerprint(message: &ExtractedMessage) -> Option<MetadataFingerprint<'_>> {
    Some(MetadataFingerprint {
        internal_date: normalize_internal_date(message.internal_date.as_deref()?),
        size_bytes: message.size_bytes?,
    })
}

pub(super) fn normalize_internal_date(value: &str) -> NormalizedInternalDate<'_> {
    DateTime::<FixedOffset>::parse_from_str(value.trim(), "%d-%b-%Y %H:%M:%S %z")
        .map(|date| NormalizedInternalDate::Epoch(date.timestamp()))
        .unwrap_or(NormalizedInternalDate::Raw(value))
}

impl MessageVerification {
    /// Detect mismatches between source and destination message sets.
    ///
    /// IMAP UIDs are mailbox-local and are deliberately never used as the
    /// identity across the two inputs. Messages are matched first by a unique
    /// RFC Message-ID, then by a unique internal-date/size fingerprint. A
    /// UID is retained only as diagnostic context in the result.
    #[cfg(test)]
    pub fn detect_mismatches(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        Self::detect_mismatches_with_folder_mapping(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            &HashMap::new(),
        )
    }

    /// Detect mismatches while enforcing the expected source-to-destination
    /// folder mapping. An absent mapping entry means the source folder is
    /// expected to retain its name. Provider-specific label semantics belong
    /// in the mapping supplied by the caller, not in this generic verifier.
    /// Reconciliation Pass 1: Message-ID + exact metadata matching.
    /// Returns (mismatches created, source keys matched, dest keys matched).
    pub(super) fn pass_1_message_id_exact_metadata<'a>(
        job_id: &Arc<str>,
        run_id: &Arc<str>,
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        folder_mapping: &'a HashMap<String, String>,
        source_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
        dest_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
    ) -> Pass1Result<'a> {
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();
        let mut clean_pairs = Vec::new();

        let mut message_ids: Vec<_> = source_by_message_id
            .keys()
            .filter(|id| dest_by_message_id.contains_key(*id))
            .copied()
            .collect();
        message_ids.sort_unstable();

        for message_id in message_ids {
            let source_uids = &source_by_message_id[message_id];
            let dest_uids = &dest_by_message_id[message_id];

            let mut destination_by_metadata =
                HashMap::<(&'a str, MetadataFingerprint<'a>), VecDeque<&MailboxMessageKey>>::new();
            for dest_key in dest_uids {
                if let Some(fingerprint) = metadata_fingerprint(&dest_messages[*dest_key]) {
                    destination_by_metadata
                        .entry((dest_key.mailbox.as_ref(), fingerprint))
                        .or_default()
                        .push_back(*dest_key);
                }
            }

            for source_key in source_uids {
                let source_msg = &source_messages[source_key];
                let Some(fingerprint) = metadata_fingerprint(source_msg) else {
                    continue;
                };
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                if let Some(dest_key) = destination_by_metadata
                    .get_mut(&(expected_folder, fingerprint))
                    .and_then(VecDeque::pop_front)
                {
                    matched_source.insert(*source_key);
                    matched_dest.insert(dest_key);
                    clean_pairs.push((*source_key, dest_key));
                }
            }

            // Handle metadata-mismatched but ID-matched pairs
            let mut destination_by_folder = HashMap::<&'a str, VecDeque<&MailboxMessageKey>>::new();
            for dest_key in dest_uids {
                if !matched_dest.contains(dest_key) {
                    destination_by_folder
                        .entry(dest_key.mailbox.as_ref())
                        .or_default()
                        .push_back(*dest_key);
                }
            }
            for source_key in source_uids {
                if matched_source.contains(source_key) {
                    continue;
                }
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                let Some(dest_key) = destination_by_folder
                    .get_mut(&expected_folder)
                    .and_then(VecDeque::pop_front)
                else {
                    continue;
                };
                let source_msg = &source_messages[source_key];
                let dest_msg = &dest_messages[dest_key];
                matched_source.insert(*source_key);
                matched_dest.insert(dest_key);
                append_mismatch_with_budget(
                    &mut mismatches,
                    make_mismatch(
                        job_id,
                        run_id,
                        MismatchType::MessageIdOnly,
                        Some(source_key),
                        Some(dest_key),
                        Some(source_msg),
                        Some(dest_msg),
                    ),
                    &mut estimated_bytes,
                )?;
            }
        }

        Ok((mismatches, matched_source, matched_dest, clean_pairs))
    }

    /// Reconciliation Pass 2: Wrong-folder detection for Message-ID matches.
    /// Finds messages with identical Message-ID and metadata but in unexpected folders.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn pass_2_wrong_folder_detection<'a>(
        job_id: &Arc<str>,
        run_id: &Arc<str>,
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        folder_mapping: &'a HashMap<String, String>,
        unmatched_source: &HashSet<&MailboxMessageKey>,
        unmatched_dest: &HashSet<&MailboxMessageKey>,
        source_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
        dest_by_message_id: &HashMap<&'a str, Vec<&'a MailboxMessageKey>>,
    ) -> ReconciliationPassResult<'a> {
        let mut mismatches = Vec::new();
        let mut estimated_bytes = 0usize;
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();

        let mut message_ids = source_by_message_id
            .keys()
            .filter(|message_id| dest_by_message_id.contains_key(*message_id))
            .copied()
            .collect::<Vec<_>>();
        message_ids.sort_unstable();
        for message_id in message_ids {
            let source_uids = &source_by_message_id[message_id];
            let Some(dest_uids) = dest_by_message_id.get(message_id) else {
                continue;
            };

            // Build an ordered folder index for each metadata fingerprint. A
            // source message can select the nearest deterministic folder on
            // either side of its expected folder without scanning every
            // folder bucket in this Message-ID group.
            let mut dest_by_metadata: HashMap<
                Option<MetadataFingerprint<'a>>,
                BTreeMap<&'a str, VecDeque<&MailboxMessageKey>>,
            > = HashMap::new();
            for dest_key in dest_uids {
                if !unmatched_dest.contains(dest_key) {
                    continue;
                }
                let metadata = metadata_fingerprint(&dest_messages[dest_key]);
                dest_by_metadata
                    .entry(metadata)
                    .or_default()
                    .entry(dest_key.mailbox.as_ref())
                    .or_default()
                    .push_back(dest_key);
            }

            // For each unmatched source with this Message-ID, try to find it in a wrong folder
            for source_key in source_uids {
                if !unmatched_source.contains(source_key) {
                    continue;
                }
                let expected_folder = expected_destination_folder(source_key, folder_mapping);
                let source_metadata = metadata_fingerprint(&source_messages[source_key]);

                // Try to find in any wrong folder with matching metadata.
                // The range lookup is O(log F), where F is the number of
                // folders in this duplicate-ID group, instead of O(F) for
                // every source candidate.
                let Some(folder_candidates) = dest_by_metadata.get_mut(&source_metadata) else {
                    continue;
                };
                let lower = folder_candidates.range(..expected_folder).next_back();
                let upper = folder_candidates
                    .range::<&str, _>((Excluded(expected_folder), Unbounded))
                    .next();
                // Match the staged SQL rule: choose the greatest folder below
                // the expected folder, falling back to the smallest above it.
                let candidate_folder = lower
                    .map(|(folder, _)| *folder)
                    .or_else(|| upper.map(|(folder, _)| *folder));
                let Some(candidate_folder) = candidate_folder else {
                    continue;
                };
                let Some(dest_key) = folder_candidates
                    .get_mut(&candidate_folder)
                    .and_then(VecDeque::pop_front)
                else {
                    continue;
                };
                if folder_candidates
                    .get(&candidate_folder)
                    .is_some_and(VecDeque::is_empty)
                {
                    folder_candidates.remove(&candidate_folder);
                }
                matched_source.insert(*source_key);
                matched_dest.insert(dest_key);
                append_mismatch_with_budget(
                    &mut mismatches,
                    make_mismatch(
                        job_id,
                        run_id,
                        MismatchType::PresentWrongFolder,
                        Some(source_key),
                        Some(dest_key),
                        Some(&source_messages[source_key]),
                        Some(&dest_messages[dest_key]),
                    ),
                    &mut estimated_bytes,
                )?;
            }
        }

        Ok((mismatches, matched_source, matched_dest))
    }

    /// Reconciliation Pass 3: Fingerprint fallback for unmatched messages.
    /// Only processes messages not matched in Passes 1-2.
    pub(super) fn pass_3_fingerprint_fallback<'a>(
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        unmatched_source: &HashSet<&'a MailboxMessageKey>,
        unmatched_dest: &HashSet<&'a MailboxMessageKey>,
        folder_mapping: &'a HashMap<String, String>,
    ) -> (
        u64,
        HashSet<&'a MailboxMessageKey>,
        HashSet<&'a MailboxMessageKey>,
    ) {
        let mut matched_source = HashSet::new();
        let mut matched_dest = HashSet::new();

        let source_by_fingerprint =
            index_by_fingerprint(source_messages, unmatched_source, folder_mapping, true);
        let dest_by_fingerprint =
            index_by_fingerprint(dest_messages, unmatched_dest, folder_mapping, false);

        let mut fingerprints: Vec<_> = source_by_fingerprint
            .keys()
            .filter(|fp| dest_by_fingerprint.contains_key(fp))
            .cloned()
            .collect();
        fingerprints.sort();

        let mut probable_matches = 0_u64;
        for fingerprint in fingerprints {
            let source_uids = &source_by_fingerprint[&fingerprint];
            let dest_uids = &dest_by_fingerprint[&fingerprint];
            if source_uids.len() == 1 && dest_uids.len() == 1 {
                matched_source.insert(source_uids[0]);
                matched_dest.insert(dest_uids[0]);
                probable_matches += 1;
            }
        }

        (probable_matches, matched_source, matched_dest)
    }

    #[cfg(test)]
    pub fn detect_mismatches_with_folder_mapping(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        let reconciliation = Self::reconcile_metadata(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            folder_mapping,
        )?;
        Ok((reconciliation.mismatches, reconciliation.summary))
    }

    #[allow(unused_assignments, clippy::collapsible_if, clippy::needless_borrow)]
    pub(super) fn reconcile_metadata<'a>(
        job_id: &str,
        run_id: &str,
        source_messages: &'a ExtractedMessages,
        dest_messages: &'a ExtractedMessages,
        folder_mapping: &'a HashMap<String, String>,
    ) -> Result<MetadataReconciliation<'a>, String> {
        let estimated_state_bytes = estimated_verifier_state_bytes(source_messages, dest_messages);
        enforce_verifier_state_budget(estimated_state_bytes)?;
        let job_id: Arc<str> = Arc::from(job_id);
        let run_id: Arc<str> = Arc::from(run_id);
        let mut all_mismatches = Vec::new();
        let mut estimated_detail_bytes = 0usize;
        let mut total_metadata_matches = 0_u64;
        let mut total_probable_matches = 0_u64;

        let source_by_message_id = index_by_message_id(source_messages);
        let dest_by_message_id = index_by_message_id(dest_messages);

        // Pass 1: Message-ID + exact metadata matching
        let (mut pass1_mismatches, pass1_matched_src, pass1_matched_dst, clean_pairs) =
            Self::pass_1_message_id_exact_metadata(
                &job_id,
                &run_id,
                source_messages,
                dest_messages,
                folder_mapping,
                &source_by_message_id,
                &dest_by_message_id,
            )?;
        // Pass 1 returns one mismatch for each matched source that failed
        // exact metadata comparison. Counting those directly avoids scanning
        // the growing mismatch vector once per matched message.
        let pass1_mismatch_count = pass1_mismatches.len();
        total_metadata_matches =
            pass1_matched_src.len().saturating_sub(pass1_mismatch_count) as u64;
        // Move the first-pass records into the final output instead of
        // cloning every mismatch and retaining two full vectors concurrently.
        estimated_detail_bytes = estimated_detail_bytes.saturating_add(
            pass1_mismatches
                .iter()
                .map(estimated_mismatch_bytes)
                .sum::<usize>(),
        );
        if estimated_detail_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
            return Err(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            ));
        }
        all_mismatches.append(&mut pass1_mismatches);

        // Build unmatched sets for Pass 2
        // Borrow canonical map keys throughout reconciliation. Cloning every
        // key into both unmatched sets materially inflates peak memory on
        // large accounts; owned keys are created only for final membership
        // evidence after classification is complete.
        let mut unmatched_source: HashSet<&MailboxMessageKey> = source_messages
            .keys()
            .filter(|k| !pass1_matched_src.contains(k))
            .collect();
        let mut unmatched_dest: HashSet<&MailboxMessageKey> = dest_messages
            .keys()
            .filter(|k| !pass1_matched_dst.contains(k))
            .collect();

        // Pass 2: Wrong-folder detection for Message-ID matches
        let (mut pass2_mismatches, pass2_matched_src, pass2_matched_dst) =
            Self::pass_2_wrong_folder_detection(
                &job_id,
                &run_id,
                source_messages,
                dest_messages,
                folder_mapping,
                &unmatched_source,
                &unmatched_dest,
                &source_by_message_id,
                &dest_by_message_id,
            )?;
        estimated_detail_bytes = estimated_detail_bytes.saturating_add(
            pass2_mismatches
                .iter()
                .map(estimated_mismatch_bytes)
                .sum::<usize>(),
        );
        if estimated_detail_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
            return Err(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            ));
        }
        all_mismatches.append(&mut pass2_mismatches);
        for key in &pass2_matched_src {
            unmatched_source.remove(key);
        }
        for key in &pass2_matched_dst {
            unmatched_dest.remove(key);
        }

        // Pass 3: Fingerprint fallback for unmatched messages
        let (pass3_probable, pass3_matched_src, pass3_matched_dst) =
            Self::pass_3_fingerprint_fallback(
                source_messages,
                dest_messages,
                &unmatched_source,
                &unmatched_dest,
                folder_mapping,
            );
        total_probable_matches = pass3_probable;
        for key in &pass3_matched_src {
            unmatched_source.remove(key);
        }
        for key in &pass3_matched_dst {
            unmatched_dest.remove(key);
        }

        // Report remaining unmatched messages
        let mut unmatched_source_vec: Vec<_> = unmatched_source.iter().collect();
        unmatched_source_vec.sort();
        for source_uid in unmatched_source_vec {
            let source_msg = &source_messages[source_uid];
            append_mismatch_with_budget(
                &mut all_mismatches,
                make_mismatch(
                    &job_id,
                    &run_id,
                    MismatchType::Missing,
                    Some(source_uid),
                    None,
                    Some(source_msg),
                    None,
                ),
                &mut estimated_detail_bytes,
            )?;
        }

        // Report duplicates and extra destination messages
        let mut duplicate_dest_uids: Vec<_> = dest_by_message_id
            .iter()
            .filter(|(message_id, dest_uids)| {
                source_by_message_id
                    .get(*message_id)
                    .is_some_and(|source_uids| dest_uids.len() > source_uids.len())
            })
            .flat_map(|(_, uids)| uids.iter())
            .filter(|uid| unmatched_dest.contains(*uid))
            .cloned()
            .collect();
        duplicate_dest_uids.sort();
        for dest_uid in duplicate_dest_uids {
            let dest_msg = &dest_messages[&dest_uid];
            unmatched_dest.remove(&dest_uid);
            append_mismatch_with_budget(
                &mut all_mismatches,
                make_mismatch(
                    &job_id,
                    &run_id,
                    MismatchType::Duplicated,
                    None,
                    Some(&dest_uid),
                    None,
                    Some(dest_msg),
                ),
                &mut estimated_detail_bytes,
            )?;
        }

        let mut unmatched_dest_vec: Vec<_> = unmatched_dest.iter().collect();
        unmatched_dest_vec.sort();
        for dest_uid in unmatched_dest_vec {
            let dest_msg = &dest_messages[dest_uid];
            append_mismatch_with_budget(
                &mut all_mismatches,
                make_mismatch(
                    &job_id,
                    &run_id,
                    MismatchType::Extra,
                    None,
                    Some(dest_uid),
                    None,
                    Some(dest_msg),
                ),
                &mut estimated_detail_bytes,
            )?;
        }

        let source_index = build_uid_folder_index(source_messages);
        let dest_index = build_uid_folder_index(dest_messages);
        let mut missing_source: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut extra_destination: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut duplicated_destination: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut changed_source: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut changed_destination: HashSet<&MailboxMessageKey> = HashSet::new();
        let mut missing_count = 0_u64;
        let mut extra_count = 0_u64;
        let mut duplicated_count = 0_u64;
        let mut changed_count = 0_u64;
        for mismatch in &all_mismatches {
            match mismatch.mismatch_type {
                MismatchType::Missing => {
                    missing_count += 1;
                    if let Some(key) = mismatch_source_key(mismatch, &source_index) {
                        missing_source.insert(key);
                    }
                }
                MismatchType::Extra => {
                    extra_count += 1;
                    if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                        extra_destination.insert(key);
                    }
                }
                MismatchType::Duplicated => {
                    duplicated_count += 1;
                    if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                        duplicated_destination.insert(key);
                    }
                }
                MismatchType::MessageIdOnly | MismatchType::PresentWrongFolder => {
                    changed_count += 1;
                    if let Some(key) = mismatch_source_key(mismatch, &source_index) {
                        changed_source.insert(key);
                    }
                    if let Some(key) = mismatch_destination_key(mismatch, &dest_index) {
                        changed_destination.insert(key);
                    }
                }
            }
        }

        let summary = VerificationSummary {
            total_source: source_messages.len() as u64,
            total_destination: dest_messages.len() as u64,
            metadata_matches: total_metadata_matches,
            probable_matches: total_probable_matches,
            missing_count,
            extra_count,
            duplicated_count,
            changed_count,
        };

        let membership = VerificationMembership {
            matched_source: pass1_matched_src
                .iter()
                .copied()
                .filter(|key| !changed_source.contains(*key))
                .collect(),
            matched_destination: pass1_matched_dst
                .iter()
                .copied()
                .filter(|key| !changed_destination.contains(*key))
                .collect(),
            probable_source: pass3_matched_src,
            probable_destination: pass3_matched_dst,
            missing_source,
            extra_destination,
            duplicated_destination,
            changed_source,
            changed_destination,
        };
        validate_verification_summary(
            source_messages,
            dest_messages,
            &all_mismatches,
            &summary,
            &membership,
        )?;

        Ok(MetadataReconciliation {
            mismatches: all_mismatches,
            summary,
            membership,
            clean_pairs,
        })
    }
}
