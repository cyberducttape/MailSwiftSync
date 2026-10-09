# Engine execution profiles

MailSwiftSync defaults to the hardened execution profile. Production
organization policy also rejects compatibility mode before live admission.
Compatibility mode remains available only as an explicit, visibly weaker
compatibility/lab choice for existing imapsync installations.

MSP and production operators can select the hardened profile for a headless
operation:

```sh
mailswiftsync --execution-profile hardened headless /path/to/state.db live
```

Hardened execution:

- resolves the engine to a canonical absolute regular file before launch;
- uses a fixed system executable PATH rather than inheriting PATH;
- removes inherited `PERL5LIB` and `PERL_LOCAL_LIB_ROOT` values;
- removes inherited `SSL_CERT_FILE` and `SSL_CERT_DIR` overrides;
- rejects attempts to reintroduce those variables through an engine runtime
  environment override; and
- relies on the plan's explicit CA-bundle arguments for TLS configuration;
  certificate-pinned plans are rejected until the external transfer backend
  can enforce the pin on every connection and reconnect.

The profile does not sandbox imapsync or doveadm. The local operator and the
verified engine remain part of the trust boundary, and the hardened profile
does not replace executable SHA-256 provenance or the final pre-launch
revalidation check. A same-UID local actor can still replace a verified file
in the small open/hash/close-to-spawn window; descriptor-based `fexecve` or
platform-specific equivalents would be required to eliminate that race
completely. Every durable run snapshot and exported production report records
the effective profile. `compatibility` can be selected explicitly with
`--execution-profile compatibility`, but it requires a non-production
organization policy.
