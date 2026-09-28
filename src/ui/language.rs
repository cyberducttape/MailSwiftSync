//! UI language selection and the initial built-in German interface catalog.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum UiLanguage {
    #[default]
    English,
    German,
}

impl UiLanguage {
    pub(crate) fn all() -> &'static [Self] {
        &[Self::English, Self::German]
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::German => "Deutsch (teilweise)",
        }
    }

    pub(crate) fn text(self, source: &'static str) -> &'static str {
        if self == Self::English {
            return source;
        }
        match source {
            "Overview" => "Übersicht",
            "Plan" => "Plan",
            "Mailboxes" => "Postfächer",
            "Activity" => "Aktivität",
            "Verification" => "Verifizierung",
            "Projects" => "Projekte",
            "Stop" => "Anhalten",
            "Durable view is stale" => "Dauerhafte Ansicht ist nicht aktuell",
            "Migration plan" => "Migrationsplan",
            "Configure endpoints and credentials before running a dry preflight." => {
                "Konfigurieren Sie Endpunkte und Zugangsdaten, bevor Sie eine Vorabprüfung ausführen."
            }
            "Plan name" => "Planname",
            "Source" => "Quelle",
            "Destination" => "Ziel",
            "Source account" => "Quellkonto",
            "Destination account" => "Zielkonto",
            "Engine" => "Engine",
            "imapsync" => "imapsync",
            "Dovecot" => "Dovecot",
            "Dry run / preflight" => "Probelauf / Vorabprüfung",
            "Save / create project" => "Speichern / Projekt erstellen",
            "Save profile" => "Profil speichern",
            "Assess plan" => "Plan bewerten",
            "Run preflight" => "Vorabprüfung ausführen",
            "Preview command" => "Befehl anzeigen",
            "Advanced" => "Erweitert",
            "Start live migration" => "Live-Migration starten",
            "Confirm live migration" => "Live-Migration bestätigen",
            "Destination changes require confirmation" => {
                "Änderungen am Ziel erfordern eine Bestätigung"
            }
            "Source mail: not deleted by default" => {
                "Quellnachrichten werden standardmäßig nicht gelöscht"
            }
            "Destination deletion: ENABLED ⚠" => "Löschen am Ziel: AKTIVIERT ⚠",
            "Destination deletion: disabled" => "Löschen am Ziel: deaktiviert",
            "Cancel" => "Abbrechen",
            "I understand — start migration" => "Verstanden — Migration starten",
            "Execution plan" => "Ausführungsplan",
            "Advanced migration options" => "Erweiterte Migrationsoptionen",
            "Passwords are redacted. This is an argument list for review, not a shell command to paste." => {
                "Passwörter werden ausgeblendet. Dies ist eine Argumentliste zur Prüfung, kein Shell-Befehl zum Einfügen."
            }
            "Fix the advanced options above before this plan can run." => {
                "Korrigieren Sie die erweiterten Optionen, bevor dieser Plan ausgeführt werden kann."
            }
            "These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules." => {
                "Diese Einstellungen betreffen den imapsync-Fallback. Native Dovecot-Migrationen verwenden doveadm und serverseitige Konsistenzregeln."
            }
            "Reliability and metadata" => "Zuverlässigkeit und Metadaten",
            "Sync internal dates  (--syncinternaldates)" => {
                "Interne Datumswerte synchronisieren  (--syncinternaldates)"
            }
            "Use message UIDs when available  (--useuid)" => {
                "Nachrichten-UIDs verwenden, sofern verfügbar  (--useuid)"
            }
            "Use imapsync cache  (--usecache)" => "imapsync-Cache verwenden  (--usecache)",
            "Allow message-size mismatch  (--allowsizemismatch)" => {
                "Abweichende Nachrichtengröße erlauben  (--allowsizemismatch)"
            }
            "Performance" => "Leistung",
            "Fast I/O for source  (--fastio1)" => "Schnelle I/O für die Quelle  (--fastio1)",
            "Uses imapsync's faster source I/O path; test this with the provider before a production cutover." => {
                "Verwendet den schnelleren I/O-Pfad der Quelle; testen Sie dies vor einer Produktivumschaltung mit dem Anbieter."
            }
            "Fast I/O for destination  (--fastio2)" => "Schnelle I/O für das Ziel  (--fastio2)",
            "Uses imapsync's faster destination I/O path; provider behavior varies." => {
                "Verwendet den schnelleren I/O-Pfad des Ziels; das Verhalten hängt vom Anbieter ab."
            }
            "Messages/second target (0 = unlimited)" => "Nachrichten/Sekunde-Ziel (0 = unbegrenzt)",
            "Bytes/second target (0 = unlimited)" => "Bytes/Sekunde-Ziel (0 = unbegrenzt)",
            "For a batch this is an aggregate target: MailSwiftSync divides it across concurrent imapsync workers. A single run uses the value unchanged." => {
                "Für einen Stapel ist dies ein Gesamtziel: MailSwiftSync teilt es auf parallele imapsync-Prozesse auf. Ein Einzelvorgang verwendet den Wert unverändert."
            }
            "Process timeout (hours)" => "Prozesszeitlimit (Stunden)",
            "Maximum wall-clock time for one engine process. It is a safety bound, not an estimate of completion time." => {
                "Maximale Echtzeitdauer eines Engine-Prozesses. Dies ist eine Sicherheitsgrenze, keine Schätzung der Fertigstellungszeit."
            }
            "Batch targets are divided across workers and process starts are globally paced; provider-side limits still take precedence. A finite target must be at least the worker count." => {
                "Stapelziele werden auf Prozesse verteilt und Prozessstarts global getaktet; Anbietergrenzen haben weiterhin Vorrang. Ein begrenztes Ziel muss mindestens der Prozessanzahl entsprechen."
            }
            "Dovecot migration strategy" => "Dovecot-Migrationsstrategie",
            "Dovecot has no MailSwiftSync throttle; source load may be high. Initial backup may fail if the target Maildir refuses INBOX replacement. Test the exact destination storage before cutover; repeat final passes after exit code 2." => {
                "Dovecot hat keine MailSwiftSync-Drosselung; die Quellenlast kann hoch sein. Die erste Sicherung kann fehlschlagen, wenn das Ziel-Maildir den INBOX-Ersatz ablehnt. Testen Sie den exakten Zielspeicher vor der Umschaltung und wiederholen Sie Abschlussläufe nach Exit-Code 2."
            }
            "Destructive destination option" => "Destruktive Zieloption",
            "Delete destination messages missing from source  (--delete2)" => {
                "Zielnachrichten löschen, die in der Quelle fehlen  (--delete2)"
            }
            "Use only for an intentionally exact backup after a tested preflight. This can remove destination mail." => {
                "Nur für eine ausdrücklich exakte Sicherung nach getesteter Vorabprüfung verwenden. Dadurch können Nachrichten am Ziel gelöscht werden."
            }
            "Advanced plan settings are locked while a migration is running." => {
                "Erweiterte Planeinstellungen sind während einer laufenden Migration gesperrt."
            }
            "The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected." => {
                "Das Feld für zusätzliche imapsync-Optionen akzeptiert nur die dokumentierte Liste sicherer Tuning- und Diagnoseoptionen. Verbindungs-, Zugangsdaten-, TLS-, destruktive, Protokollierungs- und unbekannte Optionen werden abgelehnt."
            }
            "Review verification" => "Verifizierung prüfen",
            "Confirm live batch migration" => "Live-Stapel-Migration bestätigen",
            "This will change destination mailboxes" => "Dies ändert Zielpostfächer",
            "I understand — start batch" => "Verstanden — Stapel starten",
            "Sample of selected mailboxes:" => "Beispiel der ausgewählten Postfächer:",
            "View all selected" => "Alle ausgewählten anzeigen",
            "{} eligible of {} explicitly selected · {} blocked" => {
                "{} berechtigt von {} ausdrücklich ausgewählt · {} blockiert"
            }
            "Selected scope: {} explicit · {} visible · {} hidden by current filter" => {
                "Auswahlumfang: {} ausdrücklich · {} sichtbar · {} durch aktuellen Filter verborgen"
            }
            "Worker concurrency: {}" => "Gleichzeitige Arbeitsprozesse: {}",
            "Destination deletion: {}" => "Löschen am Ziel: {}",
            "ENABLED ⚠" => "AKTIVIERT ⚠",
            "disabled" => "deaktiviert",
            "Each mailbox must already have a matching successful preflight. Source mail is not deleted by default." => {
                "Für jedes Postfach muss bereits eine passende erfolgreiche Vorabprüfung vorliegen. Quellnachrichten werden standardmäßig nicht gelöscht."
            }
            "Review the queue, concurrency, throttles, and exact plans before continuing." => {
                "Prüfen Sie Warteschlange, Parallelität, Drosselungen und exakte Pläne, bevor Sie fortfahren."
            }
            "Plan identity: {}" => "Planidentität: {}",
            "Scope: {}." => "Umfang: {}.",
            "Confirmation stale: concurrency, scope, or settings changed while dialog was open. Review the queue and try again." => {
                "Bestätigung veraltet: Parallelität, Umfang oder Einstellungen wurden während des Dialogs geändert. Prüfen Sie die Warteschlange und versuchen Sie es erneut."
            }
            "Clear mailbox queue?" => "Postfachwarteschlange leeren?",
            "Discard the current queue?" => "Aktuelle Warteschlange verwerfen?",
            "Keep queue" => "Warteschlange behalten",
            "Clear queue" => "Warteschlange leeren",
            "Replace mailbox queue?" => "Postfachwarteschlange ersetzen?",
            "Replace the current queue?" => "Aktuelle Warteschlange ersetzen?",
            "Keep current queue" => "Aktuelle Warteschlange behalten",
            "Replace queue" => "Warteschlange ersetzen",
            "Source preset" => "Quellvorgabe",
            "Destination preset" => "Zielvorgabe",
            "Generic IMAP preset" => "Allgemeine IMAP-Vorgabe",
            "Google Workspace preset" => "Google-Workspace-Vorgabe",
            "Microsoft 365 preset" => "Microsoft-365-Vorgabe",
            "Fastmail preset" => "Fastmail-Vorgabe",
            "Zoho Mail preset" => "Zoho-Mail-Vorgabe",
            "Enter the provider's documented IMAP endpoint and authentication policy." => {
                "Geben Sie den dokumentierten IMAP-Endpunkt und die Authentifizierungsrichtlinien des Anbieters ein."
            }
            "OAuth is preferred; Workspace administrators may use delegated gmail.imap_admin access. MailSwiftSync does not perform consent, so configure the tenant/client flow separately." => {
                "OAuth wird bevorzugt. Workspace-Administratoren können delegierten gmail.imap_admin-Zugriff verwenden. MailSwiftSync führt keine Zustimmung durch; richten Sie den Tenant-/Client-Ablauf separat ein."
            }
            "MailSwiftSync does not perform provider consent. Automatic refresh is available when an operator supplies a registered client and refresh token. Exchange Online IMAP and tenant OAuth policy must permit the account." => {
                "MailSwiftSync fordert keine Zustimmung beim Anbieter an. Eine automatische Erneuerung ist mit registriertem Client und Refresh-Token möglich. Exchange-Online-IMAP- und Tenant-OAuth-Richtlinien müssen das Konto zulassen."
            }
            "Use a Fastmail app password, not the primary account password; create it in Fastmail security settings before preflight." => {
                "Verwenden Sie ein Fastmail-App-Passwort statt des primären Kontopassworts. Erstellen Sie es vor der Vorabprüfung in den Fastmail-Sicherheitseinstellungen."
            }
            "Zoho region and account policy may change the endpoint; confirm the documented IMAP host before preflight." => {
                "Region und Kontorichtlinien von Zoho können den Endpunkt beeinflussen. Prüfen Sie vor der Vorabprüfung den dokumentierten IMAP-Host."
            }
            "Profile saved without credential material." => "Profil ohne Zugangsdaten gespeichert.",
            "Could not save profile" => "Profil konnte nicht gespeichert werden",
            "Server" => "Server",
            "User" => "Benutzer",
            "Authentication" => "Authentifizierung",
            "Local Dovecot account" => "Lokales Dovecot-Konto",
            "IMAP connection" => "IMAP-Verbindung",
            "Use a currently valid provider-issued access token with IMAP scope. Tokens are session-only unless stored in the OS keyring. MailSwiftSync does not request provider consent, but can refresh an expired token automatically if you configure a refresh token in the OS keyring dialog's Automatic OAuth refresh section." => {
                "Verwenden Sie ein gültiges, vom Anbieter ausgestelltes Zugriffstoken mit IMAP-Berechtigung. Token gelten nur für die Sitzung, sofern sie nicht im Betriebssystem-Schlüsselbund gespeichert werden. MailSwiftSync fordert keine Anbieterfreigabe an, kann ein abgelaufenes Token jedoch automatisch erneuern, wenn im Schlüsselbunddialog die automatische OAuth-Erneuerung eingerichtet ist."
            }
            "Password" => "Passwort",
            "Access token" => "Zugriffstoken",
            "Show" => "Anzeigen",
            "Hide" => "Ausblenden",
            "Port" => "Port",
            "Credential ID" => "Zugangsdaten-ID",
            "CA bundle" => "CA-Zertifikatssammlung",
            "Certificate pin (SHA-256)" => "Zertifikat-Fingerabdruck (SHA-256)",
            "TLS" => "TLS",
            "Allow insecure source transport (review carefully)" => {
                "Unsicheren Quelltransport erlauben (sorgfältig prüfen)"
            }
            "Migration overview" => "Migrationsübersicht",
            "A calm, evidence-led workspace for moving mailboxes safely." => {
                "Ein übersichtlicher, evidenzbasierter Arbeitsbereich für sichere Postfachmigrationen."
            }
            "MIGRATION WORKFLOW" => "MIGRATIONSABLAUF",
            "Start your first migration" => "Erste Migration starten",
            "MailSwiftSync guides every migration through a reviewable preflight before any destination changes are allowed." => {
                "MailSwiftSync führt jede Migration durch eine überprüfbare Vorabprüfung, bevor Änderungen am Ziel zulässig sind."
            }
            "Configure first mailbox  →" => "Erstes Postfach konfigurieren  →",
            "For multiple mailboxes, use Batch after reviewing one representative pilot." => {
                "Verwenden Sie für mehrere Postfächer den Stapelmodus, nachdem Sie eine repräsentative Testmigration geprüft haben."
            }
            "CURRENT PHASE" => "AKTUELLE PHASE",
            "PROJECT STATUS" => "PROJEKTSTATUS",
            "MAILBOXES" => "POSTFÄCHER",
            "EVIDENCE" => "NACHWEISE",
            "Recommended next step" => "Empfohlener nächster Schritt",
            "None configured" => "Noch nicht eingerichtet",
            "Use Mailboxes to review scope before running anything." => {
                "Prüfen Sie den Umfang unter „Postfächer“, bevor Sie einen Vorgang starten."
            }
            "Safety contract" => "Sicherheitsregeln",
            "! Source transport is cleartext by explicit configuration" => {
                "! Quelltransport ist aufgrund der Konfiguration unverschlüsselt"
            }
            "✓ Encrypted source transport with certificate verification" => {
                "✓ Verschlüsselter Quelltransport mit Zertifikatsprüfung"
            }
            "INSECURE SOURCE TRANSPORT" => "UNSICHERER QUELLTRANSPORT",
            "Plain IMAP can expose the source password and mailbox data in transit." => {
                "Unverschlüsseltes IMAP kann das Quellpasswort und Postfachdaten während der Übertragung offenlegen."
            }
            "I understand and explicitly allow cleartext source transport" => {
                "Ich verstehe dies und erlaube den unverschlüsselten Quelltransport ausdrücklich"
            }
            "Use IMAPS or STARTTLS whenever possible. This acknowledgement is required before any authenticated operation, including dry preflight, and is included in the preflight fingerprint." => {
                "Verwenden Sie nach Möglichkeit IMAPS oder STARTTLS. Diese Bestätigung ist vor jedem authentifizierten Vorgang einschließlich der Vorabprüfung erforderlich und wird in den Vorabprüfungs-Fingerabdruck aufgenommen."
            }
            "Start a safe migration" => "Sichere Migration starten",
            "MailSwiftSync guides every migration through a reviewed preflight before any destination changes are allowed." => {
                "MailSwiftSync führt jede Migration durch eine geprüfte Vorabprüfung, bevor Änderungen am Ziel zulässig sind."
            }
            "Connect" => "Verbinden",
            "Configure source and destination" => "Quelle und Ziel konfigurieren",
            "Preflight" => "Vorabprüfung",
            "Authenticate and review blockers" => "Authentifizieren und Blockierungen prüfen",
            "Pilot" => "Pilot",
            "Start with a small mailbox set" => "Mit wenigen Postfächern beginnen",
            "Import mailbox list" => "Postfachliste importieren",
            "For one mailbox, continue with the migration plan below." => {
                "Für ein Postfach fahren Sie unten mit dem Migrationsplan fort."
            }
            "PROCESS OWNERSHIP REVIEW REQUIRED" => "PRÜFUNG DER PROZESSZUGEHÖRIGKEIT ERFORDERLICH",
            "MailSwiftSync could not prove that a previously recorded migration process is gone. Do not start another migration until you have checked the host process list and confirmed no MailSwiftSync engine remains." => {
                "MailSwiftSync konnte nicht nachweisen, dass ein zuvor erfasster Migrationsprozess beendet ist. Starten Sie keine weitere Migration, bevor Sie die Host-Prozessliste geprüft und bestätigt haben, dass keine MailSwiftSync-Engine mehr läuft."
            }
            "I confirmed no unverified migration process remains" => {
                "Ich habe bestätigt, dass kein ungeprüfter Migrationsprozess mehr läuft"
            }
            "HISTORICAL PROJECT · READ ONLY" => "HISTORISCHES PROJEKT · NUR LESEN",
            "You are viewing durable history for this project. The editable migration plan and execution controls are detached until you start a new migration." => {
                "Sie sehen den dauerhaften Verlauf dieses Projekts. Der bearbeitbare Migrationsplan und die Ausführungssteuerung sind getrennt, bis Sie eine neue Migration starten."
            }
            "Start a new migration" => "Neue Migration starten",
            "Migration workspace" => "Migrationsarbeitsbereich",
            "PREFLIGHT" => "VORABPRÜFUNG",
            "LIVE MIGRATION" => "LIVE-MIGRATION",
            "Recommended next step: run preflight, review blockers, then select a small pilot mailbox." => {
                "Empfohlener nächster Schritt: Vorabprüfung ausführen, Blockierungen prüfen und anschließend ein kleines Pilotpostfach auswählen."
            }
            "Preflight & readiness" => "Vorabprüfung und Bereitschaft",
            "Plan completeness is separate from live network checks. Run the authenticated probe before live migration." => {
                "Die Planvollständigkeit ist von den Live-Netzwerkprüfungen getrennt. Führen Sie vor der Live-Migration die authentifizierte Prüfung aus."
            }
            "Run authenticated readiness probe" => {
                "Authentifizierte Bereitschaftsprüfung ausführen"
            }
            "Refresh assessment" => "Bewertung aktualisieren",
            "Create project from plan" => "Projekt aus Plan erstellen",
            "Dovecot preflight checks the configured imapc source; destination readiness still requires administrative review." => {
                "Die Dovecot-Vorabprüfung kontrolliert die konfigurierte imapc-Quelle; die Zielbereitschaft erfordert weiterhin eine administrative Prüfung."
            }
            "No preflight assessment has been recorded for the current plan." => {
                "Für den aktuellen Plan wurde noch keine Vorabprüfung aufgezeichnet."
            }
            "Project controls" => "Projektsteuerung",
            "This project is complete and read-only. Reopening requires an audit reason and returns it to Attention." => {
                "Dieses Projekt ist abgeschlossen und schreibgeschützt. Zum erneuten Öffnen ist ein Prüfgrund erforderlich; anschließend wird es zur Prüfung zurückgesetzt."
            }
            "Reopen project" => "Projekt erneut öffnen",
            "Do not trust a completed process until the destination reconciles with the source." => {
                "Ein abgeschlossener Prozess gilt erst dann als bestätigt, wenn das Ziel mit der Quelle abgeglichen wurde."
            }
            "Verification and audit report" => "Verifizierungs- und Prüfbericht",
            "Mailbox evidence" => "Postfachnachweise",
            "Search" => "Suchen",
            "Loading mailbox verification…" => "Postfachverifizierung wird geladen …",
            "Durable mailbox reconciliation" => "Dauerhafter Postfachabgleich",
            "Export verification report…" => "Verifizierungsbericht exportieren …",
            "Accept residual difference" => "Verbleibende Abweichung akzeptieren",
            "Operator" => "Betreiber",
            "Durable run history" => "Dauerhafte Ausführungshistorie",
            "Filter history" => "Historie filtern",
            "No durable runs recorded yet." => {
                "Bisher sind keine dauerhaften Ausführungen erfasst."
            }
            "Migration projects" => "Migrationsprojekte",
            "New migration plan" => "Neuen Migrationsplan erstellen",
            "No historical project is selected." => "Es ist kein früheres Projekt ausgewählt.",
            "State" => "Status",
            "← Previous 200" => "← Zurück 200",
            "Next 200 →" => "Weiter 200 →",
            "Select a durable project to make it the workspace for reports, mailboxes, activity, and verification." => {
                "Wählen Sie ein dauerhaftes Projekt als Arbeitsbereich für Berichte, Postfächer, Aktivitäten und Verifizierung."
            }
            "project name, source, or destination" => "Projektname, Quelle oder Ziel",
            "Project" => "Projekt",
            "Phase" => "Phase",
            "Review, filter, select, and operate on customer mailboxes without reopening the legacy queue window." => {
                "Prüfen, filtern und bearbeiten Sie Kundenpostfächer, ohne das bisherige Warteschlangenfenster erneut zu öffnen."
            }
            "No bulk mailbox list loaded" => "Keine Postfachliste geladen",
            "A single mailbox can be configured from the migration plan." => {
                "Ein einzelnes Postfach können Sie im Migrationsplan konfigurieren."
            }
            "Import CSV / XLSX…" => "CSV / XLSX importieren…",
            "CSV or XLSX only; legacy .xls files must be converted first." => {
                "Nur CSV oder XLSX; alte .xls-Dateien müssen zuerst konvertiert werden."
            }
            "Import / edit queue" => "Warteschlange importieren / bearbeiten",
            "QUEUE HEALTH" => "WARTESCHLANGENSTATUS",
            "Use the state filter and Select visible to act on a focused set; live execution still requires a matching preflight." => {
                "Nutzen Sie Statusfilter und „Sichtbare auswählen“, um gezielt zu arbeiten. Eine Live-Ausführung erfordert weiterhin eine passende Vorabprüfung."
            }
            "mailbox, host, or user" => "Postfach, Host oder Benutzer",
            "Select visible" => "Sichtbare auswählen",
            "Select unresolved" => "Offene auswählen",
            "Select attention" => "Prüfbedürftige auswählen",
            "Clear selection" => "Auswahl aufheben",
            "Run preflight ({})" => "Vorabprüfung ausführen ({})",
            "Run live migration ({})" => "Live-Migration ausführen ({})",
            "Run final delta ({})" => "Finales Delta ausführen ({})",
            "Selected mailbox actions" => "Aktionen für ausgewählte Postfächer",
            "⚠ {} mailbox(es) are selected but hidden by the current filter. They will still be included in batch operations." => {
                "⚠ {} Postfach/er sind ausgewählt, aber durch den aktuellen Filter verborgen. Sie bleiben in Stapelvorgängen enthalten."
            }
            "Review selected ({})" => "Auswahl prüfen ({})",
            "No mailboxes selected." => "Keine Postfächer ausgewählt.",
            "DESTRUCTIVE: destination deletion enabled" => "DESTRUKTIV: Löschen am Ziel aktiviert",
            "destination deletion disabled" => "Löschen am Ziel deaktiviert",
            "State: {} · {}" => "Status: {} · {}",
            "Select one or more rows to enable actions." => {
                "Wählen Sie mindestens eine Zeile aus, um Aktionen zu aktivieren."
            }
            "Operator action" => "Betreiberaktion",
            "Select {} → {}" => "{} → {} auswählen",
            "Inspect durable run detail" => "Dauerhafte Ausführungsdetails prüfen",
            "Batch actions apply only to explicitly selected rows. Use Select unresolved or Select visible to create a selection." => {
                "Stapelaktionen gelten nur für ausdrücklich ausgewählte Zeilen. Nutzen Sie „Offene auswählen“ oder „Sichtbare auswählen“, um eine Auswahl zu erstellen."
            }
            "Batch migration queue" => "Stapel-Migrationswarteschlange",
            "Import → review → validate" => "Importieren → prüfen → validieren",
            "Batch mode" => "Stapelmodus",
            "Customer/project name" => "Kunden-/Projektname",
            "Maximum concurrent workers" => "Maximale gleichzeitige Arbeitsprozesse",
            "Transient retries" => "Wiederholungen bei vorübergehenden Fehlern",
            "Passwordless queue credentials" => "Passwortlose Zugangsdaten für die Warteschlange",
            "Source keyring ID" => "Schlüsselbund-ID der Quelle",
            "Destination keyring ID" => "Schlüsselbund-ID des Ziels",
            "Imported" => "Importiert",
            "Attention" => "Prüfung erforderlich",
            "Failed" => "Fehlgeschlagen",
            "Delta required" => "Delta erforderlich",
            "Verified" => "Verifiziert",
            "Ready" => "Bereit",
            "All states" => "Alle Status",
            "Verified with exceptions" => "Mit Ausnahmen verifiziert",
            "… plus {} more selected" => "… plus {} weitere ausgewählt",
            "OS keyring credentials" => "Zugangsdaten im Betriebssystem-Schlüsselbund",
            "Source ID" => "Quell-ID",
            "Destination ID" => "Ziel-ID",
            "Keyring IDs are non-secret references saved in the profile. Passwords and OAuth access tokens stay in the operating system credential store and are loaded only into the active session." => {
                "Schlüsselbund-IDs sind nicht geheime Verweise im Profil. Passwörter und OAuth-Zugriffstoken verbleiben im Anmeldedatenspeicher des Betriebssystems und werden nur für die aktive Sitzung geladen."
            }
            "Store source token" => "Quelltoken speichern",
            "Store source password" => "Quellpasswort speichern",
            "Store destination token" => "Zieltoken speichern",
            "Store destination password" => "Zielpasswort speichern",
            "Requires an OAuth application you have already registered with the provider and a refresh token obtained through its consent flow. MailSwiftSync does not perform consent; it only exchanges an existing refresh token for a fresh access token before each live launch." => {
                "Erfordert eine beim Anbieter registrierte OAuth-Anwendung und ein über dessen Zustimmungsablauf erhaltenes Refresh-Token. MailSwiftSync führt keine Zustimmung durch, sondern tauscht vor jedem Live-Start ein vorhandenes Refresh-Token gegen ein neues Zugriffstoken."
            }
            "The fields above are entered once per store; they are cleared from memory immediately afterward and are never written to the profile or durable ledger." => {
                "Die obigen Werte werden einmalig pro Speicherung eingegeben, danach sofort aus dem Arbeitsspeicher entfernt und niemals im Profil oder dauerhaften Datenspeicher abgelegt."
            }
            "Load source" => "Quelle laden",
            "Delete source" => "Quelle löschen",
            "Load destination" => "Ziel laden",
            "Delete destination" => "Ziel löschen",
            "Credential settings are locked while a migration is running." => {
                "Zugangsdaten sind während einer laufenden Migration gesperrt."
            }
            "Automatic OAuth refresh (optional)" => "Automatische OAuth-Erneuerung (optional)",
            "Source refresh ID" => "Erneuerungs-ID der Quelle",
            "Destination refresh ID" => "Erneuerungs-ID des Ziels",
            "Token endpoint" => "Token-Endpunkt",
            "Client ID" => "Client-ID",
            "Client secret (if required)" => "Client-Secret (falls erforderlich)",
            "Refresh token" => "Refresh-Token",
            "Store for source" => "Für Quelle speichern",
            "Store for destination" => "Für Ziel speichern",
            "Refresh source now" => "Quelle jetzt erneuern",
            "Refresh destination now" => "Ziel jetzt erneuern",
            "Delete source refresh config" => "Erneuerungskonfiguration der Quelle löschen",
            "Delete destination refresh config" => "Erneuerungskonfiguration des Ziels löschen",
            "The migration will stop where it is" => {
                "Die Migration wird an dieser Stelle angehalten"
            }
            "The destination may be partially migrated. A later preflight, delta, or verification pass may be required before continuing." => {
                "Das Zielpostfach kann teilweise migriert sein. Vor der Fortsetzung kann eine erneute Vorabprüfung, Delta-Migration oder Verifizierung erforderlich sein."
            }
            "Keep running" => "Weiter ausführen",
            "Stop migration" => "Migration anhalten",
            "Live output is retained here for operator review. Durable run history remains available after restart." => {
                "Die Live-Ausgabe bleibt hier zur Prüfung durch den Betreiber verfügbar. Die dauerhafte Ausführungshistorie bleibt auch nach einem Neustart erhalten."
            }
            "Prepare safe retry  →" => "Sicheren erneuten Versuch vorbereiten  →",
            "Run in progress" => "Ausführung läuft",
            "No active run" => "Keine aktive Ausführung",
            "Copy support summary" => "Support-Zusammenfassung kopieren",
            "Raw output…" => "Rohe Ausgabe …",
            "May contain mailbox metadata" => "Kann Postfachmetadaten enthalten",
            "Copy redacted engine output" => "Bereinigte Engine-Ausgabe kopieren",
            "Show recent 20" => "Letzte 20 anzeigen",
            "Show up to 250 runs" => "Bis zu 250 Ausführungen anzeigen",
            "Create or restore a project to see durable runs." => {
                "Erstellen Sie ein Projekt oder stellen Sie es wieder her, um dauerhafte Ausführungen zu sehen."
            }
            "mailbox, phase, engine, run ID, or detail" => {
                "Postfach, Phase, Engine, Ausführungs-ID oder Details"
            }
            "Errors and attention" => "Fehler und erforderliche Maßnahmen",
            "Running" => "Wird ausgeführt",
            "Completed" => "Abgeschlossen",
            "All statuses" => "Alle Status",
            "The transfer engine is only one part of the migration. This report is the operator-facing proof of what arrived and what still needs attention." => {
                "Die Übertragungs-Engine ist nur ein Teil der Migration. Dieser Bericht zeigt, was angekommen ist und wo noch Maßnahmen erforderlich sind."
            }
            "Export project report…" => "Projektbericht exportieren …",
            "Export project JSON…" => "Projekt-JSON exportieren …",
            "Export customer proof JSON…" => "Kundennachweis als JSON exportieren …",
            "Export support bundle…" => "Support-Paket exportieren …",
            "Export project health…" => "Projektstatus exportieren …",
            "Customer proof becomes available after the durable project is Complete, every mailbox is verified, and the state view is current." => {
                "Der Kundennachweis ist verfügbar, wenn das dauerhafte Projekt abgeschlossen, jedes Postfach verifiziert und die Zustandsansicht aktuell ist."
            }
            "Customer proof is ready: the durable project is Complete and no mailbox requires review." => {
                "Der Kundennachweis ist bereit: Das dauerhafte Projekt ist abgeschlossen und kein Postfach muss geprüft werden."
            }
            "Customer proof remains gated until the durable project is Complete, every mailbox is verified, and the state view is current." => {
                "Der Kundennachweis bleibt gesperrt, bis das dauerhafte Projekt abgeschlossen, jedes Postfach verifiziert und die Zustandsansicht aktuell ist."
            }
            "mailbox or destination" => "Postfach oder Ziel",
            "Needs review" => "Prüfung erforderlich",
            "Differences" => "Abweichungen",
            "All results" => "Alle Ergebnisse",
            "Mailbox" => "Postfach",
            "Evidence" => "Nachweis",
            "Result" => "Ergebnis",
            "No evidence" => "Kein Nachweis",
            "← Previous" => "← Zurück",
            "Next →" => "Weiter →",
            "Evidence labels describe metadata and aggregate reconciliation; message bodies were not compared." => {
                "Die Nachweislabels beschreiben Metadaten und den Gesamtabgleich; Nachrichteninhalte wurden nicht verglichen."
            }
            "Verification method" => "Verifizierungsmethode",
            "Verification outcome" => "Verifizierungsergebnis",
            "Folders" => "Ordner",
            "Messages" => "Nachrichten",
            "Bytes" => "Bytes",
            "Unresolved" => "Nicht geklärt",
            "Missing" => "Fehlend",
            "Extra" => "Zusätzlich",
            "Modified" => "Geändert",
            "Reason" => "Grund",
            "The transfer finished, but no mailbox-level evidence has been captured yet." => {
                "Die Übertragung ist beendet, aber es liegen noch keine Nachweise auf Postfachebene vor."
            }
            "This records an auditable exception; it does not change the underlying evidence or claim exact equality." => {
                "Dies erfasst eine prüfbare Ausnahme. Die zugrunde liegenden Nachweise werden dadurch nicht geändert und keine exakte Übereinstimmung behauptet."
            }
            "Why is this difference acceptable? Include the change-ticket or customer approval reference." => {
                "Warum ist diese Abweichung akzeptabel? Geben Sie das Änderungs-Ticket oder die Kundenfreigabe an."
            }
            "Accept and mark verified with exceptions" => {
                "Akzeptieren und als mit Ausnahmen verifiziert markieren"
            }
            "Run a migration to create a durable mailbox evidence record." => {
                "Führen Sie eine Migration aus, um einen dauerhaften Postfachnachweis zu erstellen."
            }
            "Settings" => "Einstellungen",
            "Operator settings" => "Betreibereinstellungen",
            "Appearance and workspace tools live here. Migration connection and engine choices belong on the Migration plan so the active plan stays visible while you configure it." => {
                "Darstellung und Arbeitsbereichswerkzeuge befinden sich hier. Verbindung und Engine wählen Sie im Migrationsplan, damit der aktive Plan beim Konfigurieren sichtbar bleibt."
            }
            "Appearance" => "Darstellung",
            "Color pack" => "Farbschema",
            "Theme" => "Darstellungsmodus",
            "Dark" => "Dunkel",
            "Light" => "Hell",
            "Interface size: {:.0}%" => "Oberflächengröße: {:.0}%",
            "Interface size" => "Oberflächengröße",
            "Decrease" => "Verkleinern",
            "Increase" => "Vergrößern",
            "Save appearance preferences" => "Darstellung speichern",
            "Language" => "Sprache",
            "Report branding" => "Berichtskennzeichnung",
            "Optional. Applied to customer-proof exports as an \"issued by\" line; independent of the migration plan and never affects preflight/live execution. Leave blank to omit it entirely." => {
                "Optional. Wird in Kundenberichten als Zeile „Ausgestellt von“ verwendet. Die Angabe ist unabhängig vom Migrationsplan und beeinflusst weder Vorabprüfung noch Ausführung. Leer lassen, um sie wegzulassen."
            }
            "Agency/operator name" => "Name der Organisation / des Betreibers",
            "Contact" => "Kontakt",
            "Save report branding" => "Berichtskennzeichnung speichern",
            "Workspace tools" => "Arbeitsbereich",
            "Open Migration plan" => "Migrationsplan öffnen",
            "Project browser" => "Projektübersicht",
            "Current engine: {}. Connection, credentials, advanced options, and readiness are available from the Migration plan." => {
                "Aktuelle Engine: {}. Verbindung, Zugangsdaten, erweiterte Optionen und Bereitschaft finden Sie im Migrationsplan."
            }
            "Could not save appearance preference: {value}" => {
                "Darstellungseinstellung konnte nicht gespeichert werden: {value}"
            }
            "Could not save report branding: {value}" => {
                "Berichtskennzeichnung konnte nicht gespeichert werden: {value}"
            }
            "Could not save appearance preference" => {
                "Darstellungseinstellung konnte nicht gespeichert werden"
            }
            "Could not save report branding" => {
                "Berichtskennzeichnung konnte nicht gespeichert werden"
            }
            "Connection, credentials, advanced options, and readiness are available from the Migration plan." => {
                "Verbindung, Zugangsdaten, erweiterte Optionen und Bereitschaft finden Sie im Migrationsplan."
            }
            _ => source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UiLanguage;

    #[test]
    fn language_catalog_keeps_english_default_and_german_shell_labels() {
        assert_eq!(UiLanguage::English.text("Overview"), "Overview");
        assert_eq!(UiLanguage::German.text("Overview"), "Übersicht");
        assert_eq!(UiLanguage::German.label(), "Deutsch (teilweise)");
        assert_eq!(
            UiLanguage::German.text("untranslated technical detail"),
            "untranslated technical detail"
        );
    }

    #[test]
    fn destructive_and_safety_paths_are_translated() {
        for key in [
            "Confirm live migration",
            "I understand — start migration",
            "Confirm live batch migration",
            "I understand — start batch",
            "Sample of selected mailboxes:",
            "View all selected",
            "Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.",
            "Confirmation stale: concurrency, scope, or settings changed while dialog was open. Review the queue and try again.",
            "Review selected ({})",
            "No mailboxes selected.",
            "DESTRUCTIVE: destination deletion enabled",
            "Select one or more rows to enable actions.",
            "Operator action",
            "Inspect durable run detail",
            "Clear mailbox queue?",
            "Advanced migration options",
            "Review verification",
            "! Source transport is cleartext by explicit configuration",
            "✓ Encrypted source transport with certificate verification",
            "Plain IMAP can expose the source password and mailbox data in transit.",
            "I understand and explicitly allow cleartext source transport",
            "PROCESS OWNERSHIP REVIEW REQUIRED",
            "I confirmed no unverified migration process remains",
            "HISTORICAL PROJECT · READ ONLY",
            "Start a new migration",
            "Run authenticated readiness probe",
            "Create project from plan",
            "No preflight assessment has been recorded for the current plan.",
            "Reopen project",
            "Passwords are redacted. This is an argument list for review, not a shell command to paste.",
            "Fix the advanced options above before this plan can run.",
            "These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.",
            "Delete destination messages missing from source  (--delete2)",
            "Use only for an intentionally exact backup after a tested preflight. This can remove destination mail.",
            "The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German translation: {key}"
            );
        }
    }
}
