use serde::{Deserialize, Serialize};

/// Provider-specific runbook for safe migration execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRunbook {
    pub provider: String,
    pub pre_migration_checklist: Vec<RunbookStep>,
    pub during_migration_monitoring: Vec<RunbookStep>,
    pub post_migration_verification: Vec<RunbookStep>,
    pub known_issues: Vec<KnownIssue>,
    pub support_contact: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunbookStep {
    pub step_number: u32,
    pub action: String,
    pub why: String,
    pub success_indicator: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownIssue {
    pub issue: String,
    pub workaround: String,
    pub affected_versions: String,
}

/// Generate runbooks based on source and destination providers.
pub struct RunbookGenerator;

impl RunbookGenerator {
    /// Generate a runbook for the given provider combination.
    pub fn generate(source_provider: &str, dest_provider: &str) -> ProviderRunbook {
        let src = source_provider.to_lowercase().replace(" 365", "");
        let dst = dest_provider.to_lowercase().replace(" 365", "");
        let src_normalized = src.trim();
        let dst_normalized = dst.trim();

        match (src_normalized, dst_normalized) {
            ("gmail", "microsoft" | "o365" | "office365") => Self::gmail_to_o365(),
            ("gmail", "fastmail") => Self::gmail_to_fastmail(),
            ("microsoft" | "o365" | "office365", "gmail") => Self::o365_to_gmail(),
            ("microsoft" | "o365" | "office365", "fastmail") => Self::o365_to_fastmail(),
            ("fastmail", "gmail") => Self::fastmail_to_gmail(),
            ("fastmail", "microsoft" | "o365" | "office365") => Self::fastmail_to_o365(),
            _ => Self::generic_imap(),
        }
    }

    fn gmail_to_o365() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Gmail → Microsoft 365".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Enable IMAP in Gmail account settings (Settings > Forwarding and POP/IMAP > Enable IMAP)".to_string(),
                    why: "Gmail requires explicit IMAP enablement; default is POP3 only".to_string(),
                    success_indicator: "IMAP shows as enabled in account settings".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Create app-specific password for the Gmail account (if 2FA enabled)".to_string(),
                    why: "Gmail blocks standard password auth when 2FA is active; app passwords bypass this".to_string(),
                    success_indicator: "App password generated and stored securely".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Verify destination O365 mailbox has 100GB+ available quota".to_string(),
                    why: "O365 default quota is 50GB; Gmail accounts often exceed this".to_string(),
                    success_indicator: "Get-Mailbox shows available quota > 100GB".to_string(),
                },
                RunbookStep {
                    step_number: 4,
                    action: "Disable auto-reply and message forwarding on destination mailbox".to_string(),
                    why: "Prevents mail loops during sync verification period".to_string(),
                    success_indicator: "Auto-reply and forwarding confirmed disabled".to_string(),
                },
                RunbookStep {
                    step_number: 5,
                    action: "Create mail forwarding rule from source Gmail to destination O365 (optional, for new mail)".to_string(),
                    why: "Ensures new mail arrives at destination during migration window".to_string(),
                    success_indicator: "Gmail forwarding rule created and working".to_string(),
                },
            ],
            during_migration_monitoring: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Monitor for '421 Too Many Connections' errors from Gmail".to_string(),
                    why: "Gmail limits concurrent connections per account; imapsync may exceed this".to_string(),
                    success_indicator: "No persistent connection failures; backoff clears errors".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Watch O365 resource health dashboard for Exchange throttling alerts".to_string(),
                    why: "O365 enforces soft/hard throttling limits on import operations".to_string(),
                    success_indicator: "No throttling alerts; migration proceeds at normal pace".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Check destination mailbox quota every 30 minutes for large migrations".to_string(),
                    why: "Prevents quota-full interruptions mid-migration".to_string(),
                    success_indicator: "Quota remains > 20GB free throughout migration".to_string(),
                },
            ],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify folder structure matches source Gmail labels".to_string(),
                    why: "Gmail labels don't map 1:1 to IMAP folders; manual mapping may be needed".to_string(),
                    success_indicator: "All expected folders present in destination".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Check Gmail 'All Mail' duplication in destination".to_string(),
                    why: "Gmail's 'All Mail' includes labeled messages; may appear as duplicates".to_string(),
                    success_indicator: "No unintended duplicates in destination".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Verify message counts match via MailSwiftSync verification report".to_string(),
                    why: "Ensures all messages transferred correctly".to_string(),
                    success_indicator: "Verification report shows exact match or accepted exceptions only".to_string(),
                },
            ],
            known_issues: vec![
                KnownIssue {
                    issue: "Gmail IMAP connection quota exceeded".to_string(),
                    workaround: "Reduce imapsync --maxbatchsize; increase --sleepdelay between operations".to_string(),
                    affected_versions: "All versions; inherent Gmail API limitation".to_string(),
                },
                KnownIssue {
                    issue: "O365 soft throttling slows migration significantly".to_string(),
                    workaround: "Use smaller batch sizes and increase delay; consider multiple parallel jobs with folder-level splits".to_string(),
                    affected_versions: "All versions; inherent O365 API limitation".to_string(),
                },
            ],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }

    fn gmail_to_fastmail() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Gmail → Fastmail".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Enable IMAP in Gmail settings".to_string(),
                    why: "Required for IMAP access".to_string(),
                    success_indicator: "IMAP enabled in Gmail".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Generate Gmail app-specific password if 2FA is enabled".to_string(),
                    why: "Gmail blocks standard passwords with 2FA".to_string(),
                    success_indicator: "App password created".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Verify Fastmail mailbox is active and IMAP-enabled".to_string(),
                    why: "Fastmail supports IMAP natively without special configuration".to_string(),
                    success_indicator: "IMAP access confirmed via manual test".to_string(),
                },
            ],
            during_migration_monitoring: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Monitor Gmail connection limits".to_string(),
                    why: "Gmail enforces per-account connection limits".to_string(),
                    success_indicator: "No persistent connection errors".to_string(),
                },
            ],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify message counts match in MailSwiftSync report".to_string(),
                    why: "Ensure all messages transferred correctly".to_string(),
                    success_indicator: "Verification report shows success".to_string(),
                },
            ],
            known_issues: vec![
                KnownIssue {
                    issue: "Gmail IMAP connection limits".to_string(),
                    workaround: "Reduce batch size and increase delay".to_string(),
                    affected_versions: "All".to_string(),
                },
            ],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }

    fn o365_to_gmail() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Microsoft 365 → Gmail".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Enable IMAP for source O365 mailbox via Exchange admin center".to_string(),
                    why: "O365 IMAP is disabled by default for security".to_string(),
                    success_indicator: "IMAP shows enabled in mailbox settings".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Enable Gmail to accept IMAP connections (IMAP settings in Gmail)".to_string(),
                    why: "Gmail requires explicit IMAP enablement".to_string(),
                    success_indicator: "IMAP enabled in Gmail account settings".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Create Gmail app-specific password if destination has 2FA".to_string(),
                    why: "Gmail blocks standard passwords with 2FA enabled".to_string(),
                    success_indicator: "App password created and working".to_string(),
                },
            ],
            during_migration_monitoring: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Monitor O365 throttling; reduce batch size if errors occur".to_string(),
                    why: "O365 enforces strict rate limiting".to_string(),
                    success_indicator: "No persistent throttling errors".to_string(),
                },
            ],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify MailSwiftSync report shows all messages transferred".to_string(),
                    why: "Confirm migration completeness".to_string(),
                    success_indicator: "Verification report passes".to_string(),
                },
            ],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }

    fn o365_to_fastmail() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Microsoft 365 → Fastmail".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Enable IMAP for O365 mailbox in Exchange admin center".to_string(),
                    why: "O365 IMAP is disabled by default".to_string(),
                    success_indicator: "IMAP enabled".to_string(),
                },
            ],
            during_migration_monitoring: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Monitor O365 throttling".to_string(),
                    why: "O365 has strict rate limits".to_string(),
                    success_indicator: "No throttling errors".to_string(),
                },
            ],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify message counts via MailSwiftSync".to_string(),
                    why: "Confirm all messages transferred".to_string(),
                    success_indicator: "Verification passes".to_string(),
                },
            ],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }

    fn fastmail_to_gmail() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Fastmail → Gmail".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Enable Gmail IMAP access".to_string(),
                    why: "Required for migration".to_string(),
                    success_indicator: "IMAP enabled".to_string(),
                },
            ],
            during_migration_monitoring: vec![],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify message counts".to_string(),
                    why: "Ensure completeness".to_string(),
                    success_indicator: "Verification passes".to_string(),
                },
            ],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }

    fn fastmail_to_o365() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Fastmail → Microsoft 365".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify O365 mailbox has sufficient quota".to_string(),
                    why: "Prevent quota-full errors".to_string(),
                    success_indicator: "Quota check passed".to_string(),
                },
            ],
            during_migration_monitoring: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Monitor O365 throttling".to_string(),
                    why: "O365 enforces rate limits".to_string(),
                    success_indicator: "No throttling errors".to_string(),
                },
            ],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify MailSwiftSync report".to_string(),
                    why: "Confirm migration success".to_string(),
                    success_indicator: "Report shows success".to_string(),
                },
            ],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }

    fn generic_imap() -> ProviderRunbook {
        ProviderRunbook {
            provider: "Generic IMAP".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify IMAP is enabled on both source and destination".to_string(),
                    why: "Required for IMAP-based migration".to_string(),
                    success_indicator: "IMAP connection successful".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Test credentials manually before starting migration".to_string(),
                    why: "Catch authentication issues early".to_string(),
                    success_indicator: "Test connection successful".to_string(),
                },
            ],
            during_migration_monitoring: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Monitor for connection timeouts or errors".to_string(),
                    why: "Generic IMAP providers vary in stability".to_string(),
                    success_indicator: "No persistent connection errors".to_string(),
                },
            ],
            post_migration_verification: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Verify message counts via MailSwiftSync report".to_string(),
                    why: "Confirm migration completeness".to_string(),
                    success_indicator: "Verification report passes".to_string(),
                },
            ],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/itchyitchy123/MailSwiftSync/issues".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_gmail_to_o365_runbook() {
        let runbook = RunbookGenerator::generate("Gmail", "Microsoft 365");
        assert_eq!(runbook.provider, "Gmail → Microsoft 365");
        assert!(!runbook.pre_migration_checklist.is_empty());
        assert!(!runbook.known_issues.is_empty());
    }

    #[test]
    fn generates_o365_to_gmail_runbook() {
        let runbook = RunbookGenerator::generate("O365", "gmail");
        assert_eq!(runbook.provider, "Microsoft 365 → Gmail");
        assert!(runbook.pre_migration_checklist.len() > 0);
    }

    #[test]
    fn generates_generic_imap_runbook() {
        let runbook = RunbookGenerator::generate("unknown1", "unknown2");
        assert_eq!(runbook.provider, "Generic IMAP");
    }

    #[test]
    fn runbook_steps_have_rationale() {
        let runbook = RunbookGenerator::generate("Gmail", "O365");
        for step in &runbook.pre_migration_checklist {
            assert!(!step.why.is_empty());
            assert!(!step.success_indicator.is_empty());
        }
    }
}
