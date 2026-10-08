//! Migration plan validation, including certificate pins and folder mapping rules.

use super::*;

pub(crate) fn validate_certificate_pin(value: &str, label: &str) -> Result<(), String> {
    let pin = value.trim();
    if pin.is_empty() {
        return Ok(());
    }
    if pin.len() != 64 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!(
            "{label} must be a 64-character SHA-256 certificate fingerprint"
        ));
    }
    Ok(())
}

impl Form {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_internal(true)
    }

    /// Validate the complete executable plan and return the only type that
    /// can prepare an engine invocation.
    pub(crate) fn validated_plan(&self) -> Result<ValidatedPlan<'_>, String> {
        self.validate()?;
        Ok(ValidatedPlan { form: self })
    }

    pub(crate) fn validate_for_import(&self) -> Result<(), String> {
        self.validate_internal(false)
    }

    pub(crate) fn validate_internal(&self, require_credentials: bool) -> Result<(), String> {
        let mut required = vec![
            ("Source IMAP host", self.profile.source_host.as_str()),
            ("Source username", self.profile.source_user.as_str()),
            (
                "Destination IMAP host",
                self.profile.destination_host.as_str(),
            ),
            (
                "Destination username",
                self.profile.destination_user.as_str(),
            ),
        ];
        let source_automatic_refresh = auth_method_is_oauth(&self.profile.source_auth)
            && !self
                .profile
                .source_oauth_refresh_credential_id
                .trim()
                .is_empty();
        let destination_automatic_refresh = auth_method_is_oauth(&self.profile.destination_auth)
            && !self
                .profile
                .destination_oauth_refresh_credential_id
                .trim()
                .is_empty();
        if require_credentials && !source_automatic_refresh {
            required.push((
                if auth_method_is_oauth(&self.profile.source_auth) {
                    "Source OAuth 2.0 access token"
                } else {
                    "Source password"
                },
                self.source_password.as_str(),
            ));
        }
        if require_credentials
            && self.engine() != core::Engine::Dovecot
            && !destination_automatic_refresh
        {
            required.push((
                if auth_method_is_oauth(&self.profile.destination_auth) {
                    "Destination OAuth 2.0 access token"
                } else {
                    "Destination password"
                },
                self.destination_password.as_str(),
            ));
        }
        if self.engine() == core::Engine::Dovecot
            && (auth_method_is_oauth(&self.profile.source_auth)
                || auth_method_is_oauth(&self.profile.destination_auth))
        {
            return Err(
                "OAuth 2.0 authentication is currently supported for imapsync only; Dovecot native execution requires password authentication.".into(),
            );
        }
        for (label, method) in [
            ("Source authentication", self.profile.source_auth.as_str()),
            (
                "Destination authentication",
                self.profile.destination_auth.as_str(),
            ),
        ] {
            if !matches!(method, "" | "password" | "oauth2") {
                return Err(format!(
                    "{label} must be password or OAuth 2.0 access token."
                ));
            }
        }
        for (label, host, tls_mode, method) in [
            (
                "Source",
                self.profile.source_host.as_str(),
                self.profile.source_tls.as_str(),
                self.profile.source_auth.as_str(),
            ),
            (
                "Destination",
                self.profile.destination_host.as_str(),
                effective_destination_tls(&self.profile.destination_tls),
                self.profile.destination_auth.as_str(),
            ),
        ] {
            if is_microsoft_365_endpoint(host, tls_mode) && method != "oauth2" {
                return Err(format!(
                    "{label} Microsoft 365 IMAP requires OAuth 2.0 / Modern Authentication; Basic Authentication and app passwords are not supported"
                ));
            }
        }
        let source_port = self.profile.source_port.trim();
        if !source_port.is_empty() && source_port.parse::<u16>().map_or(true, |port| port == 0) {
            return Err("Source IMAP port must be a number between 1 and 65535.".into());
        }
        if !matches!(
            self.profile.source_tls.as_str(),
            "imaps" | "starttls" | "plain"
        ) {
            return Err("Source TLS mode must be imaps, starttls, or plain.".into());
        }
        let destination_port = self.profile.destination_port.trim();
        if !destination_port.is_empty()
            && destination_port
                .parse::<u16>()
                .map_or(true, |port| port == 0)
        {
            return Err("Destination IMAP port must be a number between 1 and 65535.".into());
        }
        if !matches!(
            self.profile.destination_tls.as_str(),
            "" | "imaps" | "starttls"
        ) {
            return Err("Destination TLS mode must be imaps or starttls.".into());
        }
        validate_certificate_pin(
            &self.profile.source_certificate_pin_sha256,
            "Source certificate pin",
        )?;
        validate_certificate_pin(
            &self.profile.destination_certificate_pin_sha256,
            "Destination certificate pin",
        )?;
        let has_certificate_pin = || {
            !self.profile.source_certificate_pin_sha256.trim().is_empty()
                || !self
                    .profile
                    .destination_certificate_pin_sha256
                    .trim()
                    .is_empty()
        };
        if has_certificate_pin() {
            return Err(match self.engine() {
                core::Engine::Dovecot => "Certificate pinning is not currently supported by the Dovecot engine; remove the pin or select an engine with transfer-level pin enforcement.".into(),
                core::Engine::ImapSync | core::Engine::Auto => "Certificate pins are verified by MailSwiftSync's probe but cannot currently be enforced by the qualified imapsync transfer; remove the pin until a pin-enforcing engine backend is qualified.".into(),
            });
        }
        endpoint::parts(
            &self.profile.source_host,
            default_imap_port(&self.profile.source_tls),
        )
        .map_err(|error| format!("Source IMAP host is not a valid endpoint: {error}"))?;
        endpoint::parts(
            &self.profile.destination_host,
            default_imap_port(effective_destination_tls(&self.profile.destination_tls)),
        )
        .map_err(|error| format!("Destination IMAP host is not a valid endpoint: {error}"))?;
        if !(1..=720).contains(&self.profile.migration_timeout_hours) {
            return Err("Migration timeout must be between 1 and 720 hours.".into());
        }
        for (label, value) in required {
            if value.trim().is_empty() {
                return Err(format!("{label} is required."));
            }
            if value.chars().any(char::is_control) {
                return Err(format!("{label} cannot contain control characters."));
            }
        }
        for (label, value) in [
            ("Source keyring ID", &self.profile.source_credential_id),
            (
                "Destination keyring ID",
                &self.profile.destination_credential_id,
            ),
        ] {
            let trimmed = value.trim();
            if trimmed.chars().any(char::is_control) || trimmed.len() > 256 {
                return Err(format!(
                    "{label} must not contain control characters and must be at most 256 bytes."
                ));
            }
        }
        for (label, value) in [
            (
                "Source provider tenant scope",
                &self.profile.source_rate_tenant,
            ),
            (
                "Destination provider tenant scope",
                &self.profile.destination_rate_tenant,
            ),
        ] {
            if value.len() > 256 || value.chars().any(char::is_control) {
                return Err(format!(
                    "{label} must not contain control characters and must be at most 256 bytes."
                ));
            }
        }
        self.validate_folder_mapping_rules()?;
        if require_credentials && self.requires_insecure_transport_ack() {
            return Err(
                "Plain IMAP requires an explicit cleartext-transport acknowledgement before any authenticated operation, including dry preflight.".into(),
            );
        }
        for (label, value) in [
            ("Source IMAP host", self.profile.source_host.as_str()),
            (
                "Destination IMAP host",
                self.profile.destination_host.as_str(),
            ),
            ("Source username", self.profile.source_user.as_str()),
            (
                "Destination username",
                self.profile.destination_user.as_str(),
            ),
            ("Source credential", self.source_password.as_str()),
            ("Destination credential", self.destination_password.as_str()),
        ] {
            if !value.is_empty() && value.chars().any(char::is_control) {
                return Err(format!("{label} cannot contain control characters."));
            }
        }
        self.extra_options_valid()?;
        Ok(())
    }

    pub(crate) fn extra_options_valid(&self) -> Result<(), String> {
        engine::validate_extra_options(&self.profile.extra_options)
    }

    pub(super) fn validate_folder_mapping_rules(&self) -> Result<(), String> {
        if self.profile.folder_mapping_rules.len() > 256 {
            return Err("Folder mapping rules cannot exceed 256 entries.".into());
        }
        let mut seen_sources = std::collections::HashSet::new();
        let mut destination_sources = std::collections::HashMap::new();
        for rule in &self.profile.folder_mapping_rules {
            let source = &rule.source;
            let destination = (!rule.exclude).then_some(&rule.destination);
            if source.trim().is_empty() || destination.is_some_and(|value| value.trim().is_empty())
            {
                return Err("Folder mapping source and destination are required.".into());
            }
            if source.chars().any(char::is_control)
                || destination.is_some_and(|value| value.chars().any(char::is_control))
            {
                return Err("Folder mapping names cannot contain control characters.".into());
            }
            if source.contains('=') || destination.is_some_and(|value| value.contains('=')) {
                return Err("Folder mapping names cannot contain '='.".into());
            }
            if !seen_sources.insert(source.clone()) {
                return Err(format!(
                    "Folder mapping source is specified more than once: {}",
                    source
                ));
            }
            if let Some(destination) = destination {
                let collision_key = crate::ui::fold_search_text(destination);
                if let Some(previous_source) = destination_sources.get(&collision_key) {
                    return Err(format!(
                        "Folder mapping destination targets collide after case folding: {previous_source} and {source} both map to {destination}. Choose distinct destination folders to avoid an unintended merge."
                    ));
                }
                destination_sources.insert(collision_key, source.as_str());
            }
        }
        Ok(())
    }

    pub(crate) fn requires_insecure_transport_ack(&self) -> bool {
        self.profile.source_tls == "plain" && !self.profile.allow_insecure_source_transport
    }
}
