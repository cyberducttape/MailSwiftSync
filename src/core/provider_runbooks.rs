use serde::{Deserialize, Serialize};

/// Provider-specific runbook for safe migration execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderRunbook {
    /// Version of the provider-authentication and operational guidance used to
    /// generate this runbook. Update when provider policy assumptions change.
    pub guidance_version: String,
    pub provider: String,
    pub pre_migration_checklist: Vec<RunbookStep>,
    pub during_migration_monitoring: Vec<RunbookStep>,
    pub post_migration_verification: Vec<RunbookStep>,
    pub known_issues: Vec<KnownIssue>,
    pub support_contact: String,
}

/// Version the compiled provider guidance so generated runbooks are auditable
/// when provider authentication policies change.
pub const PROVIDER_GUIDANCE_VERSION: &str = "2026-09";

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
        let normalize = |provider: &str| {
            provider
                .to_lowercase()
                .replace(['-', '_'], " ")
                .replace("microsoft365", "microsoft")
                .replace("office365", "microsoft")
                .replace(" 365", "")
        };
        let src = normalize(source_provider);
        let dst = normalize(dest_provider);
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
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
            provider: "Gmail → Microsoft 365".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Confirm Gmail IMAP access is permitted by the account or Google Workspace administrator".to_string(),
                    why: "Gmail IMAP availability and administration controls vary by account and Workspace policy; this runbook does not assume a timeless settings path".to_string(),
                    success_indicator: "The account's documented Gmail IMAP setting or Workspace control permits IMAP".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Configure Gmail OAuth 2.0 / XOAUTH2 credentials and an IMAP-scoped token".to_string(),
                    why: "OAuth is the preferred and default Gmail authentication path; Google Workspace third-party mail clients must use OAuth".to_string(),
                    success_indicator: "A current IMAP-scoped access token or configured refresh token is available".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "If OAuth is unavailable, create a Gmail app password only when Google offers it and account policy permits password IMAP".to_string(),
                    why: "App passwords are a conditional fallback, not a replacement for Workspace OAuth, and a regular account password must not be used".to_string(),
                    success_indicator: "The eligible app password is stored securely, or OAuth remains configured as the authentication method".to_string(),
                },
                RunbookStep {
                    step_number: 4,
                    action: "Verify destination Microsoft 365 quota and usage, including operational headroom".to_string(),
                    why: "Exchange Online capacity depends on the plan, mailbox type, licensing, archive configuration, and tenant settings; no universal quota threshold is safe".to_string(),
                    success_indicator: "Discovered destination capacity exceeds source usage plus documented migration headroom".to_string(),
                },
                RunbookStep {
                    step_number: 5,
                    action: "Disable auto-reply and message forwarding on destination mailbox".to_string(),
                    why: "Prevents mail loops during sync verification period".to_string(),
                    success_indicator: "Auto-reply and forwarding confirmed disabled".to_string(),
                },
                RunbookStep {
                    step_number: 6,
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
                    success_indicator: "Observed free capacity remains above the projected remaining transfer and operational headroom".to_string(),
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
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues".to_string(),
        }
    }

    fn gmail_to_fastmail() -> ProviderRunbook {
        ProviderRunbook {
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
            provider: "Gmail → Fastmail".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Confirm Gmail IMAP access is permitted by the account or Google Workspace administrator".to_string(),
                    why: "Gmail IMAP availability depends on the account and Workspace policy".to_string(),
                    success_indicator: "The account's documented Gmail IMAP setting or Workspace control permits IMAP".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Configure Gmail OAuth 2.0 / XOAUTH2 credentials and an IMAP-scoped token; use an app password only as an eligible fallback".to_string(),
                    why: "OAuth is preferred and required for Google Workspace third-party mail clients; regular account passwords are not supported".to_string(),
                    success_indicator: "OAuth token is ready, or an eligible app password is stored securely".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Verify Fastmail mailbox is active and IMAP-enabled".to_string(),
                    why: "Fastmail supports IMAP natively without special configuration"
                        .to_string(),
                    success_indicator: "IMAP access confirmed via manual test".to_string(),
                },
            ],
            during_migration_monitoring: vec![RunbookStep {
                step_number: 1,
                action: "Monitor Gmail connection limits".to_string(),
                why: "Gmail enforces per-account connection limits".to_string(),
                success_indicator: "No persistent connection errors".to_string(),
            }],
            post_migration_verification: vec![RunbookStep {
                step_number: 1,
                action: "Verify message counts match in MailSwiftSync report".to_string(),
                why: "Ensure all messages transferred correctly".to_string(),
                success_indicator: "Verification report shows success".to_string(),
            }],
            known_issues: vec![KnownIssue {
                issue: "Gmail IMAP connection limits".to_string(),
                workaround: "Reduce batch size and increase delay".to_string(),
                affected_versions: "All".to_string(),
            }],
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues"
                .to_string(),
        }
    }

    fn o365_to_gmail() -> ProviderRunbook {
        ProviderRunbook {
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
            provider: "Microsoft 365 → Gmail".to_string(),
            pre_migration_checklist: vec![
                RunbookStep {
                    step_number: 1,
                    action: "Enable IMAP for source O365 mailbox via Exchange admin center"
                        .to_string(),
                    why: "O365 IMAP is disabled by default for security".to_string(),
                    success_indicator: "IMAP shows enabled in mailbox settings".to_string(),
                },
                RunbookStep {
                    step_number: 2,
                    action: "Confirm Gmail IMAP access is permitted by the account or Google Workspace administrator"
                        .to_string(),
                    why: "Gmail IMAP availability depends on the account and Workspace policy".to_string(),
                    success_indicator: "The account's documented Gmail IMAP setting or Workspace control permits IMAP".to_string(),
                },
                RunbookStep {
                    step_number: 3,
                    action: "Configure Gmail OAuth 2.0 / XOAUTH2 credentials and an IMAP-scoped token; use an app password only as an eligible fallback".to_string(),
                    why: "OAuth is preferred and required for Google Workspace third-party mail clients; regular account passwords are not supported".to_string(),
                    success_indicator: "OAuth token is ready, or an eligible app password is stored securely".to_string(),
                },
            ],
            during_migration_monitoring: vec![RunbookStep {
                step_number: 1,
                action: "Monitor O365 throttling; reduce batch size if errors occur".to_string(),
                why: "The IMAP server may signal capacity or temporary throttling".to_string(),
                success_indicator: "No persistent throttling errors".to_string(),
            }],
            post_migration_verification: vec![RunbookStep {
                step_number: 1,
                action: "Verify MailSwiftSync report shows all messages transferred".to_string(),
                why: "Confirm migration completeness".to_string(),
                success_indicator: "Verification report passes".to_string(),
            }],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues"
                .to_string(),
        }
    }

    fn o365_to_fastmail() -> ProviderRunbook {
        ProviderRunbook {
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
            provider: "Microsoft 365 → Fastmail".to_string(),
            pre_migration_checklist: vec![RunbookStep {
                step_number: 1,
                action: "Enable IMAP for O365 mailbox in Exchange admin center".to_string(),
                why: "O365 IMAP is disabled by default".to_string(),
                success_indicator: "IMAP enabled".to_string(),
            }],
            during_migration_monitoring: vec![RunbookStep {
                step_number: 1,
                action: "Monitor O365 throttling".to_string(),
                why: "The IMAP server may signal capacity or temporary throttling".to_string(),
                success_indicator: "No throttling errors".to_string(),
            }],
            post_migration_verification: vec![RunbookStep {
                step_number: 1,
                action: "Verify message counts via MailSwiftSync".to_string(),
                why: "Confirm all messages transferred".to_string(),
                success_indicator: "Verification passes".to_string(),
            }],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues"
                .to_string(),
        }
    }

    fn fastmail_to_gmail() -> ProviderRunbook {
        ProviderRunbook {
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
            provider: "Fastmail → Gmail".to_string(),
            pre_migration_checklist: vec![RunbookStep {
                step_number: 1,
                action: "Enable Gmail IMAP access".to_string(),
                why: "Required for migration".to_string(),
                success_indicator: "IMAP enabled".to_string(),
            }],
            during_migration_monitoring: vec![],
            post_migration_verification: vec![RunbookStep {
                step_number: 1,
                action: "Verify message counts".to_string(),
                why: "Ensure completeness".to_string(),
                success_indicator: "Verification passes".to_string(),
            }],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues"
                .to_string(),
        }
    }

    fn fastmail_to_o365() -> ProviderRunbook {
        ProviderRunbook {
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
            provider: "Fastmail → Microsoft 365".to_string(),
            pre_migration_checklist: vec![RunbookStep {
                step_number: 1,
                action: "Verify O365 mailbox has sufficient quota".to_string(),
                why: "Prevent quota-full errors".to_string(),
                success_indicator: "Quota check passed".to_string(),
            }],
            during_migration_monitoring: vec![RunbookStep {
                step_number: 1,
                action: "Monitor O365 throttling".to_string(),
                why: "O365 enforces rate limits".to_string(),
                success_indicator: "No throttling errors".to_string(),
            }],
            post_migration_verification: vec![RunbookStep {
                step_number: 1,
                action: "Verify MailSwiftSync report".to_string(),
                why: "Confirm migration success".to_string(),
                success_indicator: "Report shows success".to_string(),
            }],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues"
                .to_string(),
        }
    }

    fn generic_imap() -> ProviderRunbook {
        ProviderRunbook {
            guidance_version: PROVIDER_GUIDANCE_VERSION.to_string(),
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
            during_migration_monitoring: vec![RunbookStep {
                step_number: 1,
                action: "Monitor for connection timeouts or errors".to_string(),
                why: "Generic IMAP providers vary in stability".to_string(),
                success_indicator: "No persistent connection errors".to_string(),
            }],
            post_migration_verification: vec![RunbookStep {
                step_number: 1,
                action: "Verify message counts via MailSwiftSync report".to_string(),
                why: "Confirm migration completeness".to_string(),
                success_indicator: "Verification report passes".to_string(),
            }],
            known_issues: vec![],
            support_contact: "GitHub Issues: https://github.com/cyberducttape/MailSwiftSync/issues"
                .to_string(),
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
        assert!(!runbook.pre_migration_checklist.is_empty());
    }

    #[test]
    fn normalizes_compact_microsoft365_provider_names() {
        let runbook = RunbookGenerator::generate("gmail", "microsoft365");
        assert_eq!(runbook.provider, "Gmail → Microsoft 365");
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

    #[test]
    fn gmail_guidance_is_versioned_and_oauth_first() {
        let runbook = RunbookGenerator::generate("Gmail", "O365");
        assert_eq!(runbook.guidance_version, PROVIDER_GUIDANCE_VERSION);

        let guidance = runbook
            .pre_migration_checklist
            .iter()
            .map(|step| format!("{} {}", step.action, step.why))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(guidance.contains("OAuth"));
        assert!(guidance.contains("conditional fallback"));
        assert!(guidance.contains("regular account password must not be used"));
        assert!(!guidance.contains("default is POP3 only"));
    }

    #[test]
    fn microsoft_quota_guidance_is_plan_aware() {
        let runbook = RunbookGenerator::generate("Gmail", "Microsoft 365");
        let quota_step = &runbook.pre_migration_checklist[3];
        assert!(quota_step.action.contains("quota and usage"));
        assert!(
            quota_step
                .success_indicator
                .contains("source usage plus documented migration headroom")
        );
        assert!(!quota_step.action.contains("100GB"));
        assert!(!quota_step.why.contains("default is 50GB"));
    }
}
