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
        self.lookup(source).unwrap_or(source)
    }

    /// Translate text that is only known at runtime, such as a status
    /// message stored as a `String`. Returns `None` for English and for text
    /// without a catalog entry (for example, messages with values filled in).
    pub(crate) fn lookup(self, source: &str) -> Option<&'static str> {
        if self == Self::English {
            return None;
        }
        Some(match source {
            "Overview" => "Übersicht",
            "Manage" => "Verwalten",
            "Tools" => "Werkzeuge",
            "Connection details" => "Verbindungsdetails",
            "Preflight runs the engine without changing the destination. Clear Dry run / preflight to start a live migration." => {
                "Die Vorabprüfung führt die Engine aus, ohne das Ziel zu verändern. Deaktivieren Sie Probelauf / Vorabprüfung, um eine Live-Migration zu starten."
            }
            "Local Dovecot destination" => "Lokales Dovecot-Ziel",
            "Destination: local Dovecot storage" => "Ziel: lokaler Dovecot-Speicher",
            "imapsync options" => "imapsync-Optionen",
            "Map standard folders automatically" => "Standardordner automatisch zuordnen",
            "Folders only" => "Nur Ordner",
            "Add Message-ID header when needed" => "Message-ID-Header bei Bedarf hinzufügen",
            "Extra imapsync options" => "Zusätzliche imapsync-Optionen",
            "imapsync executable" => "imapsync-Programmdatei",
            "Leave empty to use imapsync from PATH." => {
                "Leer lassen, um imapsync aus PATH zu verwenden."
            }
            "DESTINATION DELETION ENABLED" => "ZIELLÖSCHUNG AKTIVIERT",
            "Messages that exist only on the destination may be removed during live migration." => {
                "Nachrichten, die nur im Ziel vorhanden sind, können während der Live-Migration entfernt werden."
            }
            "ATTENTION CENTER" => "ZENTRALE FÜR AUFMERKSAMKEIT",
            "Batch queue loaded" => "Stapelwarteschlange geladen",
            "Check" => "Prüfung",
            "Current engine:" => "Aktuelle Engine:",
            "0 selected · 0 visible · 0 hidden by current filter" => {
                "0 ausgewählt · 0 sichtbar · 0 durch aktuellen Filter verborgen"
            }
            "(fixed palette)" => "(feste Palette)",
            "Project: {}" => "Projekt: {}",
            "Executable: {}" => "Ausführbare Datei: {}",
            "in progress" => "läuft",
            "This will invoke {} with the current credentials and rules." => {
                "Dies ruft {} mit den aktuellen Zugangsdaten und Regeln auf."
            }
            "Dovecot strategy: {} — {}" => "Dovecot-Strategie: {} — {}",
            "Cancellation requested…" => "Abbruch angefordert…",
            "Execution completed without a durable run context" => {
                "Ausführung ohne dauerhaften Laufkontext abgeschlossen"
            }
            "Migration result requires durable storage; retrying terminal commit" => {
                "Migrationsergebnis benötigt dauerhafte Speicherung; Abschluss wird erneut versucht"
            }
            "Migration result requires durability review" => {
                "Migrationsergebnis erfordert eine Prüfung der Dauerhaftigkeit"
            }
            "Migration failed: {error}" => "Migration fehlgeschlagen: {error}",
            "Preflight completed successfully" => "Vorabprüfung erfolgreich abgeschlossen",
            "Batch transfer completed; review per-mailbox verification results" => {
                "Stapelübertragung abgeschlossen; prüfen Sie die Verifizierungsergebnisse je Postfach"
            }
            "Migration completed and verified" => "Migration abgeschlossen und verifiziert",
            "Migration completed with accepted verification exceptions" => {
                "Migration mit akzeptierten Verifizierungsausnahmen abgeschlossen"
            }
            "Dovecot synchronization completed with changes pending; repeat the final pass until exit code 0" => {
                "Dovecot-Synchronisierung mit ausstehenden Änderungen abgeschlossen; wiederholen Sie den letzten Durchlauf bis zum Exit-Code 0"
            }
            "Migration completed; verification found differences requiring review" => {
                "Migration abgeschlossen; die Verifizierung fand prüfpflichtige Unterschiede"
            }
            "Migration completed; verification requires operator review" => {
                "Migration abgeschlossen; die Verifizierung erfordert eine Betreiberprüfung"
            }
            "Provider readiness runbook" => "Anbieter-Betriebsleitfaden",
            "Read-only operational guidance. Preflight and live admission remain authoritative." => {
                "Schreibgeschützte Betriebshinweise. Vorprüfung und Live-Zulassung bleiben maßgeblich."
            }
            "Guidance version: {}" => "Leitfadenversion: {}",
            "Why:" => "Warum:",
            "Success:" => "Erfolg:",
            "Known provider issues" => "Bekannte Anbieterprobleme",
            "Recovery guidance" => "Wiederherstellungshinweise",
            "Do not resume until the current endpoint and durable state have been reviewed." => {
                "Setzen Sie erst fort, nachdem der aktuelle Endpunkt und der dauerhafte Zustand geprüft wurden."
            }
            "Migration was paused by operator" => "Die Migration wurde vom Betreiber pausiert",
            "Review any configuration changes since pause" => {
                "Prüfen Sie alle Konfigurationsänderungen seit der Pause"
            }
            "Click 'Resume' to continue from the last checkpoint" => {
                "Klicken Sie auf „Fortsetzen“, um ab dem letzten Prüfpunkt fortzufahren"
            }
            "Network connection to provider was lost" => {
                "Die Netzwerkverbindung zum Anbieter wurde unterbrochen"
            }
            "Retry delay may reduce a transient failure, but does not prove recovery" => {
                "Eine Wiederholungsverzögerung kann einen vorübergehenden Fehler verringern, beweist aber keine Wiederherstellung"
            }
            "Revalidate DNS, TCP, TLS, authentication, and IMAP capability before resuming" => {
                "Prüfen Sie DNS, TCP, TLS, Authentifizierung und IMAP-Fähigkeiten vor dem Fortsetzen erneut"
            }
            "Check firewall rules and VPN if applicable" => {
                "Prüfen Sie gegebenenfalls Firewall-Regeln und VPN"
            }
            "Resume uses the engine's checkpoint semantics; reconcile aggregate evidence afterward" => {
                "Das Fortsetzen verwendet die Prüfpunktsemantik der Engine; gleichen Sie anschließend die aggregierten Nachweise ab"
            }
            "The provider or IMAP server returned a throttling signal" => {
                "Der Anbieter oder IMAP-Server meldete eine Drosselung"
            }
            "Wait for the configured retry delay; elapsed time does not prove that the limit has reset" => {
                "Warten Sie die konfigurierte Wiederholungsverzögerung ab; verstrichene Zeit beweist nicht, dass das Limit zurückgesetzt wurde"
            }
            "Resume with conservative profile message/byte limits and monitor for another server response" => {
                "Setzen Sie mit konservativen Nachrichten-/Byte-Limits fort und beobachten Sie weitere Serverantworten"
            }
            "Source or destination mailbox became unavailable" => {
                "Das Quell- oder Zielpostfach ist nicht mehr verfügbar"
            }
            "Verify the source mailbox is accessible and credentials are still valid" => {
                "Prüfen Sie, ob das Quellpostfach erreichbar und die Zugangsdaten noch gültig sind"
            }
            "Check destination mailbox quota and disk space" => {
                "Prüfen Sie das Kontingent des Zielpostfachs und den Speicherplatz"
            }
            "If source is a shared mailbox, verify access permissions haven't changed" => {
                "Prüfen Sie bei einem gemeinsam genutzten Quellpostfach, ob sich die Zugriffsrechte geändert haben"
            }
            "Test connectivity with a manual IMAP connection before resuming" => {
                "Testen Sie die Verbindung vor dem Fortsetzen mit einer manuellen IMAP-Verbindung"
            }
            "Migration process was terminated (killed, system reboot, etc.)" => {
                "Der Migrationsprozess wurde beendet (Abbruch, Systemneustart usw.)"
            }
            "Controller state and completed engine evidence are durably stored; in-flight work requires reconciliation" => {
                "Controllerstatus und abgeschlossene Engine-Nachweise sind dauerhaft gespeichert; laufende Arbeit muss abgeglichen werden"
            }
            "Review system logs to understand why termination occurred" => {
                "Prüfen Sie die Systemprotokolle, um den Grund der Beendigung zu ermitteln"
            }
            "If termination was due to resource constraints, increase available memory or CPU" => {
                "Erhöhen Sie verfügbaren Speicher oder CPU, wenn Ressourcenmangel die Beendigung verursacht hat"
            }
            "Revalidate endpoints and review aggregate evidence before resuming from the engine checkpoint" => {
                "Prüfen Sie die Endpunkte erneut und bewerten Sie aggregierte Nachweise, bevor Sie vom Engine-Prüfpunkt fortsetzen"
            }
            "Application or system crashed unexpectedly" => {
                "Die Anwendung oder das System ist unerwartet abgestürzt"
            }
            "The durable ledger preserves recorded controller state; it does not prove the outcome of in-flight engine work" => {
                "Das dauerhafte Journal bewahrt den aufgezeichneten Controllerstatus, beweist aber nicht das Ergebnis laufender Engine-Arbeit"
            }
            "Review application logs (support bundle) to diagnose the crash" => {
                "Prüfen Sie die Anwendungsprotokolle (Support-Bundle), um den Absturz zu untersuchen"
            }
            "Ensure system has adequate disk space and memory available" => {
                "Stellen Sie ausreichenden Speicherplatz und Arbeitsspeicher sicher"
            }
            "Review the engine checkpoint and aggregate evidence before attempting resume" => {
                "Prüfen Sie den Engine-Prüfpunkt und die aggregierten Nachweise, bevor Sie fortsetzen"
            }
            "Interruption cause is unknown" => "Die Ursache der Unterbrechung ist unbekannt",
            "Review the support bundle for detailed diagnostic information" => {
                "Prüfen Sie das Support-Bundle auf detaillierte Diagnoseinformationen"
            }
            "Verify network connectivity, provider status, and endpoint accessibility" => {
                "Prüfen Sie Netzwerkverbindung, Anbieterstatus und Erreichbarkeit der Endpunkte"
            }
            "Contact support if recovery fails after resume" => {
                "Wenden Sie sich an den Support, wenn die Wiederherstellung nach dem Fortsetzen fehlschlägt"
            }
            "Selected {} mailbox row(s) for focused review." => {
                "{} Postfachzeile(n) zur gezielten Prüfung ausgewählt."
            }
            "Imported {} mailbox rows. Review them and run preflight before migration." => {
                "{} Postfachzeile(n) importiert. Prüfen Sie sie und führen Sie vor der Migration eine Vorprüfung aus."
            }
            "Choose the worksheet containing the migration rows before importing." => {
                "Wählen Sie vor dem Import das Arbeitsblatt mit den Migrationszeilen aus."
            }
            "A mailbox file is already being imported." => {
                "Eine Postfachdatei wird bereits importiert."
            }
            "Importing {} in the background…" => "{} wird im Hintergrund importiert…",
            "Importing the selected worksheet in the background…" => {
                "Das ausgewählte Arbeitsblatt wird im Hintergrund importiert…"
            }
            "Queue cleared; its durable batch association was discarded." => {
                "Warteschlange geleert; die dauerhafte Stapelzuordnung wurde verworfen."
            }
            "Execution is blocked while the durable state view is stale. Resolve the SQLite refresh error and refresh before starting a migration." => {
                "Die Ausführung ist blockiert, weil die dauerhafte Zustandsansicht veraltet ist. Beheben Sie den SQLite-Aktualisierungsfehler und aktualisieren Sie die Ansicht vor dem Start."
            }
            "Execution is blocked because the saved migration profile is unavailable; repair it before starting a migration." => {
                "Die Ausführung ist blockiert, weil das gespeicherte Migrationsprofil nicht verfügbar ist. Reparieren Sie es vor dem Start."
            }
            "This project is being viewed read-only. Start a new migration to execute a plan." => {
                "Dieses Projekt wird schreibgeschützt angezeigt. Starten Sie eine neue Migration, um einen Plan auszuführen."
            }
            "Execution is blocked until you confirm that no unverified migration process remains on this host." => {
                "Die Ausführung ist blockiert, bis Sie bestätigen, dass auf diesem Host kein nicht verifizierter Migrationsprozess mehr läuft."
            }
            "Batch execution is blocked while the durable state view is stale. Resolve the SQLite refresh error and refresh before starting a queue." => {
                "Die Stapelausführung ist blockiert, weil die dauerhafte Zustandsansicht veraltet ist. Beheben Sie den SQLite-Aktualisierungsfehler und aktualisieren Sie die Ansicht vor dem Start."
            }
            "Batch execution is blocked because the saved migration profile is unavailable; repair it before starting a queue." => {
                "Die Stapelausführung ist blockiert, weil das gespeicherte Migrationsprofil nicht verfügbar ist. Reparieren Sie es vor dem Start."
            }
            "This project is being viewed read-only. Start a new migration to execute a batch." => {
                "Dieses Projekt wird schreibgeschützt angezeigt. Starten Sie eine neue Migration, um einen Stapel auszuführen."
            }
            "Import a file before starting the queue." => {
                "Importieren Sie eine Datei, bevor Sie die Warteschlange starten."
            }
            "Batch execution requires durable SQLite storage." => {
                "Die Stapelausführung benötigt dauerhaften SQLite-Speicher."
            }
            "Run a successful preflight for this queue before starting live migrations." => {
                "Führen Sie eine erfolgreiche Vorprüfung für diese Warteschlange durch, bevor Sie Live-Migrationen starten."
            }
            "Live batch blocked: one or more selected imapsync plans use automapping, which cannot currently be independently verified. Disable automap and rerun preflight for those mailboxes." => {
                "Live-Stapel blockiert: Mindestens ein ausgewählter imapsync-Plan verwendet Automapping, das derzeit nicht unabhängig verifiziert werden kann. Deaktivieren Sie Automapping und führen Sie die Vorprüfung für diese Postfächer erneut aus."
            }
            "Conservative default" => "Konservative Voreinstellung",
            "Dovecot native" => "Dovecot nativ",
            "imapsync fallback" => "imapsync-Fallback",
            "Use imapsync as the conservative default; select Dovecot native explicitly when appropriate." => {
                "Verwenden Sie imapsync als konservative Voreinstellung; wählen Sie Dovecot nativ nur bei passender Umgebung."
            }
            "Use destination-side doveadm/dsync when the destination is Dovecot and admin access is available." => {
                "Verwenden Sie doveadm/dsync auf dem Ziel, wenn das Ziel Dovecot ist und Administrationszugriff besteht."
            }
            "Use imapsync when both ends are arbitrary IMAP servers or no destination admin stack is available." => {
                "Verwenden Sie imapsync bei beliebigen IMAP-Servern oder wenn kein Administrationsstapel am Ziel verfügbar ist."
            }
            "Initial mirror" => "Erstspiegelung",
            "Incremental mirror" => "Inkrementelle Spiegelung",
            "Final preservation pass" => "Finaler Erhaltungslauf",
            "Destination already active" => "Ziel bereits aktiv",
            "doveadm backup: mirror source mail to the destination; destination-only changes may be replaced. Dovecot may need to replace INBOX, which some Maildir targets refuse; test the exact target storage before migration." => {
                "doveadm backup: Quellnachrichten auf das Ziel spiegeln; nur am Ziel vorhandene Änderungen können ersetzt werden. Dovecot muss möglicherweise INBOX ersetzen, was manche Maildir-Ziele ablehnen; testen Sie den genauen Zielspeicher vor der Migration."
            }
            "doveadm backup with the durable checkpoint: repeat an initial mirror before cutover. Maildir targets may refuse INBOX replacement; test the exact storage format." => {
                "doveadm backup mit dauerhaftem Prüfpunkt: Vor dem Umschalten eine Erstspiegelung wiederholen. Maildir-Ziele können das Ersetzen von INBOX ablehnen; testen Sie das genaue Speicherformat."
            }
            "doveadm sync -1: preserve destination-side changes for the final cutover pass." => {
                "doveadm sync -1: Änderungen am Ziel für den finalen Umschaltlauf erhalten."
            }
            "Advanced preservation mode using doveadm sync -1; review merge behavior and Dovecot load carefully." => {
                "Erweiterter Erhaltungsmodus mit doveadm sync -1; prüfen Sie Zusammenführungsverhalten und Dovecot-Auslastung sorgfältig."
            }
            "Interrupted; recovery review required" => {
                "Unterbrochen; Wiederherstellungsprüfung erforderlich"
            }
            "Verification evidence is incomplete" => "Verifizierungsnachweise sind unvollständig",
            "Verification found differences" => "Verifizierung hat Unterschiede gefunden",
            "Process ownership could not be verified" => {
                "Prozesszugehörigkeit konnte nicht verifiziert werden"
            }
            "Authentication failed" => "Authentifizierung fehlgeschlagen",
            "Network or remote-service failure" => "Netzwerk- oder Remote-Dienstfehler",
            "Blocked by migration policy" => "Durch Migrationsrichtlinie blockiert",
            "Configuration is invalid" => "Konfiguration ist ungültig",
            "Capacity or rate limit reached" => "Kapazitäts- oder Ratenlimit erreicht",
            "A message was rejected by the destination" => {
                "Eine Nachricht wurde vom Ziel abgelehnt"
            }
            "Operator review required" => "Bedienerprüfung erforderlich",
            "Confirm no migration process remains, then retry" => {
                "Bestätigen Sie, dass kein Migrationsprozess mehr läuft, und versuchen Sie es erneut"
            }
            "Review the evidence and reconcile before retrying or completing" => {
                "Prüfen und gleichen Sie die Nachweise ab, bevor Sie es erneut versuchen oder abschließen"
            }
            "Verify credentials and endpoint permissions before retrying" => {
                "Prüfen Sie Zugangsdaten und Endpunktberechtigungen vor einem erneuten Versuch"
            }
            "Check endpoint health and retry with bounded backoff" => {
                "Prüfen Sie den Endpunktstatus und versuchen Sie es mit begrenztem Backoff erneut"
            }
            "Correct the migration configuration or policy, then rerun preflight" => {
                "Korrigieren Sie Migrationskonfiguration oder Richtlinie und führen Sie die Vorabprüfung erneut aus"
            }
            "Reduce concurrency or rate and retry after capacity recovers" => {
                "Reduzieren Sie Parallelität oder Rate und versuchen Sie es nach Wiederherstellung der Kapazität erneut"
            }
            "Review rejected-message detail and destination policy before retrying" => {
                "Prüfen Sie Details abgelehnter Nachrichten und die Zielrichtlinie vor einem erneuten Versuch"
            }
            "Inspect the durable run detail before choosing an action" => {
                "Prüfen Sie die dauerhaften Laufdetails, bevor Sie eine Aktion wählen"
            }
            "Customer proof ready" => "Kundennachweis bereit",
            "Each item names the durable reason and the next safe operator action." => {
                "Jeder Eintrag nennt den dauerhaft gespeicherten Grund und die nächste sichere Bedieneraktion."
            }
            "Imported rows are not durable until preflight admission succeeds." => {
                "Importierte Zeilen sind erst nach erfolgreicher Vorabprüfung dauerhaft gespeichert."
            }
            "No project yet" => "Noch kein Projekt",
            "Not available" => "Nicht verfügbar",
            "Open Verification to export the customer-safe evidence artifact." => {
                "Öffnen Sie die Verifizierung, um den kundensicheren Nachweis zu exportieren."
            }
            "Open Verification to review evidence; customer proof remains gated until the durable state is complete." => {
                "Öffnen Sie die Verifizierung, um Nachweise zu prüfen; der Kundennachweis bleibt gesperrt, bis der dauerhafte Status vollständig ist."
            }
            "Open migration plan" => "Migrationsplan öffnen",
            "Open verification" => "Verifizierung öffnen",
            "Project created" => "Projekt erstellt",
            "Retry prepared as a dry preflight. Review the exact plan before any live run." => {
                "Wiederholung als Vorabprüfung vorbereitet. Prüfen Sie den exakten Plan vor jedem Live-Lauf."
            }
            "Review required" => "Prüfung erforderlich",
            "Review the imported rows, then run a durable preflight." => {
                "Prüfen Sie die importierten Zeilen und führen Sie anschließend eine dauerhafte Vorabprüfung aus."
            }
            "Start by configuring endpoints or importing a mailbox list." => {
                "Beginnen Sie mit der Konfiguration der Endpunkte oder dem Import einer Postfachliste."
            }
            "State is durable and ready for review." => {
                "Der Status ist dauerhaft gespeichert und zur Prüfung bereit."
            }
            "The highlighted step is the current operator focus. A completed-looking step never bypasses the durable execution gates." => {
                "Der hervorgehobene Schritt ist der aktuelle Bedienerfokus. Ein abgeschlossen wirkender Schritt umgeht niemals die dauerhaften Ausführungsprüfungen."
            }
            "Discovery" => "Entdeckung",
            "Preflight" => "Vorabprüfung",
            "Pilot" => "Pilot",
            "Seed" => "Erstkopie",
            "Catch-up" => "Nachziehen",
            "Final delta" => "Finales Delta",
            "Complete" => "Abgeschlossen",
            "Attention" => "Prüfung erforderlich",
            "A migration is running — monitor Activity or use Stop migration if you need to halt it." => {
                "Eine Migration läuft — beobachten Sie die Aktivität oder verwenden Sie „Migration anhalten“, wenn Sie sie stoppen müssen."
            }
            "Review Attention items before starting another migration." => {
                "Prüfen Sie die offenen Punkte, bevor Sie eine weitere Migration starten."
            }
            "Create the project, then run a dry preflight against a test mailbox." => {
                "Erstellen Sie das Projekt und führen Sie anschließend eine Vorabprüfung mit einem Testpostfach aus."
            }
            "Run the dry preflight and review every blocker before going live." => {
                "Führen Sie die Vorabprüfung aus und prüfen Sie jede Blockierung vor dem Live-Betrieb."
            }
            "Review the preflight, then choose a small pilot mailbox." => {
                "Prüfen Sie die Vorabprüfung und wählen Sie anschließend ein kleines Pilotpostfach."
            }
            "Review the pilot result and prepare the seed operation." => {
                "Prüfen Sie das Pilotergebnis und bereiten Sie die Erstkopie vor."
            }
            "Run the seed operation, then schedule a catch-up pass." => {
                "Führen Sie die Erstkopie aus und planen Sie anschließend einen Nachlauf."
            }
            "Run catch-up during the migration window and review its result." => {
                "Führen Sie den Nachlauf während des Migrationsfensters aus und prüfen Sie das Ergebnis."
            }
            "Run the final delta, then open Verification for reconciliation." => {
                "Führen Sie das finale Delta aus und öffnen Sie anschließend die Verifizierung zum Abgleich."
            }
            "Review evidence for each mailbox and export the verification report." => {
                "Prüfen Sie die Nachweise jedes Postfachs und exportieren Sie den Verifizierungsbericht."
            }
            "The project is complete; export the report and retain the audit record." => {
                "Das Projekt ist abgeschlossen; exportieren Sie den Bericht und bewahren Sie den Prüfdatensatz auf."
            }
            "The batch is running — monitor Activity and review any Attention rows before continuing." => {
                "Der Stapel läuft — beobachten Sie die Aktivität und prüfen Sie offene Zeilen, bevor Sie fortfahren."
            }
            "Review Attention items in the imported batch before starting another operation." => {
                "Prüfen Sie offene Punkte im importierten Stapel, bevor Sie einen weiteren Vorgang starten."
            }
            "Review the imported mailbox rows, then run a dry preflight before any live migration." => {
                "Prüfen Sie die importierten Postfachzeilen und führen Sie vor jeder Live-Migration eine Vorabprüfung aus."
            }
            "Plan" => "Plan",
            "Mailboxes" => "Postfächer",
            "Activity" => "Aktivität",
            "Verification" => "Verifizierung",
            "Projects" => "Projekte",
            "Stop" => "Anhalten",
            "Durable view is stale" => "Dauerhafte Ansicht ist nicht aktuell",
            "Migration still running" => "Migration läuft noch",
            "This desktop owns the migration controller. Closing the window will interrupt the run and require recovery review; migrations cannot yet continue after the desktop exits." => {
                "Diese Desktop-Anwendung besitzt den Migrationscontroller. Beim Schließen des Fensters wird die Ausführung unterbrochen und muss anschließend geprüft werden; Migrationen können nach dem Beenden der Anwendung noch nicht weiterlaufen."
            }
            "To stop safely, use Stop migration in Activity and wait for the run to finish before closing." => {
                "Zum sicheren Anhalten verwenden Sie „Migration anhalten“ unter Aktivität und warten Sie vor dem Schließen, bis die Ausführung beendet ist."
            }
            "Keep window open" => "Fenster geöffnet lassen",
            "Go to Activity" => "Zu Aktivität wechseln",
            "Migration plan" => "Migrationsplan",
            "PROVIDER QUALIFICATION" => "ANBIETERQUALIFIZIERUNG",
            "UNQUALIFIED" => "NICHT QUALIFIZIERT",
            "COMMUNITY / UNQUALIFIED PATH" => "COMMUNITY-PFAD / NICHT QUALIFIZIERT",
            "The generic IMAP migration path is available, but this provider pair has not passed the MailSwiftSync production qualification suite." => {
                "Der allgemeine IMAP-Migrationspfad ist verfügbar, aber diese Anbieterpaarung hat die MailSwiftSync-Produktionsqualifizierung nicht bestanden."
            }
            "Last qualification: none · no live provider evidence is currently bundled." => {
                "Letzte Qualifizierung: keine · derzeit sind keine Live-Anbieternachweise enthalten."
            }
            "Qualification requires tested authentication, folder inventory and mapping, independent verification, interruption recovery, throttling recovery, and large-mailbox coverage." => {
                "Die Qualifizierung erfordert getestete Authentifizierung, Ordnerinventar und -zuordnung, unabhängige Verifizierung, Wiederherstellung nach Unterbrechung und Drosselung sowie Tests mit großen Postfächern."
            }
            "PRE-MIGRATION SIMULATION" => "MIGRATIONSSIMULATION",
            "SCOPE" => "UMFANG",
            "RISKS" => "RISIKEN",
            "Plan preview only — estimates are shown only when supported by observed data." => {
                "Nur Planvorschau — Schätzungen werden nur bei ausreichenden Messdaten angezeigt."
            }
            "SOURCE" => "QUELLE",
            "DESTINATION" => "ZIEL",
            "ENGINE" => "MIGRATIONSPROGRAMM",
            "MAPPING" => "ZUORDNUNG",
            "AUTH" => "AUTHENTIFIZIERUNG",
            "ESTIMATED SCALE" => "GESCHÄTZTER UMFANG",
            "PROPOSED EXECUTION" => "VORGESCHLAGENER ABLAUF",
            "Read-only; source messages are not deleted by default" => {
                "Schreibgeschützt; Quellnachrichten werden standardmäßig nicht gelöscht."
            }
            "No destination writes are intended during preflight" => {
                "Während der Vorabprüfung sind keine Schreibvorgänge im Ziel vorgesehen."
            }
            "resolved from PATH" => "über PATH aufgelöst",
            "Folder inventory: {} source · {} destination" => {
                "Ordnerinventar: {} Quelle · {} Ziel"
            }
            "Message count and data volume are not known yet" => {
                "Nachrichtenanzahl und Datenvolumen sind noch unbekannt."
            }
            "Authenticated in the latest readiness check" => {
                "Bei der letzten Bereitschaftsprüfung authentifiziert"
            }
            "Not yet verified" => "Noch nicht geprüft",
            "standard-folder automapping enabled" => {
                "Automatische Standardordnerzuordnung aktiviert"
            }
            "standard-folder automapping disabled" => {
                "Automatische Standardordnerzuordnung deaktiviert"
            }
            "namespace prefix/delimiter warning" => "Warnung zu Namespace-Präfix oder Trennzeichen",
            "namespace mapping not yet assessed" => "Namespace-Zuordnung noch nicht bewertet",
            "personal namespaces match" => "Persönliche Namespaces stimmen überein",
            "shared or other-user namespaces detected" => {
                "Gemeinsame Namespaces oder Namespaces anderer Benutzer erkannt"
            }
            "version checked at readiness" => "Version bei Bereitschaftsprüfung ermittelt",
            "engine version not yet checked" => "Engine-Version noch nicht geprüft",
            "One mailbox plan template" => "Planvorlage für ein Postfach",
            "{} selected of {} queued mailbox(es)" => {
                "{} von {} eingereihten Postfächern ausgewählt"
            }
            "High: destination quota is currently exhausted" => {
                "Hoch: Das Zielkontingent ist derzeit ausgeschöpft."
            }
            "Medium: personal namespace prefixes or delimiters differ" => {
                "Mittel: Namespace- oder Verhalten gemeinsamer Ordner muss geprüft werden."
            }
            "Medium: namespace or shared-folder behavior needs review" => {
                "Mittel: Namespace- oder Verhalten gemeinsamer Ordner muss geprüft werden."
            }
            "Readiness pending: authenticate both endpoints" => {
                "Bereitschaft ausstehend: Beide Endpunkte authentifizieren."
            }
            "Inventory pending: mailbox scale is unknown" => {
                "Inventar ausstehend: Der Postfachumfang ist unbekannt."
            }
            "No risk is established yet; provider-specific review is still required" => {
                "Es wurde noch kein Risiko festgestellt; eine anbieterspezifische Prüfung ist weiterhin erforderlich."
            }
            "OAuth 2.0" => "OAuth-2.0-Verfahren",
            "Password / app password" => "Passwort / App-Passwort",
            "Configured authentication" => "Konfigurierte Authentifizierung",
            "Preflight → small pilot → seed → catch-up → final delta → independent verification" => {
                "Vorabprüfung → kleiner Pilot → Erstübertragung → Nachlauf → finales Delta → unabhängige Verifizierung"
            }
            "Configure endpoints and credentials before running a dry preflight." => {
                "Konfigurieren Sie Endpunkte und Zugangsdaten, bevor Sie eine Vorabprüfung ausführen."
            }
            "Plan name" => "Planname",
            "DURABLE WORKSPACE UNAVAILABLE" => "DAUERHAFTER ARBEITSBEREICH NICHT VERFÜGBAR",
            "MailSwiftSync cannot open its durable workspace. This session is temporary; live migrations are disabled to protect recovery history." => {
                "MailSwiftSync kann den dauerhaften Arbeitsbereich nicht öffnen. Diese Sitzung ist temporär; Live-Migrationen sind zum Schutz des Wiederherstellungsverlaufs deaktiviert."
            }
            "State file:" => "Statusdatei:",
            "Copy state path" => "Statuspfad kopieren",
            "Open Activity diagnostics" => "Aktivitätsdiagnose öffnen",
            "Unavailable" => "Nicht verfügbar",
            "Plan tools" => "Planwerkzeuge",
            "Assess configuration" => "Konfiguration bewerten",
            "Testing accounts…" => "Konten werden getestet…",
            "Review the proposed plan before testing either account." => {
                "Prüfen Sie den vorgeschlagenen Plan, bevor Sie eines der Konten testen."
            }
            "Authenticate both IMAP accounts and inspect folder namespaces." => {
                "Authentifizieren Sie beide IMAP-Konten und prüfen Sie die Ordner-Namespaces."
            }
            "Run the engine's non-writing preflight after account testing." => {
                "Führen Sie nach dem Kontentest die schreibgeschützte Engine-Vorabprüfung aus."
            }
            "The exact plan passed dry preflight; live execution still requires explicit confirmation." => {
                "Der genaue Plan hat die Probelauf-Vorabprüfung bestanden; die Live-Ausführung erfordert weiterhin eine ausdrückliche Bestätigung."
            }
            "Migration method" => "Migrationsmethode",
            "Standard IMAP migration (imapsync)" => "Standard-IMAP-Migration (imapsync)",
            "Local Dovecot migration (doveadm)" => "Lokale Dovecot-Migration (doveadm)",
            "Advanced method: runs local Dovecot tools and follows Dovecot-specific destination semantics." => {
                "Erweiterte Methode: Führt lokale Dovecot-Werkzeuge aus und folgt der Dovecot-spezifischen Zielsemantik."
            }
            "Recommended for provider-to-provider moves; runs the imapsync engine." => {
                "Für Migrationen zwischen Anbietern empfohlen; verwendet die imapsync-Engine."
            }
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
            "Destination policy: {}" => "Zielrichtlinie: {}",
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
            "Could not create project: {error}" => "Projekt konnte nicht erstellt werden: {error}",
            "Preflight input is invalid: {error}" => "Vorprüfungsdaten sind ungültig: {error}",
            "Source readiness probe blocked: {error}" => {
                "Quell-Bereitschaftsprüfung blockiert: {error}"
            }
            "Destination readiness probe blocked: {error}" => {
                "Ziel-Bereitschaftsprüfung blockiert: {error}"
            }
            "Live authentication probe blocked: {error}" => {
                "Live-Authentifizierungsprüfung blockiert: {error}"
            }
            "⚠ Execution plan validation failed: {error}" => {
                "⚠ Validierung des Ausführungsplans fehlgeschlagen: {error}"
            }
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
            "Enable bounded body-content verification (forensic)" => {
                "Begrenzte Inhaltsprüfung der Nachrichtentexte aktivieren (forensisch)"
            }
            "Downloads and hashes message bodies from both accounts. It is opt-in, bounded, and requires a stable metadata-preserving plan." => {
                "Lädt Nachrichtentexte aus beiden Konten herunter und hasht sie. Dies ist optional, begrenzt und erfordert einen stabilen, metadatenerhaltenden Plan."
            }
            "Body proof is resource-intensive. The run will stop rather than exceed either byte bound; successful evidence is labeled BodyHash." => {
                "Der Inhaltsnachweis benötigt viele Ressourcen. Der Lauf stoppt, bevor eine Byte-Grenze überschritten wird; erfolgreiche Nachweise werden als BodyHash gekennzeichnet."
            }
            "Maximum body bytes per message" => "Maximale Body-Bytes pro Nachricht",
            "Maximum body bytes per verification" => "Maximale Body-Bytes pro Verifizierung",
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
            "Choose migration engine" => "Migrations-Engine auswählen",
            "How should this migration run?" => "Wie soll diese Migration ausgeführt werden?",
            "Select the execution engine that fits the destination. MailSwiftSync owns planning, safety gates, orchestration, and verification; the selected engine owns message transfer." => {
                "Wählen Sie die zum Ziel passende Ausführungs-Engine. MailSwiftSync übernimmt Planung, Sicherheitsprüfungen, Orchestrierung und Verifizierung; die ausgewählte Engine übernimmt die Nachrichtenübertragung."
            }
            "Config" => "Konfiguration",
            "Native Dovecot execution is local-only until a secret-safe broker is implemented." => {
                "Native Dovecot-Ausführung ist nur lokal möglich, bis ein geheimnissicherer Broker implementiert ist."
            }
            "Dry mode only lists the destination mailbox. Native Dovecot uses the selected migration strategy; backup and sync -1 have different merge behavior." => {
                "Der Probelauf listet nur das Zielpostfach. Native Dovecot-Ausführung verwendet die gewählte Migrationsstrategie; backup und sync -1 haben unterschiedliches Zusammenführungsverhalten."
            }
            "Engine and execution settings are locked while a migration is running." => {
                "Engine- und Ausführungseinstellungen sind während einer laufenden Migration gesperrt."
            }
            "Continue to migration plan" => "Zum Migrationsplan",
            "Choose worksheet" => "Arbeitsblatt auswählen",
            "Workspace" => "Arbeitsbereich",
            "No project selected" => "Kein Projekt ausgewählt",
            "Task center" => "Aufgabenzentrale",
            "Migration running" => "Migration läuft",
            "No active migration" => "Keine aktive Migration",
            "Select the migration worksheet" => "Migrationsarbeitsblatt auswählen",
            "{} contains {} worksheet(s). Choose the sheet with the mailbox headers." => {
                "{} enthält {} Arbeitsblatt/Arbeitsblätter. Wählen Sie das Blatt mit den Postfachüberschriften."
            }
            "Select a worksheet" => "Arbeitsblatt auswählen",
            "The selected worksheet is parsed and validated in the background. Other worksheets are not imported." => {
                "Das ausgewählte Arbeitsblatt wird im Hintergrund eingelesen und validiert. Andere Arbeitsblätter werden nicht importiert."
            }
            "Import selected worksheet" => "Ausgewähltes Arbeitsblatt importieren",
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
            "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace." => {
                "Dies entfernt {} Postfachzeile(n), die Auswahl, Passwörter im Speicher und die dauerhafte Stapelzuordnung aus diesem Arbeitsbereich."
            }
            "Keep queue" => "Warteschlange behalten",
            "Clear queue" => "Warteschlange leeren",
            "Replace mailbox queue?" => "Postfachwarteschlange ersetzen?",
            "Importing {} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association." => {
                "Der Import von {} ersetzt {} aktuelle(n) Postfachzeile(n), die Auswahl, Passwörter im Speicher und die dauerhafte Stapelzuordnung."
            }
            "Keep current queue" => "Aktuelle Warteschlange behalten",
            "Replace queue" => "Warteschlange ersetzen",
            "Source preset" => "Quellvorgabe",
            "Destination preset" => "Zielvorgabe",
            "Generic IMAP preset" => "Allgemeine IMAP-Vorgabe",
            "cPanel / Dovecot preset" => "cPanel- / Dovecot-Vorgabe",
            "Where are you migrating from?" => "Von welchem System migrieren Sie?",
            "Where are you migrating to?" => "Zu welchem System migrieren Sie?",
            "Advanced migration settings" => "Erweiterte Migrationseinstellungen",
            "Advanced connection settings" => "Erweiterte Verbindungseinstellungen",
            "Advanced engine options" => "Erweiterte Engine-Optionen",
            "OAuth setup guidance" => "OAuth-Einrichtungshinweise",
            "Account authorization" => "Kontozugriff",
            "Expand account authorization to add a password or OAuth token." => {
                "Öffnen Sie den Kontozugriff, um ein Passwort oder OAuth-Token hinzuzufügen."
            }
            "Test accounts and inspect namespaces" => "Konten testen und Namespaces prüfen",
            "Choose the systems and accounts involved in this migration." => {
                "Wählen Sie die Quell- und Zielsysteme sowie die zugehörigen Konten aus."
            }
            "Use a provider-issued access token with IMAP scope. Tokens stay in this session unless stored in the OS keyring. For automatic refresh, run `mailswiftsync oauth-authorize` and add its keyring ID in Advanced migration settings." => {
                "Verwenden Sie ein vom Anbieter ausgestelltes Zugriffstoken mit IMAP-Berechtigung. Token bleiben in dieser Sitzung, sofern sie nicht im OS-Schlüsselbund gespeichert werden. Für automatische Erneuerung führen Sie `mailswiftsync oauth-authorize` aus und fügen die Schlüsselbund-ID unter den erweiterten Migrationseinstellungen hinzu."
            }
            "Enter the cPanel mail server for this domain. cPanel endpoints vary by hosting provider; confirm the hostname in the hosting account." => {
                "Geben Sie den cPanel-Mailserver dieser Domain ein. cPanel-Endpunkte unterscheiden sich je nach Hosting-Anbieter; prüfen Sie den Hostnamen im Hosting-Konto."
            }
            "Prepare" => "Vorbereiten",
            "Cutover" => "Umschaltung",
            "Verify" => "Prüfen",
            "Begin in Prepare: choose the source and destination, then test both accounts before preflight." => {
                "Beginnen Sie unter Vorbereiten: Wählen Sie Quelle und Ziel und testen Sie beide Konten vor der Vorabprüfung."
            }
            "For a batch, import the mailbox list and begin with a small pilot during the Pilot stage." => {
                "Importieren Sie für einen Stapel die Postfachliste und beginnen Sie in der Pilotphase mit einem kleinen Pilotlauf."
            }
            "Begin in Prepare by choosing the source, destination, and account access." => {
                "Beginnen Sie unter Vorbereiten mit der Auswahl von Quelle, Ziel und Kontozugriff."
            }
            "Google Workspace preset" => "Google-Workspace-Vorgabe",
            "Microsoft 365 preset" => "Microsoft-365-Vorgabe",
            "Fastmail preset" => "Fastmail-Vorgabe",
            "Zoho Mail preset" => "Zoho-Mail-Vorgabe",
            "Enter the provider's documented IMAP endpoint and authentication policy." => {
                "Geben Sie den dokumentierten IMAP-Endpunkt und die Authentifizierungsrichtlinien des Anbieters ein."
            }
            "OAuth is preferred; Workspace administrators may use delegated gmail.imap_admin access. Register your own OAuth client, then run `mailswiftsync oauth-authorize google` to store a refresh token." => {
                "OAuth wird bevorzugt. Workspace-Administratoren können delegierten gmail.imap_admin-Zugriff verwenden. Registrieren Sie einen eigenen OAuth-Client und führen Sie dann `mailswiftsync oauth-authorize google` aus, um ein Refresh-Token zu speichern."
            }
            "Register your own OAuth application, then run `mailswiftsync oauth-authorize microsoft` to store a refresh token; live launches refresh access tokens automatically. Exchange Online IMAP and tenant OAuth policy must permit the account." => {
                "Registrieren Sie eine eigene OAuth-Anwendung und führen Sie dann `mailswiftsync oauth-authorize microsoft` aus, um ein Refresh-Token zu speichern; Live-Starts erneuern Zugriffstoken automatisch. Exchange-Online-IMAP- und Tenant-OAuth-Richtlinien müssen das Konto zulassen."
            }
            "Use a Fastmail app password, not the primary account password; create it in Fastmail security settings before preflight." => {
                "Verwenden Sie ein Fastmail-App-Passwort statt des primären Kontopassworts. Erstellen Sie es vor der Vorabprüfung in den Fastmail-Sicherheitseinstellungen."
            }
            "Zoho region and account policy may change the endpoint; confirm the documented IMAP host before preflight." => {
                "Region und Kontorichtlinien von Zoho können den Endpunkt beeinflussen. Prüfen Sie vor der Vorabprüfung den dokumentierten IMAP-Host."
            }
            "Profile saved without credential material." => "Profil ohne Zugangsdaten gespeichert.",
            "Could not save profile" => "Profil konnte nicht gespeichert werden",
            "Saved OAuth credential configured; session token not required." => {
                "Gespeicherte OAuth-Zugangsdaten konfiguriert; Sitzungstoken nicht erforderlich."
            }
            "Saved credential configured; session password not required." => {
                "Gespeicherte Zugangsdaten konfiguriert; Sitzungspasswort nicht erforderlich."
            }
            "Source credential stored in OS keyring" => {
                "Quellzugangsdaten im Betriebssystem-Schlüsselbund gespeichert"
            }
            "Source credential loaded" => "Quellzugangsdaten geladen",
            "Source credential deleted from OS keyring" => {
                "Quellzugangsdaten aus dem Betriebssystem-Schlüsselbund gelöscht"
            }
            "Destination credential stored in OS keyring" => {
                "Zielzugangsdaten im Betriebssystem-Schlüsselbund gespeichert"
            }
            "Destination credential loaded" => "Zielzugangsdaten geladen",
            "Destination credential deleted from OS keyring" => {
                "Zielzugangsdaten aus dem Betriebssystem-Schlüsselbund gelöscht"
            }
            "This is password storage, not OAuth/Modern Auth. Do not use it as a substitute for provider-specific OAuth setup or unattended secret brokering." => {
                "Dies ist eine Passwortablage, keine OAuth-/Modern-Auth-Konfiguration. Verwenden Sie sie nicht als Ersatz für anbieterspezifische OAuth-Einrichtung oder unbeaufsichtigte Zugangsdatenvermittlung."
            }
            "Source OAuth refresh configuration deleted" => {
                "OAuth-Erneuerungskonfiguration der Quelle gelöscht"
            }
            "Destination OAuth refresh configuration deleted" => {
                "OAuth-Erneuerungskonfiguration des Ziels gelöscht"
            }
            "Delete saved credential?" => "Gespeicherte Zugangsdaten löschen?",
            "{}: {} · keyring ID: {}" => "{}: {} · Schlüsselbund-ID: {}",
            "This permanently removes the saved credential from the OS keyring." => {
                "Dadurch werden die gespeicherten Zugangsdaten dauerhaft aus dem Betriebssystem-Schlüsselbund entfernt."
            }
            "This removes only the locally stored OAuth refresh configuration; it does not revoke the provider token." => {
                "Dadurch wird nur die lokal gespeicherte OAuth-Erneuerungskonfiguration entfernt; das Anbietertoken wird nicht widerrufen."
            }
            "The profile reference and any credential already loaded in this session are not changed." => {
                "Der Profilverweis und bereits in dieser Sitzung geladene Zugangsdaten bleiben unverändert."
            }
            "This permanently removes the selected item from the OS keyring. The profile reference and any credential already loaded in this session are not changed." => {
                "Dieser Vorgang entfernt den ausgewählten Eintrag dauerhaft aus dem OS-Schlüsselbund. Der Profilverweis und bereits in dieser Sitzung geladene Zugangsdaten bleiben unverändert."
            }
            "Delete saved credential" => "Gespeicherte Zugangsdaten löschen",
            "saved password / access token" => "gespeichertes Passwort / Zugriffstoken",
            "automatic OAuth refresh configuration" => {
                "automatische OAuth-Erneuerungskonfiguration"
            }
            "Refreshing OAuth token…" => "OAuth-Token wird erneuert…",
            "Refreshing OAuth token in the background…" => {
                "OAuth-Token wird im Hintergrund erneuert…"
            }
            "OAuth refresh worker stopped before returning a result." => {
                "Der OAuth-Erneuerungsprozess wurde vor der Rückgabe eines Ergebnisses beendet."
            }
            "OAuth refresh finished after account settings changed; its access token was discarded. Review the account and refresh again." => {
                "Die OAuth-Erneuerung endete nach einer Änderung der Kontoeinstellungen; das Zugriffstoken wurde verworfen. Prüfen und erneuern Sie das Konto erneut."
            }
            "{} OAuth access token refreshed (expires in {}s)" => {
                "{} OAuth-Zugriffstoken erneuert (läuft in {} s ab)"
            }
            "{} OAuth access token refreshed" => "{} OAuth-Zugriffstoken erneuert",
            "No automatic refresh is configured for the {}" => {
                "Für {} ist keine automatische Erneuerung konfiguriert"
            }
            "{} OAuth refresh configuration stored in OS keyring" => {
                "OAuth-Erneuerungskonfiguration für {} im Betriebssystem-Schlüsselbund gespeichert"
            }
            "Server" => "Server",
            "User" => "Benutzer",
            "{} is required." => "{} ist erforderlich.",
            "{} contains an invalid control character." => {
                "{} enthält ein ungültiges Steuerzeichen."
            }
            "Authentication" => "Authentifizierung",
            "Local Dovecot account" => "Lokales Dovecot-Konto",
            "IMAP connection" => "IMAP-Verbindung",
            "Use a currently valid provider-issued access token with IMAP scope. Tokens are session-only unless stored in the OS keyring. To avoid manual tokens, run `mailswiftsync oauth-authorize` once and enter its keyring ID in the OS keyring dialog's Automatic OAuth refresh section." => {
                "Verwenden Sie ein gültiges, vom Anbieter ausgestelltes Zugriffstoken mit IMAP-Berechtigung. Token gelten nur für die Sitzung, sofern sie nicht im Betriebssystem-Schlüsselbund gespeichert werden. Um manuelle Token zu vermeiden, führen Sie einmal `mailswiftsync oauth-authorize` aus und tragen Sie dessen Schlüsselbund-ID im Abschnitt zur automatischen OAuth-Erneuerung des Schlüsselbunddialogs ein."
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
            "Open migration plan  →" => "Migrationsplan öffnen  →",
            "Review preflight  →" => "Vorabprüfung prüfen  →",
            "Review migration plan  →" => "Migrationsplan prüfen  →",
            "Open Activity  →" => "Aktivität öffnen  →",
            "Review mailboxes  →" => "Postfächer prüfen  →",
            "Open customer proof  →" => "Kundennachweis öffnen  →",
            "Review pilot activity  →" => "Pilotaktivität prüfen  →",
            "Open mailbox actions  →" => "Postfachaktionen öffnen  →",
            "Run final delta  →" => "Letzte Delta-Synchronisierung starten  →",
            "Open verification  →" => "Verifizierung öffnen  →",
            "Review batch mailboxes  →" => "Stapelpostfächer prüfen  →",
            "Refresh preflight assessment" => "Vorabprüfung aktualisieren",
            "Preflight is the default" => "Vorabprüfung ist der Standard",
            "Saved profiles exclude passwords" => "Gespeicherte Profile enthalten keine Passwörter",
            "Source mail is read-only by default" => {
                "Quellnachrichten sind standardmäßig schreibgeschützt"
            }
            "{} shown · {} total need review" => "{} angezeigt · insgesamt {} erfordern Prüfung",
            "{}/{} configuration items complete" => "{}/{} Konfigurationselemente abgeschlossen",
            "Process review acknowledged; execution gates are available again." => {
                "Prozessprüfung bestätigt; Ausführungsfreigaben sind wieder verfügbar."
            }
            "Could not clear reviewed process identities: {error}" => {
                "Geprüfte Prozesskennungen konnten nicht gelöscht werden: {error}"
            }
            "Readiness observations expired because the migration plan changed; run discovery again." => {
                "Die Bereitschaftsbeobachtungen sind abgelaufen, weil sich der Migrationsplan geändert hat. Führen Sie die Erkennung erneut aus."
            }
            "Project reopened for documented review" => {
                "Projekt für dokumentierte Prüfung erneut geöffnet"
            }
            "Could not reopen project: {error}" => {
                "Projekt konnte nicht erneut geöffnet werden: {error}"
            }
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
            "Authenticate and review blockers" => "Authentifizieren und Blockierungen prüfen",
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
            "MIGRATION LIFECYCLE" => "MIGRATIONSLEBENSZYKLUS",
            "ATTENTION REQUIRED" => "PRÜFUNG ERFORDERLICH",
            "A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved." => {
                "Ein Postfach oder eine Ausführung erfordert eine Prüfung durch den Betreiber. Der normale Lebenszyklus ist bis zur Klärung angehalten."
            }
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
            "Evidence includes bounded RFC822 body fingerprints from both accounts; provider-specific qualification remains required." => {
                "Die Nachweise enthalten begrenzte RFC822-Körper-Fingerprints beider Konten; eine anbieterspezifische Qualifizierung bleibt erforderlich."
            }
            "Evidence labels describe metadata and aggregate reconciliation; message bodies were not compared." => {
                "Die Nachweisbezeichnungen beschreiben Metadaten- und aggregierten Abgleich; Nachrichtenkörper wurden nicht verglichen."
            }
            "Export verification report…" => "Verifizierungsbericht exportieren …",
            "Accept residual difference" => "Verbleibende Abweichung akzeptieren",
            "Operator" => "Betreiber",
            "Durable run history" => "Dauerhafte Ausführungshistorie",
            "Filter history" => "Historie filtern",
            "No durable runs recorded yet." => {
                "Bisher sind keine dauerhaften Ausführungen erfasst."
            }
            "Worksheet selection cancelled; no rows were imported." => {
                "Arbeitsblattauswahl abgebrochen; es wurden keine Zeilen importiert."
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
            "Review, filter, select, and operate on customer mailboxes." => {
                "Prüfen, filtern, auswählen und bearbeiten Sie Kundenpostfächer."
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
            "{} mailbox jobs in scope" => "{} Postfachaufträge im Umfang",
            "{} imported" => "{} importiert",
            "{} queued" => "{} wartend",
            "{} preflight" => "{} Vorabprüfung",
            "{} ready" => "{} bereit",
            "{} running" => "{} läuft",
            "{} verified" => "{} verifiziert",
            "{} attention" => "{} Prüfung erforderlich",
            "{} failed" => "{} fehlgeschlagen",
            "{} delta required" => "{} Delta erforderlich",
            "{} unresolved" => "{} offen",
            "{} imported · {} queued · {} preflight · {} ready · {} attention · {} unresolved" => {
                "{} importiert · {} wartend · {} Vorabprüfung · {} bereit · {} Prüfung erforderlich · {} offen"
            }
            "{} total" => "{} insgesamt",
            "{} ready · {} running · {} verified" => "{} bereit · {} läuft · {} verifiziert",
            "{} require operator attention" => "{} erfordern die Aufmerksamkeit des Betreibers",
            "… plus {} more on this page" => "… plus {} weitere auf dieser Seite",
            "{} project(s)" => "{} Projekt(e)",
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
            "{} selected mailbox(es) are unavailable for live migration." => {
                "{} ausgewählte Postfach/Postfächer sind für eine Live-Migration nicht verfügbar."
            }
            "{} selected mailbox(es) are not marked as requiring a final delta." => {
                "{} ausgewählte Postfach/Postfächer sind nicht als finales Delta erforderlich markiert."
            }
            "Selected mailbox actions" => "Aktionen für ausgewählte Postfächer",
            "⚠ {} mailbox(es) are selected but hidden by the current filter. They will still be included in batch operations." => {
                "⚠ {} Postfach/er sind ausgewählt, aber durch den aktuellen Filter verborgen. Sie bleiben in Stapelvorgängen enthalten."
            }
            "Review selected ({})" => "Auswahl prüfen ({})",
            "{} selected" => "{} ausgewählt",
            "{} ready for pilot" => "{} bereit für den Pilotlauf",
            "{} require operator review" => "{} erfordern eine Betreiberprüfung",
            "{} in other states; not classified as ready or review-blocked" => {
                "{} in anderen Status; weder als bereit noch als prüfblockiert eingestuft"
            }
            "{} selected rows are unavailable in the loaded queue." => {
                "{} ausgewählte Zeilen sind in der geladenen Warteschlange nicht verfügbar."
            }
            "Size and completion-time estimates require inventory and throughput data not recorded for these queue rows." => {
                "Größen- und Zeitprognosen benötigen Bestands- und Durchsatzdaten, die für diese Warteschlangen nicht vorliegen."
            }
            "The selected mailbox is not present in the current queue." => {
                "Das ausgewählte Postfach ist nicht in der aktuellen Warteschlange vorhanden."
            }
            "Migration behavior" => "Migrationsverhalten",
            "Recorded verification inventory" => "Aufgezeichneter Verifizierungsbestand",
            "{} folders · {} messages · {}" => "{} Ordner · {} Nachrichten · {}",
            "Recorded plan snapshot digest: {}" => "Prüfsumme des gespeicherten Plans: {}",
            "View complete assessment" => "Vollständige Bewertung anzeigen",
            "Selected mailbox scope remains explicit while this drawer is open." => {
                "Der ausgewählte Postfachumfang bleibt ausdrücklich, solange diese Leiste geöffnet ist."
            }
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
            // Fixed status-bar messages (translated via `lookup`).
            "Idle" => "Bereit",
            "A migration is running; finish or stop it before starting a new workspace." => {
                "Eine Migration läuft; schließen Sie sie ab oder stoppen Sie sie, bevor Sie einen neuen Arbeitsbereich beginnen."
            }
            "Authenticated capability discovery requires encrypted IMAP; plain transport remains blocked by the explicit cleartext acknowledgement gate." => {
                "Die authentifizierte Fähigkeitsprüfung erfordert verschlüsseltes IMAP; unverschlüsselter Transport bleibt durch die ausdrückliche Klartext-Bestätigung gesperrt."
            }
            "Authenticated dual-endpoint IMAPS probing is for imapsync mode; Dovecot destination readiness is checked by the native dry preflight." => {
                "Die authentifizierte IMAPS-Prüfung beider Endpunkte gilt für den imapsync-Modus; die Bereitschaft eines Dovecot-Ziels prüft die native Vorabprüfung."
            }
            "Authenticating and inspecting IMAPS readiness…" => {
                "IMAPS-Bereitschaft wird authentifiziert und geprüft…"
            }
            "Capability discovery complete" => "Fähigkeitsprüfung abgeschlossen",
            "Could not start without a durable mailbox project." => {
                "Start ohne dauerhaftes Postfachprojekt nicht möglich."
            }
            "Durable state view is stale; execution is disabled until SQLite refresh succeeds." => {
                "Die dauerhafte Statusansicht ist veraltet; die Ausführung bleibt gesperrt, bis die SQLite-Aktualisierung gelingt."
            }
            "Enter source and destination hosts before creating a project." => {
                "Geben Sie Quell- und Zielserver ein, bevor Sie ein Projekt erstellen."
            }
            "Fresh IMAPS authentication passed; continuing live admission…" => {
                "Neue IMAPS-Authentifizierung erfolgreich; Live-Freigabe wird fortgesetzt…"
            }
            "Live migration blocked: acknowledge the cleartext source-transport risk before continuing." => {
                "Live-Migration gesperrt: Bestätigen Sie das Risiko des unverschlüsselten Quelltransports, bevor Sie fortfahren."
            }
            "Live migration blocked: imapsync automapping has no immutable mapping snapshot for independent verification. Disable automap and run a new preflight before migrating." => {
                "Live-Migration gesperrt: Die automatische imapsync-Ordnerzuordnung hat keinen unveränderlichen Zuordnungsstand für die unabhängige Verifizierung. Deaktivieren Sie automap und führen Sie vor der Migration eine neue Vorabprüfung aus."
            }
            "Live migration is disabled because durable SQLite storage is unavailable." => {
                "Die Live-Migration ist deaktiviert, weil der dauerhafte SQLite-Speicher nicht verfügbar ist."
            }
            "Loading migration credentials from the OS keyring…" => {
                "Zugangsdaten für die Migration werden aus dem Betriebssystem-Schlüsselbund geladen…"
            }
            "New migration workspace ready; configure the endpoints before preflight." => {
                "Neuer Migrationsarbeitsbereich bereit; konfigurieren Sie die Endpunkte vor der Vorabprüfung."
            }
            "Project created; ready for preflight review" => {
                "Projekt erstellt; bereit für die Vorabprüfung"
            }
            "Project switching is disabled while a migration is running." => {
                "Während eine Migration läuft, kann das Projekt nicht gewechselt werden."
            }
            "Re-authenticating encrypted IMAP endpoints before live execution…" => {
                "Verschlüsselte IMAP-Endpunkte werden vor der Live-Ausführung erneut authentifiziert…"
            }
            "The current mailbox identity differs from the durable project. Run a new dry preflight for this plan before starting live migration." => {
                "Die aktuelle Postfachidentität weicht vom dauerhaften Projekt ab. Führen Sie für diesen Plan eine neue Vorabprüfung aus, bevor Sie die Live-Migration starten."
            }
            "The migration plan changed while credentials were loading; review it and start again." => {
                "Der Migrationsplan hat sich beim Laden der Zugangsdaten geändert; prüfen Sie ihn und starten Sie erneut."
            }
            "The saved Dovecot checkpoint has no UIDVALIDITY context; run a fresh full Dovecot pass before resuming." => {
                "Der gespeicherte Dovecot-Prüfpunkt hat keinen UIDVALIDITY-Kontext; führen Sie vor dem Fortsetzen einen neuen vollständigen Dovecot-Durchlauf aus."
            }
            "Viewing a historical project read-only. Start a new migration to edit or execute a plan." => {
                "Historisches Projekt schreibgeschützt geöffnet. Starten Sie eine neue Migration, um einen Plan zu bearbeiten oder auszuführen."
            }
            // Destination mutation policy.
            "DESTINATION STATE MAY BE REMOVED" => "ZIELZUSTAND KANN ENTFERNT WERDEN",
            "Destination-only messages and mailboxes are kept for every selected mailbox." => {
                "Nur im Ziel vorhandene Nachrichten und Postfächer bleiben für jedes ausgewählte Postfach erhalten."
            }
            "⚠ Selected destination names differ only by case on a provider whose account-name case rules are unknown. They may be the same mailbox; verify the provider identities before proceeding." => {
                "⚠ Ausgewählte Zielnamen unterscheiden sich nur durch Groß-/Kleinschreibung; die Regeln des Anbieters sind unbekannt. Es könnte dasselbe Postfach sein. Prüfen Sie die Konten vor dem Fortfahren."
            }
            "⚠ Selected destination names differ only by case on a provider whose account-name case rules are unknown. They may be the same mailbox; verify the provider identities before proceeding. Acknowledged case-only aliases must run at concurrency 1." => {
                "⚠ Ausgewählte Zielnamen unterscheiden sich nur durch Groß-/Kleinschreibung; die Regeln des Anbieters sind unbekannt. Es könnte dasselbe Postfach sein. Prüfen Sie die Konten vor dem Fortfahren. Bestätigte Namensvarianten müssen mit Parallelität 1 ausgeführt werden."
            }
            "I reviewed these destination accounts and acknowledge the possible identity collision" => {
                "Ich habe diese Zielkonten geprüft und bestätige die mögliche Identitätskollision"
            }
            "I confirm destination-only mail may be removed for these mailboxes" => {
                "Ich bestätige, dass nur im Ziel vorhandene E-Mails für diese Postfächer entfernt werden können"
            }
            "I confirm destination-only mail may be removed for this mailbox" => {
                "Ich bestätige, dass nur im Ziel vorhandene E-Mails für dieses Postfach entfernt werden können"
            }
            "⚠ {} selected mailbox(es) use destination mirror or --delete2: destination-only messages/mailboxes may be removed or replaced." => {
                "⚠ {} ausgewählte(s) Postfach/Postfächer nutzen Zielspiegelung oder --delete2: Nur im Ziel vorhandene Nachrichten/Postfächer können entfernt oder ersetzt werden."
            }
            "Additive copy" => "Hinzufügende Kopie",
            "Merge preserving destination" => "Zusammenführung mit Erhalt des Ziels",
            "Destination mirror" => "Zielspiegelung",
            "Delete destination-only messages" => "Nur im Ziel vorhandene Nachrichten löschen",
            "Additive copy — destination-only messages and mailboxes are kept." => {
                "Hinzufügende Kopie — nur im Ziel vorhandene Nachrichten und Postfächer bleiben erhalten."
            }
            "Merge preserving destination — destination-side changes are kept; review merge conflicts." => {
                "Zusammenführung mit Erhalt des Ziels — Änderungen im Ziel bleiben erhalten; prüfen Sie Zusammenführungskonflikte."
            }
            "Destination mirror mode — destination-only messages/mailboxes may be removed or replaced." => {
                "Zielspiegelung — nur im Ziel vorhandene Nachrichten/Postfächer können entfernt oder ersetzt werden."
            }
            "Destination deletion (--delete2) — destination messages missing from the source will be removed." => {
                "Löschen im Ziel (--delete2) — Nachrichten im Ziel, die in der Quelle fehlen, werden entfernt."
            }
            "Dismiss" => "Ausblenden",
            "Queue settings" => "Warteschlangeneinstellungen",
            "Concurrent workers" => "Parallele Prozesse",
            "Applies to preflight and live migration." => {
                "Gilt für Vorabprüfung und Live-Migration."
            }
            "Authentication and configuration failures are never retried." => {
                "Authentifizierungs- und Konfigurationsfehler werden nie wiederholt."
            }
            "{} selected · {} visible · {} hidden by current filter" => {
                "{} ausgewählt · {} sichtbar · {} durch aktuellen Filter verborgen"
            }
            "Export selected set…" => "Ausgewählte Zeilen exportieren…",
            "Writes the selected rows as JSON without credentials or engine options." => {
                "Schreibt die ausgewählten Zeilen als JSON ohne Zugangsdaten oder Engine-Optionen."
            }
            "Selected batch rows exported without credentials or engine options." => {
                "Ausgewählte Stapelzeilen ohne Zugangsdaten oder Engine-Optionen exportiert."
            }
            "Select one or more rows to export." => {
                "Wählen Sie eine oder mehrere Zeilen zum Exportieren aus."
            }
            "Batch selection export cancelled." => "Export der Stapelauswahl abgebrochen.",
            "Enter a keyring ID before applying it." => {
                "Geben Sie eine Schlüsselbund-ID ein, bevor Sie sie anwenden."
            }
            "Applied the keyring ID to {} row(s) without a credential reference." => {
                "Schlüsselbund-ID auf {} Zeile(n) ohne Zugangsdatenverweis angewendet."
            }
            "Apply an existing OS-keyring reference to rows that do not already have a password or credential ID. The secret itself is never copied into the queue." => {
                "Wendet einen vorhandenen Betriebssystem-Schlüsselbund-Verweis auf Zeilen ohne Passwort oder Zugangsdaten-ID an. Das Geheimnis selbst wird nie in die Warteschlange kopiert."
            }
            "Apply to empty source rows" => "Auf Quellzeilen ohne Zugangsdaten anwenden",
            "Apply to empty destination rows" => "Auf Zielzeilen ohne Zugangsdaten anwenden",
            "Source keyring ID" => "Schlüsselbund-ID der Quelle",
            "Destination keyring ID" => "Schlüsselbund-ID des Ziels",
            "Imported" => "Importiert",
            "Queued" => "Wartend",
            "Retrying" => "Wird wiederholt",
            "○ Imported" => "○ Importiert",
            "✓ Verified with exceptions" => "✓ Mit Ausnahmen verifiziert",
            "✓ Verified" => "✓ Verifiziert",
            "✓ Completed" => "✓ Abgeschlossen",
            "● Running" => "● Läuft",
            "○ Queued" => "○ Wartend",
            "◌ Preflight" => "◌ Vorabprüfung",
            "↻ Retrying" => "↻ Wird wiederholt",
            "○ Ready" => "○ Bereit",
            "↻ Delta required" => "↻ Delta erforderlich",
            "≠ Verification difference" => "≠ Verifizierungsabweichung",
            "! Attention" => "! Prüfung erforderlich",
            "× Failed" => "× Fehlgeschlagen",
            "× Cancelled" => "× Abgebrochen",
            "? Unknown" => "? Unbekannt",
            "Failed" => "Fehlgeschlagen",
            "Delta required" => "Delta erforderlich",
            "Verified" => "Verifiziert",
            "Ready" => "Bereit",
            "All states" => "Alle Status",
            "Verified with exceptions" => "Mit Ausnahmen verifiziert",
            "… plus {} more selected" => "… plus {} weitere ausgewählt",
            "• {}: {} → {}" => "• {}: {} → {} (ausgewählt)",
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
            "Requires an OAuth application you have registered with the provider. Run `mailswiftsync oauth-authorize` to complete consent in a browser and store the refresh configuration under a keyring ID, or enter a refresh token obtained through the provider's own tooling below. Before each live launch MailSwiftSync exchanges it for a fresh access token." => {
                "Erfordert eine beim Anbieter registrierte OAuth-Anwendung. Führen Sie `mailswiftsync oauth-authorize` aus, um die Zustimmung im Browser zu erteilen und die Erneuerungskonfiguration unter einer Schlüsselbund-ID zu speichern, oder geben Sie unten ein mit den Werkzeugen des Anbieters erhaltenes Refresh-Token ein. Vor jedem Live-Start tauscht MailSwiftSync es gegen ein neues Zugriffstoken."
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
            "Credential settings are locked while a migration is running or OAuth refresh is in progress." => {
                "Zugangsdaten sind während einer laufenden Migration oder OAuth-Erneuerung gesperrt."
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
            "Elapsed {}" => "Dauer {}",
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
            "{} visible of {} loaded" => "{} sichtbar von {} geladen",
            "Showing the newest 250 runs. Export the audit report for complete history." => {
                "Die neuesten 250 Ausführungen werden angezeigt. Exportieren Sie den Prüfbericht für den vollständigen Verlauf."
            }
            "Run" => "Ausführung",
            "Mailbox" => "Postfach",
            "Stage" => "Phase",
            "Started" => "Gestartet",
            "Finished" => "Beendet",
            "Detail" => "Details",
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
            "{} of {} verified · {} require review" => {
                "{} von {} verifiziert · {} erfordern Prüfung"
            }
            "mailbox or destination" => "Postfach oder Ziel",
            "Needs review" => "Prüfung erforderlich",
            "Differences" => "Abweichungen",
            "All results" => "Alle Ergebnisse",
            "Evidence" => "Nachweis",
            "Result" => "Ergebnis",
            "No evidence" => "Kein Nachweis",
            "{} visible on page · showing {}–{} of {}" => {
                "{} auf der Seite sichtbar · {}–{} von {} werden angezeigt"
            }
            "← Previous" => "← Zurück",
            "Next →" => "Weiter →",
            "Assurance" => "Nachweissicherheit",
            "Attention required: this mailbox is not currently safe to close." => {
                "Maßnahme erforderlich: Dieses Postfach ist derzeit nicht sicher abschließbar."
            }
            "Transfer and inventory evidence support this mailbox's current state." => {
                "Übertragungs- und Inventarnachweise stützen den aktuellen Zustand dieses Postfachs."
            }
            "Assurance is incomplete; review the missing facts before proceeding." => {
                "Der Nachweis ist unvollständig; prüfen Sie die fehlenden Fakten vor dem Fortfahren."
            }
            "Transfer" => "Übertragung",
            "completed" => "abgeschlossen",
            "not complete" => "nicht abgeschlossen",
            "Destination reachable" => "Ziel erreichbar",
            "confirmed" => "bestätigt",
            "failed" => "fehlgeschlagen",
            "unknown" => "unbekannt",
            "Inventory reconciled" => "Inventar abgeglichen",
            "not confirmed" => "nicht bestätigt",
            "Message-level evidence" => "Nachweis auf Nachrichtenebene",
            "collected" => "erfasst",
            "not collected" => "nicht erfasst",
            "none recorded" => "keine erfasst",
            "found" => "gefunden",
            "Verification authority" => "Verifizierungsinstanz",
            "Verification method" => "Verifizierungsmethode",
            "Verification outcome" => "Verifizierungsergebnis",
            "aggregate_engine" => "Aggregierte Engine-Prüfung",
            "metadata_reconciliation" => "Metadaten-Abgleich",
            "body_hash" => "Inhalts-Hash",
            "native_dovecot" => "Native Dovecot-Prüfung",
            "Exact body match — bounded RFC822 SHA-256 fingerprints compared" => {
                "Exakte Inhaltsübereinstimmung — begrenzte RFC822-SHA-256-Fingerabdrücke verglichen"
            }
            "Exact metadata match — message bodies not compared" => {
                "Exakte Metadatenübereinstimmung — Nachrichtentexte nicht verglichen"
            }
            "Probable metadata match — message bodies not compared" => {
                "Wahrscheinliche Metadatenübereinstimmung — Nachrichtentexte nicht verglichen"
            }
            "Ambiguous metadata result — message bodies not compared" => {
                "Mehrdeutiges Metadatenergebnis — Nachrichtentexte nicht verglichen"
            }
            "Missing messages detected" => "Fehlende Nachrichten erkannt",
            "Changed messages detected" => "Geänderte Nachrichten erkannt",
            "Unexpected messages detected" => "Unerwartete Nachrichten erkannt",
            "Verification evidence incomplete" => "Verifizierungsnachweise unvollständig",
            "Verification failed" => "Verifizierung fehlgeschlagen",
            "none" => "keine",
            "Accepted by {operator} at {time}: {reason}" => {
                "Akzeptiert von {operator} um {time}: {reason}"
            }
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
            "Verification exception recorded durably" => {
                "Verifizierungs-Ausnahme dauerhaft aufgezeichnet"
            }
            "Could not accept verification exception: {error}" => {
                "Verifizierungs-Ausnahme konnte nicht akzeptiert werden: {error}"
            }
            "No active project selected" => "Kein aktives Projekt ausgewählt",
            "Run a migration to create a durable mailbox evidence record." => {
                "Führen Sie eine Migration aus, um einen dauerhaften Postfachnachweis zu erstellen."
            }
            "The selected mailbox is not present in the cached project snapshot. Refresh the workspace before viewing or exporting its evidence." => {
                "Das ausgewählte Postfach ist nicht im zwischengespeicherten Projektsnapshot enthalten. Aktualisieren Sie den Arbeitsbereich, bevor Sie seine Nachweise anzeigen oder exportieren."
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
            _ => return None,
        })
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
            "Migration still running",
            "This desktop owns the migration controller. Closing the window will interrupt the run and require recovery review; migrations cannot yet continue after the desktop exits.",
            "To stop safely, use Stop migration in Activity and wait for the run to finish before closing.",
            "Keep window open",
            "Go to Activity",
            "I understand — start migration",
            "Confirm live batch migration",
            "I understand — start batch",
            "Sample of selected mailboxes:",
            "View all selected",
            "Each mailbox must already have a matching successful preflight. Source mail is not deleted by default.",
            "Confirmation stale: concurrency, scope, or settings changed while dialog was open. Review the queue and try again.",
            "Review selected ({})",
            "{} selected",
            "{} ready for pilot",
            "{} require operator review",
            "{} selected rows are unavailable in the loaded queue.",
            "Migration behavior",
            "Recorded verification inventory",
            "{} folders · {} messages · {}",
            "Recorded plan snapshot digest: {}",
            "View complete assessment",
            "No mailboxes selected.",
            "DESTRUCTIVE: destination deletion enabled",
            "Select one or more rows to enable actions.",
            "Operator action",
            "Inspect durable run detail",
            "Clear mailbox queue?",
            "Advanced migration options",
            "Review verification",
            "PROVIDER QUALIFICATION",
            "UNQUALIFIED",
            "COMMUNITY / UNQUALIFIED PATH",
            "The generic IMAP migration path is available, but this provider pair has not passed the MailSwiftSync production qualification suite.",
            "Last qualification: none · no live provider evidence is currently bundled.",
            "Qualification requires tested authentication, folder inventory and mapping, independent verification, interruption recovery, throttling recovery, and large-mailbox coverage.",
            "PRE-MIGRATION SIMULATION",
            "SCOPE",
            "RISKS",
            "Plan preview only — estimates are shown only when supported by observed data.",
            "SOURCE",
            "DESTINATION",
            "ENGINE",
            "MAPPING",
            "AUTH",
            "ESTIMATED SCALE",
            "PROPOSED EXECUTION",
            "Read-only; source messages are not deleted by default",
            "No destination writes are intended during preflight",
            "resolved from PATH",
            "Folder inventory: {} source · {} destination",
            "Message count and data volume are not known yet",
            "Authenticated in the latest readiness check",
            "Not yet verified",
            "standard-folder automapping enabled",
            "standard-folder automapping disabled",
            "namespace prefix/delimiter warning",
            "namespace mapping not yet assessed",
            "personal namespaces match",
            "shared or other-user namespaces detected",
            "version checked at readiness",
            "engine version not yet checked",
            "One mailbox plan template",
            "{} selected of {} queued mailbox(es)",
            "High: destination quota is currently exhausted",
            "Medium: namespace or shared-folder behavior needs review",
            "Readiness pending: authenticate both endpoints",
            "Inventory pending: mailbox scale is unknown",
            "No risk is established yet; provider-specific review is still required",
            "OAuth 2.0",
            "Password / app password",
            "Configured authentication",
            "Preflight → small pilot → seed → catch-up → final delta → independent verification",
            "Provider readiness runbook",
            "Read-only operational guidance. Preflight and live admission remain authoritative.",
            "Guidance version: {}",
            "Why:",
            "Success:",
            "Known provider issues",
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
            "Could not create project: {error}",
            "Preflight input is invalid: {error}",
            "Source readiness probe blocked: {error}",
            "Destination readiness probe blocked: {error}",
            "Live authentication probe blocked: {error}",
            "⚠ Execution plan validation failed: {error}",
            "These controls affect the imapsync fallback. Dovecot-native migrations use doveadm and server-side consistency rules.",
            "Delete destination messages missing from source  (--delete2)",
            "Use only for an intentionally exact backup after a tested preflight. This can remove destination mail.",
            "Enable bounded body-content verification (forensic)",
            "Downloads and hashes message bodies from both accounts. It is opt-in, bounded, and requires a stable metadata-preserving plan.",
            "Body proof is resource-intensive. The run will stop rather than exceed either byte bound; successful evidence is labeled BodyHash.",
            "Maximum body bytes per message",
            "Maximum body bytes per verification",
            "The Extra imapsync options field accepts only the documented safe tuning and diagnostic allowlist. Connection, credential, TLS, destructive, logging, and unknown flags are rejected.",
            "Selected mailbox scope remains explicit while this drawer is open.",
            "Choose migration engine",
            "How should this migration run?",
            "Select the execution engine that fits the destination. MailSwiftSync owns planning, safety gates, orchestration, and verification; the selected engine owns message transfer.",
            "Native Dovecot execution is local-only until a secret-safe broker is implemented.",
            "Dry mode only lists the destination mailbox. Native Dovecot uses the selected migration strategy; backup and sync -1 have different merge behavior.",
            "Engine and execution settings are locked while a migration is running.",
            "Continue to migration plan",
            "Choose worksheet",
            "Select the migration worksheet",
            "{} contains {} worksheet(s). Choose the sheet with the mailbox headers.",
            "The selected worksheet is parsed and validated in the background. Other worksheets are not imported.",
            "Import selected worksheet",
            "Workspace",
            "No project selected",
            "Task center",
            "Migration running",
            "No active migration",
            "This removes {} mailbox row(s), selection, in-memory passwords, and the durable batch association from this workspace.",
            "Importing {} will replace {} current mailbox row(s), selection, in-memory passwords, and the durable batch association.",
            "• {}: {} → {}",
            "Export verification report…",
            "Customer proof is ready: the durable project is Complete and no mailbox requires review.",
            "Customer proof remains gated until the durable project is Complete, every mailbox is verified, and the state view is current.",
            "Assurance",
            "Attention required: this mailbox is not currently safe to close.",
            "Transfer and inventory evidence support this mailbox's current state.",
            "Assurance is incomplete; review the missing facts before proceeding.",
            "Destination reachable",
            "Inventory reconciled",
            "Message-level evidence",
            "Verification authority",
            "The transfer finished, but no mailbox-level evidence has been captured yet.",
            "Open migration plan  →",
            "Review preflight  →",
            "Review migration plan  →",
            "Open Activity  →",
            "Review mailboxes  →",
            "Open customer proof  →",
            "Review pilot activity  →",
            "Open mailbox actions  →",
            "Run final delta  →",
            "Open verification  →",
            "Review batch mailboxes  →",
            "Refresh preflight assessment",
            "Preflight is the default",
            "Saved profiles exclude passwords",
            "Source mail is read-only by default",
            "{} shown · {} total need review",
            "{}/{} configuration items complete",
            "Process review acknowledged; execution gates are available again.",
            "Could not clear reviewed process identities: {error}",
            "Readiness observations expired because the migration plan changed; run discovery again.",
            "Project reopened for documented review",
            "Could not reopen project: {error}",
            "{} visible on page · showing {}–{} of {}",
            "MIGRATION LIFECYCLE",
            "ATTENTION REQUIRED",
            "A mailbox or run needs operator review. Normal lifecycle progress is paused until it is resolved.",
            "Elapsed {}",
            "{} visible of {} loaded",
            "Selected {} mailbox row(s) for focused review.",
            "Imported {} mailbox rows. Review them and run preflight before migration.",
            "Choose the worksheet containing the migration rows before importing.",
            "A mailbox file is already being imported.",
            "Importing {} in the background…",
            "Importing the selected worksheet in the background…",
            "Queue cleared; its durable batch association was discarded.",
            "Errors and attention",
            "Running",
            "Completed",
            "Execution completed without a durable run context",
            "Migration result requires durable storage; retrying terminal commit",
            "Migration result requires durability review",
            "Migration failed: {error}",
            "Preflight completed successfully",
            "Batch transfer completed; review per-mailbox verification results",
            "Migration completed and verified",
            "Migration completed with accepted verification exceptions",
            "Dovecot synchronization completed with changes pending; repeat the final pass until exit code 0",
            "Migration completed; verification found differences requiring review",
            "Migration completed; verification requires operator review",
            "All statuses",
            "No durable runs recorded yet.",
            "Worksheet selection cancelled; no rows were imported.",
            "Create or restore a project to see durable runs.",
            "Showing the newest 250 runs. Export the audit report for complete history.",
            "Run",
            "Mailbox",
            "Stage",
            "Started",
            "Finished",
            "Detail",
            "Accept residual difference",
            "This records an auditable exception; it does not change the underlying evidence or claim exact equality.",
            "Operator",
            "Why is this difference acceptable? Include the change-ticket or customer approval reference.",
            "Accept and mark verified with exceptions",
            "Verification exception recorded durably",
            "Could not accept verification exception: {error}",
            "{} is required.",
            "{} contains an invalid control character.",
            "Saved OAuth credential configured; session token not required.",
            "Saved credential configured; session password not required.",
            "Source credential stored in OS keyring",
            "Source credential loaded",
            "Source credential deleted from OS keyring",
            "Destination credential stored in OS keyring",
            "Destination credential loaded",
            "Destination credential deleted from OS keyring",
            "Credential settings are locked while a migration is running or OAuth refresh is in progress.",
            "This is password storage, not OAuth/Modern Auth. Do not use it as a substitute for provider-specific OAuth setup or unattended secret brokering.",
            "Source OAuth refresh configuration deleted",
            "Destination OAuth refresh configuration deleted",
            "Delete saved credential?",
            "{}: {} · keyring ID: {}",
            "This permanently removes the saved credential from the OS keyring.",
            "This removes only the locally stored OAuth refresh configuration; it does not revoke the provider token.",
            "The profile reference and any credential already loaded in this session are not changed.",
            "This permanently removes the selected item from the OS keyring. The profile reference and any credential already loaded in this session are not changed.",
            "Delete saved credential",
            "saved password / access token",
            "automatic OAuth refresh configuration",
            "Refreshing OAuth token…",
            "Refreshing OAuth token in the background…",
            "OAuth refresh worker stopped before returning a result.",
            "OAuth refresh finished after account settings changed; its access token was discarded. Review the account and refresh again.",
            "{} OAuth access token refreshed (expires in {}s)",
            "{} OAuth access token refreshed",
            "No automatic refresh is configured for the {}",
            "{} OAuth refresh configuration stored in OS keyring",
            "No active project selected",
            "Verified with exceptions",
            "Run a migration to create a durable mailbox evidence record.",
            "The selected mailbox is not present in the cached project snapshot. Refresh the workspace before viewing or exporting its evidence.",
            "Discovery",
            "Seed",
            "Catch-up",
            "Final delta",
            "Complete",
            "A migration is running — monitor Activity or use Stop migration if you need to halt it.",
            "Review Attention items before starting another migration.",
            "Run the dry preflight and review every blocker before going live.",
            "The batch is running — monitor Activity and review any Attention rows before continuing.",
            "Review the imported mailbox rows, then run a dry preflight before any live migration.",
            "✓ Verified with exceptions",
            "✓ Verified",
            "✓ Completed",
            "● Running",
            "○ Queued",
            "◌ Preflight",
            "↻ Retrying",
            "○ Ready",
            "↻ Delta required",
            "≠ Verification difference",
            "! Attention",
            "× Failed",
            "× Cancelled",
            "? Unknown",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German translation: {key}"
            );
        }
    }

    #[test]
    fn destination_mutation_policy_text_is_translated() {
        use crate::migration_plan::DestinationMutationPolicy as Policy;
        for policy in [
            Policy::Additive,
            Policy::MergePreservingDestination,
            Policy::MirrorMayRemoveDestinationState,
            Policy::ExplicitDeleteMissingSourceMessages,
        ] {
            for text in [policy.label(), policy.warning()] {
                assert_ne!(
                    UiLanguage::German.text(text),
                    text,
                    "missing German: {text}"
                );
            }
        }
    }

    #[test]
    fn lookup_translates_fixed_runtime_text_only() {
        assert_eq!(UiLanguage::German.lookup("Idle"), Some("Bereit"));
        assert_eq!(UiLanguage::English.lookup("Idle"), None);
        // Formatted messages carry values and stay untranslated.
        assert_eq!(UiLanguage::German.lookup("Durability error: save"), None);
        assert_eq!(UiLanguage::German.text("Idle"), "Bereit");
    }

    #[test]
    fn batch_health_counts_are_translated() {
        for key in [
            "{} mailbox jobs in scope",
            "{} imported",
            "{} queued",
            "{} preflight",
            "{} ready",
            "{} running",
            "{} verified",
            "{} attention",
            "{} failed",
            "{} delta required",
            "{} unresolved",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German batch-health translation: {key}"
            );
        }
    }

    #[test]
    fn verification_scope_explanations_are_translated() {
        for key in [
            "Evidence includes bounded RFC822 body fingerprints from both accounts; provider-specific qualification remains required.",
            "Evidence labels describe metadata and aggregate reconciliation; message bodies were not compared.",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German verification explanation: {key}"
            );
        }
    }

    #[test]
    fn verification_detail_labels_are_translated() {
        for key in [
            "aggregate_engine",
            "metadata_reconciliation",
            "body_hash",
            "native_dovecot",
            "Exact body match — bounded RFC822 SHA-256 fingerprints compared",
            "Exact metadata match — message bodies not compared",
            "Probable metadata match — message bodies not compared",
            "Ambiguous metadata result — message bodies not compared",
            "Missing messages detected",
            "Changed messages detected",
            "Unexpected messages detected",
            "Verification evidence incomplete",
            "Verification failed",
            "Accepted by {operator} at {time}: {reason}",
        ] {
            assert_ne!(
                UiLanguage::German.text(key),
                key,
                "missing German verification detail translation: {key}"
            );
        }
    }

    #[test]
    fn dynamic_engine_and_attention_copy_is_translated() {
        for engine in [
            crate::core::Engine::Auto,
            crate::core::Engine::Dovecot,
            crate::core::Engine::ImapSync,
        ] {
            assert_ne!(
                UiLanguage::German.text(engine.label()),
                engine.label(),
                "missing German engine label: {}",
                engine.label()
            );
            assert_ne!(
                UiLanguage::German.text(engine.description()),
                engine.description(),
                "missing German engine description: {}",
                engine.description()
            );
        }
        for strategy in [
            crate::migration_plan::DovecotMigrationStrategy::InitialMirror,
            crate::migration_plan::DovecotMigrationStrategy::IncrementalMirror,
            crate::migration_plan::DovecotMigrationStrategy::FinalPreservationPass,
            crate::migration_plan::DovecotMigrationStrategy::DestinationAlreadyActive,
        ] {
            assert_ne!(
                UiLanguage::German.text(strategy.label()),
                strategy.label(),
                "missing German Dovecot strategy label: {}",
                strategy.label()
            );
            assert_ne!(
                UiLanguage::German.text(strategy.description()),
                strategy.description(),
                "missing German Dovecot strategy description"
            );
        }
        for reason in [
            crate::core::AttentionReason::Interrupted,
            crate::core::AttentionReason::VerificationIncomplete,
            crate::core::AttentionReason::VerificationDifference,
            crate::core::AttentionReason::ProcessIdentityUnverified,
            crate::core::AttentionReason::AuthenticationFailed,
            crate::core::AttentionReason::TransportFailed,
            crate::core::AttentionReason::PolicyBlocked,
            crate::core::AttentionReason::ConfigurationInvalid,
            crate::core::AttentionReason::CapacityLimited,
            crate::core::AttentionReason::MessageRejected,
            crate::core::AttentionReason::Unknown,
        ] {
            assert_ne!(
                UiLanguage::German.text(reason.label()),
                reason.label(),
                "missing German attention label: {}",
                reason.label()
            );
            assert_ne!(
                UiLanguage::German.text(reason.recommended_action()),
                reason.recommended_action(),
                "missing German attention action"
            );
        }
        for interruption in [
            crate::core::recovery_dashboard::InterruptionReason::ProcessTerminated,
            crate::core::recovery_dashboard::InterruptionReason::NetworkTimeout,
            crate::core::recovery_dashboard::InterruptionReason::ProviderThrottled,
        ] {
            for step in
                crate::core::recovery_dashboard::RecoveryPlanner::generate_guidance(interruption)
            {
                assert_ne!(
                    UiLanguage::German.text(step),
                    step,
                    "missing German recovery guidance: {step}"
                );
            }
        }
    }
}
