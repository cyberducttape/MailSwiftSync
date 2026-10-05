//! Body-content fingerprint verification layered on metadata reconciliation.

use super::*;

impl MessageVerification {
    /// Reconcile metadata first, then enrich the reconciler's own pairs with
    /// independently fetched RFC822 fingerprints:
    ///
    /// identity -> placement -> metadata -> content
    ///
    /// Content never forms a pairing the metadata reconciler rejected. It may
    /// (a) resolve a Missing source and an Extra destination that share a
    /// unique fingerprint in the expected folder, and (b) reclassify a clean
    /// metadata pair whose bodies differ as changed. Every source and
    /// destination message keeps exactly one classification, which is
    /// re-validated before returning.
    pub fn detect_mismatches_with_content_fingerprints(
        job_id: &str,
        run_id: &str,
        source_messages: &ExtractedMessages,
        dest_messages: &ExtractedMessages,
        source_fingerprints: &HashMap<MailboxMessageKey, String>,
        dest_fingerprints: &HashMap<MailboxMessageKey, String>,
        folder_mapping: &HashMap<String, String>,
    ) -> Result<(Vec<MessageMismatch>, VerificationSummary), String> {
        for (side, messages, fingerprints) in [
            ("source", source_messages, source_fingerprints),
            ("destination", dest_messages, dest_fingerprints),
        ] {
            // Equal sizes plus one-way containment imply equal key sets.
            if messages.len() != fingerprints.len()
                || messages.keys().any(|key| !fingerprints.contains_key(key))
            {
                return Err(format!(
                    "content verification requires exact {side} message/fingerprint key coverage"
                ));
            }
        }
        let job_context: Arc<str> = Arc::from(job_id);
        let run_context: Arc<str> = Arc::from(run_id);
        let MetadataReconciliation {
            mut mismatches,
            mut summary,
            mut membership,
            clean_pairs,
        } = Self::reconcile_metadata(
            job_id,
            run_id,
            source_messages,
            dest_messages,
            folder_mapping,
        )?;

        // (a) A unique content fingerprint is a fallback identity when the
        // metadata pass could not pair a message. Only a Missing source and
        // an Extra destination in the expected folder are eligible; a
        // stronger classification (changed, duplicated, probable) is never
        // overridden.
        let source_by_fingerprint = unique_fingerprint_index(source_fingerprints);
        let dest_by_fingerprint = unique_fingerprint_index(dest_fingerprints);
        let mut fingerprint_pairs = source_by_fingerprint
            .iter()
            .filter_map(|(fingerprint, source_key)| {
                let source_key = source_messages.get_key_value(*source_key)?.0;
                let dest_key = dest_messages
                    .get_key_value(*dest_by_fingerprint.get(fingerprint)?)?
                    .0;
                (membership.missing_source.contains(source_key)
                    && membership.extra_destination.contains(dest_key)
                    && expected_destination_folder(source_key, folder_mapping)
                        == dest_key.mailbox.as_ref())
                .then_some((source_key, dest_key))
            })
            .collect::<Vec<_>>();
        fingerprint_pairs.sort();
        if !fingerprint_pairs.is_empty() {
            let source_index = build_uid_folder_index(source_messages);
            let dest_index = build_uid_folder_index(dest_messages);
            let resolved_source = fingerprint_pairs
                .iter()
                .map(|(source_key, _)| *source_key)
                .collect::<HashSet<_>>();
            let resolved_destination = fingerprint_pairs
                .iter()
                .map(|(_, dest_key)| *dest_key)
                .collect::<HashSet<_>>();
            mismatches.retain(|mismatch| match mismatch.mismatch_type {
                MismatchType::Missing => mismatch_source_key(mismatch, &source_index)
                    .is_none_or(|key| !resolved_source.contains(key)),
                MismatchType::Extra => mismatch_destination_key(mismatch, &dest_index)
                    .is_none_or(|key| !resolved_destination.contains(key)),
                _ => true,
            });
            for (source_key, dest_key) in fingerprint_pairs {
                membership.missing_source.remove(source_key);
                membership.extra_destination.remove(dest_key);
                membership.matched_source.insert(source_key);
                membership.matched_destination.insert(dest_key);
                summary.missing_count -= 1;
                summary.extra_count -= 1;
                summary.metadata_matches += 1;
            }
        }

        // (b) Content comparison of the reconciler's clean pairs only.
        let mut estimated_bytes = mismatches
            .iter()
            .map(estimated_mismatch_bytes)
            .sum::<usize>();
        if estimated_bytes > MAX_ESTIMATED_MISMATCH_DETAIL_BYTES {
            return Err(format!(
                "verification mismatch detail exceeds the estimated {}-byte evidence budget",
                MAX_ESTIMATED_MISMATCH_DETAIL_BYTES
            ));
        }
        let mut clean_pairs = clean_pairs;
        clean_pairs.sort();
        for (source_key, dest_key) in clean_pairs {
            let (Some(source_fingerprint), Some(destination_fingerprint)) = (
                source_fingerprints.get(source_key),
                dest_fingerprints.get(dest_key),
            ) else {
                return Err(
                    "content verification encountered a missing fingerprint after coverage validation"
                        .into(),
                );
            };
            if source_fingerprint == destination_fingerprint {
                continue;
            }
            let mut mismatch = make_mismatch(
                &job_context,
                &run_context,
                MismatchType::MessageIdOnly,
                Some(source_key),
                Some(dest_key),
                Some(&source_messages[source_key]),
                Some(&dest_messages[dest_key]),
            );
            mismatch.source_fingerprint = Some(source_fingerprint.clone());
            mismatch.destination_fingerprint = Some(destination_fingerprint.clone());
            append_mismatch_with_budget(&mut mismatches, mismatch, &mut estimated_bytes)?;
            membership.matched_source.remove(source_key);
            membership.matched_destination.remove(dest_key);
            membership.changed_source.insert(source_key);
            membership.changed_destination.insert(dest_key);
            summary.metadata_matches -= 1;
            summary.changed_count += 1;
        }

        validate_verification_summary(
            source_messages,
            dest_messages,
            &mismatches,
            &summary,
            &membership,
        )?;
        Ok((mismatches, summary))
    }
}
