//! Ledger backup/restore, status, support bundles, completions, and webhook notification commands.

use super::*;

pub(super) fn deliver_webhook_once(
    state: &std::path::Path,
    url: &str,
    project_id: Option<&str>,
    include_customer_metadata: bool,
) -> Result<(u32, u32), String> {
    let summary = headless_status_summary(state, project_id)
        .map_err(|error| format!("could not read migration status: {error}"))?;
    let payload = serde_json::to_value(summary)
        .map_err(|error| format!("could not serialize migration status: {error}"))?;
    let payload = if include_customer_metadata {
        payload
    } else {
        webhook::minimal_status_payload(payload)
    };
    let body = serde_json::to_string(&payload)
        .map_err(|error| format!("could not serialize migration status: {error}"))?;
    let endpoint_digest =
        webhook::endpoint_digest(url).map_err(|error| format!("invalid endpoint: {error}"))?;
    let store = core::StateStore::open(state)
        .map_err(|error| format!("durable outbox is unavailable: {error}"))?;
    let project_scope = project_id.unwrap_or("all-projects");
    let current_event_id = webhook::snapshot_event_id(&endpoint_digest, project_scope, &body);
    store
        .enqueue_webhook_delivery(
            &current_event_id,
            project_scope,
            "migration.status_snapshot",
            &body,
            &endpoint_digest,
        )
        .map_err(|error| format!("could not queue durable delivery: {error}"))?;
    store
        .bind_unbound_webhook_deliveries(&endpoint_digest, project_id)
        .map_err(|error| format!("could not bind lifecycle events to endpoint: {error}"))?;
    let lease_owner = uuid::Uuid::new_v4().to_string();
    let mut delivered = 0_u32;
    let mut failed = 0_u32;
    for _ in 0..100 {
        let delivery = store
            .claim_webhook_deliveries(&endpoint_digest, project_id, &lease_owner, 1)
            .map_err(|error| format!("could not claim durable delivery queue: {error}"))?;
        let Some(delivery) = delivery.into_iter().next() else {
            break;
        };
        match webhook::post_json(
            url,
            &delivery.event_id,
            &delivery.event_type,
            &delivery.payload,
        ) {
            Ok(status_code) if (200..300).contains(&status_code) => {
                store
                    .mark_webhook_delivered(&delivery.event_id, &lease_owner)
                    .map_err(|error| {
                        format!("durability failed after HTTP {status_code}: {error}")
                    })?;
                delivered = delivered.saturating_add(1);
            }
            Ok(status_code) => {
                let error = format!("endpoint rejected delivery with HTTP {status_code}");
                store
                    .mark_webhook_failed(&delivery.event_id, &lease_owner, &error)
                    .map_err(|mark_error| {
                        format!("delivery failure could not be durably recorded: {mark_error}")
                    })?;
                failed = failed.saturating_add(1);
            }
            Err(error) => {
                store
                    .mark_webhook_failed(&delivery.event_id, &lease_owner, &error)
                    .map_err(|mark_error| {
                        format!("delivery failure could not be durably recorded: {mark_error}")
                    })?;
                failed = failed.saturating_add(1);
            }
        }
    }
    Ok((delivered, failed))
}

pub(super) fn completions_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let shell = arguments.next();
    let script = shell
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
        .and_then(crate::completions::script);
    match (script, arguments.next()) {
        (Some(script), None) => {
            out_raw(&script);
            Ok(())
        }
        _ => {
            eprintln!(
                "Usage: mailswiftsync completions {}",
                crate::completions::SHELLS.join("|")
            );
            std::process::exit(2);
        }
    }
}

pub(super) fn backup_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(source), Some(destination)) = (arguments.next(), arguments.next()) else {
        eprintln!("Usage: mailswiftsync backup <state.db> <backup.db>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync backup <state.db> <backup.db>");
        std::process::exit(2);
    }
    let source = std::path::PathBuf::from(source);
    let destination = std::path::PathBuf::from(destination);
    let _lock = match acquire_instance_lock(&source) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("Ledger backup refused: {error}");
            std::process::exit(1);
        }
    };
    match core::StateStore::open_readonly(&source).and_then(|store| store.backup_to(&destination)) {
        Ok(()) => {
            out!("Created verified ledger backup: {}", destination.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("Ledger backup failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn restore_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(backup), Some(destination)) = (arguments.next(), arguments.next()) else {
        eprintln!("Usage: mailswiftsync restore <backup.db> <state.db>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync restore <backup.db> <state.db>");
        std::process::exit(2);
    }
    let backup = std::path::PathBuf::from(backup);
    let destination = std::path::PathBuf::from(destination);
    let _lock = match acquire_instance_lock(&destination) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("Ledger restore refused: {error}");
            std::process::exit(1);
        }
    };
    match restore_ledger(&backup, &destination) {
        Ok(Some(previous)) => {
            out!(
                "Restored verified ledger to {}; previous ledger preserved at {}.",
                destination.display(),
                previous.display()
            );
            Ok(())
        }
        Ok(None) => {
            out!("Restored verified ledger to {}.", destination.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("Ledger restore failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn status_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let Some(state) = arguments.next() else {
        eprintln!("Usage: mailswiftsync status <state.db> [project-id] [--summary]");
        std::process::exit(2);
    };
    let mut project_id = None;
    let mut summary = false;
    for argument in arguments {
        if argument == std::ffi::OsStr::new("--summary") && !summary {
            summary = true;
        } else if project_id.is_none() {
            project_id = Some(argument);
        } else {
            eprintln!("Usage: mailswiftsync status <state.db> [project-id] [--summary]");
            std::process::exit(2);
        }
    }
    let state = std::path::PathBuf::from(state);
    let project_id = match project_id.as_deref() {
        Some(value) => match value.to_str() {
            Some(value) => Some(value),
            None => {
                eprintln!("Status refused: project ID must be valid UTF-8");
                std::process::exit(2);
            }
        },
        None => None,
    };
    let result = if summary {
        headless_status_summary(&state, project_id).and_then(|status| {
            serde_json::to_string_pretty(&status)
                .map_err(|error| format!("could not serialize migration status: {error}"))
        })
    } else {
        headless_status(&state, project_id).and_then(|status| {
            serde_json::to_string_pretty(&status)
                .map_err(|error| format!("could not serialize migration status: {error}"))
        })
    };
    match result {
        Ok(status) => {
            out!("{status}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Could not read migration status: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn fleet_status_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let Some(root) = arguments.next() else {
        eprintln!("Usage: mailswiftsync fleet-status <directory>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync fleet-status <directory>");
        std::process::exit(2);
    }
    let root = std::path::PathBuf::from(root);
    match fleet_status(&root).and_then(|status| {
        serde_json::to_string_pretty(&status)
            .map_err(|error| format!("could not serialize fleet status: {error}"))
    }) {
        Ok(status) => {
            out!("{status}");
            Ok(())
        }
        Err(error) => {
            eprintln!("Could not read fleet status: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn support_bundle_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(state), Some(output)) = (arguments.next(), arguments.next()) else {
        eprintln!("Usage: mailswiftsync support-bundle <state.db> <output.json>");
        std::process::exit(2);
    };
    if arguments.next().is_some() {
        eprintln!("Usage: mailswiftsync support-bundle <state.db> <output.json>");
        std::process::exit(2);
    }
    let state = std::path::PathBuf::from(state);
    let output = std::path::PathBuf::from(output);
    let _lock = match acquire_instance_lock(&state) {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("Support-bundle export refused: {error}");
            std::process::exit(1);
        }
    };
    match export_support_bundle(&state, &output) {
        Ok(()) => {
            out!("Created sanitized support bundle: {}", output.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("Support-bundle export failed: {error}");
            std::process::exit(1);
        }
    }
}

pub(super) fn notify_webhook_command(mut arguments: std::env::ArgsOs) -> eframe::Result<()> {
    let (Some(state), Some(url)) = (arguments.next(), arguments.next()) else {
        eprintln!("{WEBHOOK_USAGE}");
        std::process::exit(2);
    };
    let mut project_id = None;
    let mut include_customer_metadata = false;
    let mut watch = false;
    let mut poll_seconds = None;
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        if argument == std::ffi::OsStr::new("--include-customer-metadata") {
            if include_customer_metadata {
                eprintln!("Webhook notification option was supplied more than once");
                std::process::exit(2);
            }
            include_customer_metadata = true;
        } else if argument == std::ffi::OsStr::new("--watch") {
            if watch {
                eprintln!("Webhook notification option was supplied more than once");
                std::process::exit(2);
            }
            watch = true;
        } else if argument == std::ffi::OsStr::new("--poll-seconds") {
            let Some(value) = arguments.next() else {
                eprintln!("{WEBHOOK_USAGE}");
                std::process::exit(2);
            };
            if poll_seconds.is_some() {
                eprintln!("Webhook poll interval was supplied more than once");
                std::process::exit(2);
            }
            poll_seconds = Some(
                match value.to_str().and_then(|value| value.parse::<u64>().ok()) {
                    Some(value) if (1..=3_600).contains(&value) => value,
                    _ => {
                        eprintln!("{WEBHOOK_USAGE}");
                        std::process::exit(2);
                    }
                },
            );
        } else if let Some(value) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--poll-seconds="))
        {
            if poll_seconds.is_some() {
                eprintln!("Webhook poll interval was supplied more than once");
                std::process::exit(2);
            }
            poll_seconds = Some(match value.parse::<u64>() {
                Ok(value) if (1..=3_600).contains(&value) => value,
                _ => {
                    eprintln!("{WEBHOOK_USAGE}");
                    std::process::exit(2);
                }
            });
        } else if project_id.is_none() {
            project_id = Some(argument);
        } else {
            eprintln!("{WEBHOOK_USAGE}");
            std::process::exit(2);
        }
    }
    let url = match url.to_str() {
        Some(url) => url.to_owned(),
        None => {
            eprintln!("Webhook notification refused: URL must be valid UTF-8");
            std::process::exit(2);
        }
    };
    let state = std::path::PathBuf::from(state);
    let project_id = match project_id.as_deref() {
        Some(value) => match value.to_str() {
            Some(value) => Some(value),
            None => {
                eprintln!("Webhook notification refused: project ID must be valid UTF-8");
                std::process::exit(2);
            }
        },
        None => None,
    };
    let poll_seconds = poll_seconds.unwrap_or(30);
    loop {
        match deliver_webhook_once(&state, &url, project_id, include_customer_metadata) {
            Ok((delivered, 0)) => {
                if !watch {
                    out!("Webhook delivery queue processed: {delivered} event(s) delivered.");
                    return Ok(());
                }
                if delivered > 0 {
                    eprintln!("Webhook worker delivered {delivered} event(s).");
                }
            }
            Ok((_, failed)) => {
                if !watch {
                    eprintln!(
                        "Webhook delivery failed for {failed} event(s); retry state is durable."
                    );
                    std::process::exit(1);
                }
                eprintln!(
                    "Webhook worker recorded {failed} failed delivery attempt(s); retry state is durable."
                );
            }
            Err(error) => {
                eprintln!("Webhook worker stopped: {error}");
                std::process::exit(1);
            }
        }
        std::thread::sleep(Duration::from_secs(poll_seconds));
    }
}
