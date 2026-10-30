//! Control-plane API routes. Every route authenticates the caller, checks
//! the permission it needs in the tenant it touches, and audits the
//! decision. A tenant (or a resource in a tenant) the caller cannot see is
//! answered as not found, so identifiers cannot be enumerated.

use super::audit::AuditEntry;
use super::authz::{Permission, Principal, Role};
use super::store::{self, Actor, LoginError, Store, StoreError};
use super::workers::{self, Outcome, WorkerIdentity};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) struct Context {
    pub(super) store: Store,
    pub(super) oidc: Option<super::oidc::Oidc>,
}

pub(super) struct Request {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) authorization: Option<String>,
    pub(super) content_type: Option<String>,
    pub(super) body: Vec<u8>,
    pub(super) peer: Option<std::net::IpAddr>,
}

#[derive(Debug)]
pub(super) struct Response {
    pub(super) status: u16,
    pub(super) reason: &'static str,
    pub(super) body: String,
    pub(super) headers: Vec<(&'static str, String)>,
}

impl Response {
    fn json(status: u16, reason: &'static str, value: &Value) -> Self {
        Self {
            status,
            reason,
            body: format!("{value}\n"),
            headers: Vec::new(),
        }
    }

    fn ok(value: &Value) -> Self {
        Self::json(200, "OK", value)
    }

    fn created(value: &Value) -> Self {
        Self::json(201, "Created", value)
    }

    pub(super) fn error(status: u16, reason: &'static str, message: &str) -> Self {
        Self::json(status, reason, &json!({ "error": message }))
    }

    fn not_found() -> Self {
        Self::error(404, "Not Found", "not found")
    }

    fn unauthorized() -> Self {
        let mut response = Self::error(401, "Unauthorized", "a valid bearer token is required");
        response.headers.push((
            "WWW-Authenticate",
            "Bearer realm=\"mailswiftsync-control\"".into(),
        ));
        response
    }

    fn forbidden() -> Self {
        Self::error(
            403,
            "Forbidden",
            "you do not have permission for this action",
        )
    }
}

impl From<StoreError> for Response {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Response::not_found(),
            StoreError::Conflict(message) => Response::error(409, "Conflict", &message),
            StoreError::Invalid(message) => Response::error(422, "Unprocessable Content", &message),
            StoreError::Internal(message) => {
                eprintln!("Control plane storage error: {message}");
                Response::error(500, "Internal Server Error", "internal error")
            }
        }
    }
}

type Handled = Result<Response, Response>;

/// One request being evaluated.
struct Call<'a> {
    context: &'a Context,
    request: &'a Request,
    request_id: String,
}

pub(super) fn handle(context: &Context, request: &Request) -> Response {
    let request_id = super::authn::new_id("req").unwrap_or_else(|_| "req_unavailable".into());
    let call = Call {
        context,
        request,
        request_id: request_id.clone(),
    };
    let mut response = route(&call).unwrap_or_else(|response| response);
    response.headers.push(("X-Request-Id", request_id));
    response
}

fn route(call: &Call<'_>) -> Handled {
    let (path, query) = call
        .request
        .target
        .split_once('?')
        .unwrap_or((call.request.target.as_str(), ""));
    let segments = path
        .strip_prefix('/')
        .unwrap_or(path)
        .split('/')
        .collect::<Vec<_>>();
    let method = call.request.method.as_str();
    match (method, segments.as_slice()) {
        ("GET", ["healthz"]) => Ok(Response::ok(&json!({ "status": "ok" }))),
        ("POST", ["v1", "auth", "login"]) => login(call),
        ("POST", ["v1", "auth", "logout"]) => logout(call),
        ("GET", ["v1", "auth", "oidc", provider, "start"]) => oidc_start(call, provider),
        ("GET", ["v1", "auth", "oidc", provider, "callback"]) => {
            oidc_callback(call, provider, query)
        }
        ("GET", ["v1", "me"]) => me(call),
        ("GET", ["v1", "tenants"]) => list_tenants(call),
        ("POST", ["v1", "tenants"]) => create_tenant(call),
        ("GET", ["v1", "users"]) => list_users(call),
        ("POST", ["v1", "users"]) => create_user(call),
        ("POST", ["v1", "users", user, "disable"]) => disable_user(call, user),
        ("POST", ["v1", "users", user, "grants"]) => change_grant(call, user, true),
        ("POST", ["v1", "users", user, "grants", "remove"]) => change_grant(call, user, false),
        ("GET", ["v1", "api-keys"]) => list_api_keys(call),
        ("POST", ["v1", "api-keys", key, "revoke"]) => revoke_api_key(call, key),
        ("POST", ["v1", "tenants", tenant, "api-keys"]) => create_api_key(call, tenant),
        ("GET", ["v1", "tenants", tenant, "projects"]) => list_projects(call, tenant),
        ("POST", ["v1", "tenants", tenant, "projects"]) => create_project(call, tenant),
        ("GET", ["v1", "tenants", tenant, "projects", project]) => {
            get_project(call, tenant, project)
        }
        ("PUT", ["v1", "tenants", tenant, "projects", project, "plan"]) => {
            update_plan(call, tenant, project)
        }
        ("POST", ["v1", "tenants", tenant, "projects", project, "approve"]) => {
            approve_project(call, tenant, project)
        }
        ("GET", ["v1", "tenants", tenant, "workers"]) => list_workers(call, tenant),
        ("POST", ["v1", "tenants", tenant, "workers", "enrollments"]) => {
            create_enrollment(call, tenant)
        }
        ("POST", ["v1", "tenants", tenant, "workers", worker, "revoke"]) => {
            revoke_worker(call, tenant, worker)
        }
        ("GET", ["v1", "tenants", tenant, "projects", project, "assignments"]) => {
            list_assignments(call, tenant, project)
        }
        ("POST", ["v1", "tenants", tenant, "projects", project, "assignments"]) => {
            create_assignments(call, tenant, project)
        }
        ("POST", ["v1", "worker", "enroll"]) => enroll_worker(call),
        ("POST", ["v1", "worker", "lease"]) => lease(call),
        ("POST", ["v1", "worker", "assignments", assignment, "renew"]) => renew(call, assignment),
        ("POST", ["v1", "worker", "assignments", assignment, "complete"]) => {
            complete(call, assignment)
        }
        ("GET", ["v1", "tenants", tenant, "audit"]) => audit_log(call, Some(tenant), query),
        ("GET", ["v1", "audit"]) => audit_log(call, None, query),
        ("GET", ["v1", "audit", "verify"]) => verify_audit(call),
        _ => Err(Response::not_found()),
    }
}

impl Call<'_> {
    fn store(&self) -> &Store {
        &self.context.store
    }

    fn bearer(&self) -> Option<&str> {
        let value = self.request.authorization.as_deref()?;
        let (scheme, token) = value.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
    }

    fn principal(&self) -> Result<Principal, Response> {
        let Some(token) = self.bearer() else {
            return Err(Response::unauthorized());
        };
        self.store()
            .principal(token)
            .map_err(Response::from)?
            .ok_or_else(Response::unauthorized)
    }

    /// The enrolled worker presenting a worker credential. Worker
    /// credentials authenticate only these endpoints, and user credentials
    /// never authenticate them.
    fn worker(&self) -> Result<WorkerIdentity, Response> {
        let Some(token) = self.bearer() else {
            return Err(Response::unauthorized());
        };
        self.store()
            .worker_identity(token)
            .map_err(Response::from)?
            .ok_or_else(Response::unauthorized)
    }

    fn actor(&self, principal: &Principal) -> Actor<'_> {
        Actor {
            request_id: &self.request_id,
            label: principal.actor(),
        }
    }

    fn audit(
        &self,
        actor: &str,
        tenant: Option<&str>,
        action: &str,
        resource: &str,
        allowed: bool,
    ) {
        let _ = self.store().audit(&AuditEntry {
            at: store::now(),
            request_id: &self.request_id,
            actor,
            tenant,
            action,
            resource,
            allowed,
            detail: "",
        });
    }

    /// Require `permission` in `tenant`, auditing a refusal. A caller who
    /// cannot see the tenant at all gets not-found instead of forbidden.
    fn require(
        &self,
        principal: &Principal,
        permission: Permission,
        tenant: Option<&str>,
        action: &str,
        resource: &str,
    ) -> Result<(), Response> {
        if principal.allows(permission, tenant) {
            return Ok(());
        }
        self.audit(&principal.actor(), tenant, action, resource, false);
        if let Some(tenant) = tenant
            && !principal.sees_tenant(tenant)
        {
            return Err(Response::not_found());
        }
        Err(Response::forbidden())
    }

    /// Require `permission` in an existing tenant the caller may see.
    fn require_in_tenant(
        &self,
        principal: &Principal,
        permission: Permission,
        tenant: &str,
        action: &str,
        resource: &str,
    ) -> Result<(), Response> {
        self.require(principal, permission, Some(tenant), action, resource)?;
        if !self.store().tenant_exists(tenant)? {
            return Err(Response::not_found());
        }
        Ok(())
    }

    fn body<T: for<'de> Deserialize<'de>>(&self) -> Result<T, Response> {
        let json = self.request.content_type.as_deref().is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        });
        if !json {
            return Err(Response::error(
                415,
                "Unsupported Media Type",
                "send the body as application/json",
            ));
        }
        serde_json::from_slice(&self.request.body).map_err(|error| {
            Response::error(
                422,
                "Unprocessable Content",
                &format!("invalid request body: {error}"),
            )
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    email: String,
    password: crate::credentials::SecretString,
}

fn login(call: &Call<'_>) -> Handled {
    let body: LoginBody = call.body()?;
    match call
        .store()
        .login(&body.email, body.password.as_str(), &call.request_id)
    {
        Ok((token, expires_at)) => Ok(Response::created(&json!({
            "token": token.as_str(),
            "expires_at": expires_at,
        }))),
        Err(LoginError::Rejected | LoginError::Locked) => Err(Response::error(
            401,
            "Unauthorized",
            "the email address or password is not correct",
        )),
        Err(LoginError::Internal(message)) => Err(StoreError::Internal(message).into()),
    }
}

fn oidc<'a>(call: &Call<'a>) -> Result<&'a super::oidc::Oidc, Response> {
    call.context.oidc.as_ref().ok_or_else(Response::not_found)
}

fn oidc_failure(error: super::oidc::OidcError) -> Response {
    use super::oidc::OidcError;
    match error {
        OidcError::UnknownProvider => Response::not_found(),
        OidcError::Rejected(message) => Response::error(401, "Unauthorized", &message),
        OidcError::Upstream(message) => {
            eprintln!("Control plane OIDC provider error: {message}");
            Response::error(
                502,
                "Bad Gateway",
                "the identity provider could not complete the sign-in",
            )
        }
        OidcError::Internal(message) => {
            eprintln!("Control plane OIDC error: {message}");
            Response::error(500, "Internal Server Error", "internal error")
        }
    }
}

/// Send the browser to the identity provider. The URL is also in the body
/// for clients that open it themselves.
fn oidc_start(call: &Call<'_>, provider: &str) -> Handled {
    let url = oidc(call)?
        .start(call.store(), provider)
        .map_err(oidc_failure)?;
    let mut response = Response::json(302, "Found", &json!({ "authorization_url": url }));
    response.headers.push(("Location", url));
    Ok(response)
}

fn oidc_callback(call: &Call<'_>, provider: &str, query: &str) -> Handled {
    let oidc = oidc(call)?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let slot = match name.as_ref() {
            "code" => &mut code,
            "state" => &mut state,
            "error" => &mut error,
            _ => continue,
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(Response::error(
                400,
                "Bad Request",
                "a callback parameter was repeated",
            ));
        }
    }
    if let Some(error) = error {
        let error = error
            .chars()
            .filter(char::is_ascii_graphic)
            .take(64)
            .collect::<String>();
        return Err(Response::error(
            401,
            "Unauthorized",
            &format!("the identity provider declined the sign-in ({error})"),
        ));
    }
    let (Some(code), Some(state)) = (code, state) else {
        return Err(Response::error(
            400,
            "Bad Request",
            "the callback needs code and state",
        ));
    };
    let (token, expires_at) = oidc
        .callback(call.store(), provider, &code, &state, &call.request_id)
        .map_err(oidc_failure)?;
    Ok(Response::created(&json!({
        "token": token.as_str(),
        "expires_at": expires_at,
    })))
}

fn logout(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    let Some(token) = call.bearer() else {
        return Err(Response::unauthorized());
    };
    if principal.via != super::authz::Credential::Session {
        return Err(Response::error(
            400,
            "Bad Request",
            "only a session can sign out; revoke API keys instead",
        ));
    }
    call.store().logout(token, &call.actor(&principal))?;
    Ok(Response::ok(&json!({ "signed_out": true })))
}

fn me(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    let user = call.store().user(&principal.user_id)?;
    let key = match &principal.via {
        super::authz::Credential::Session => Value::Null,
        super::authz::Credential::ApiKey {
            key_id,
            tenant,
            scopes,
        } => json!({
            "id": key_id,
            "tenant_id": tenant,
            "scopes": scopes.iter().map(|scope| scope.as_str()).collect::<Vec<_>>(),
        }),
    };
    Ok(Response::ok(&json!({ "user": user, "api_key": key })))
}

fn list_tenants(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    let visible = principal.visible_tenants();
    let tenants = call.store().tenants(visible.as_ref())?;
    Ok(Response::ok(&json!({ "tenants": tenants })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameBody {
    name: String,
}

fn create_tenant(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    call.require(
        &principal,
        Permission::ServiceAdmin,
        None,
        "tenant.create",
        "tenant",
    )?;
    let body: NameBody = call.body()?;
    let tenant = call
        .store()
        .create_tenant(&body.name, &call.actor(&principal))?;
    Ok(Response::created(&json!(tenant)))
}

fn list_users(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    call.require(
        &principal,
        Permission::ServiceAdmin,
        None,
        "user.list",
        "user",
    )?;
    call.audit(&principal.actor(), None, "user.list", "user", true);
    Ok(Response::ok(&json!({ "users": call.store().users()? })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUserBody {
    email: String,
    display_name: String,
    #[serde(default)]
    password: Option<crate::credentials::SecretString>,
}

fn create_user(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    call.require(
        &principal,
        Permission::ServiceAdmin,
        None,
        "user.create",
        "user",
    )?;
    let body: CreateUserBody = call.body()?;
    let id = call.store().create_user(
        &body.email,
        &body.display_name,
        body.password.as_ref().map(|password| password.as_str()),
        &call.actor(&principal),
    )?;
    Ok(Response::created(&json!(call.store().user(&id)?)))
}

fn disable_user(call: &Call<'_>, user: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("user:{user}");
    call.require(
        &principal,
        Permission::ServiceAdmin,
        None,
        "user.disable",
        &resource,
    )?;
    if user == principal.user_id {
        return Err(Response::error(
            409,
            "Conflict",
            "you cannot disable your own account",
        ));
    }
    call.store().disable_user(user, &call.actor(&principal))?;
    Ok(Response::ok(&json!(call.store().user(user)?)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantBody {
    role: String,
    #[serde(default)]
    tenant_id: Option<String>,
}

fn change_grant(call: &Call<'_>, user: &str, add: bool) -> Handled {
    let principal = call.principal()?;
    let resource = format!("user:{user}");
    let action = if add { "grant.add" } else { "grant.remove" };
    call.require(
        &principal,
        Permission::ServiceAdmin,
        None,
        action,
        &resource,
    )?;
    let body: GrantBody = call.body()?;
    let role = Role::parse(&body.role).ok_or_else(|| {
        Response::error(
            422,
            "Unprocessable Content",
            "role must be administrator, manager, engineer, technician, auditor, or customer",
        )
    })?;
    let actor = call.actor(&principal);
    let tenant = body.tenant_id.as_deref();
    if add {
        call.store().add_grant(user, role, tenant, &actor)?;
    } else {
        call.store().remove_grant(user, role, tenant, &actor)?;
    }
    Ok(Response::ok(&json!(call.store().user(user)?)))
}

fn list_api_keys(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    let owner =
        (!principal.allows(Permission::ServiceAdmin, None)).then_some(principal.user_id.as_str());
    Ok(Response::ok(
        &json!({ "api_keys": call.store().api_keys(owner)? }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKeyBody {
    name: String,
    scopes: Vec<String>,
    expires_in_days: i64,
}

fn create_api_key(call: &Call<'_>, tenant: &str) -> Handled {
    let principal = call.principal()?;
    if principal.via != super::authz::Credential::Session {
        call.audit(
            &principal.actor(),
            Some(tenant),
            "api_key.create",
            "api_key",
            false,
        );
        return Err(Response::error(
            403,
            "Forbidden",
            "API keys are issued from a signed-in session, not by another key",
        ));
    }
    call.require_in_tenant(
        &principal,
        Permission::TenantRead,
        tenant,
        "api_key.create",
        "api_key",
    )?;
    let body: CreateKeyBody = call.body()?;
    let scopes = body
        .scopes
        .iter()
        .map(|scope| {
            Permission::parse(scope).ok_or_else(|| {
                Response::error(
                    422,
                    "Unprocessable Content",
                    &format!("unknown scope {scope:?}"),
                )
            })
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    let (key, token) = call.store().create_api_key(
        &principal,
        tenant,
        &body.name,
        &scopes,
        body.expires_in_days,
        &call.actor(&principal),
    )?;
    Ok(Response::created(&json!({
        "api_key": key,
        "token": token.as_str(),
        "note": "store this token now; it is not shown again",
    })))
}

fn revoke_api_key(call: &Call<'_>, key: &str) -> Handled {
    let principal = call.principal()?;
    let owner =
        (!principal.allows(Permission::ServiceAdmin, None)).then_some(principal.user_id.as_str());
    call.store()
        .revoke_api_key(key, owner, &call.actor(&principal))?;
    Ok(Response::ok(&json!({ "revoked": key })))
}

fn list_projects(call: &Call<'_>, tenant: &str) -> Handled {
    let principal = call.principal()?;
    call.require_in_tenant(
        &principal,
        Permission::ProjectRead,
        tenant,
        "project.list",
        "project",
    )?;
    call.audit(
        &principal.actor(),
        Some(tenant),
        "project.list",
        "project",
        true,
    );
    Ok(Response::ok(
        &json!({ "projects": call.store().projects(tenant)? }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProjectBody {
    name: String,
    #[serde(default = "empty_plan")]
    plan: Value,
}

fn empty_plan() -> Value {
    json!({})
}

fn create_project(call: &Call<'_>, tenant: &str) -> Handled {
    let principal = call.principal()?;
    call.require_in_tenant(
        &principal,
        Permission::ProjectWrite,
        tenant,
        "project.create",
        "project",
    )?;
    let body: CreateProjectBody = call.body()?;
    let project =
        call.store()
            .create_project(tenant, &body.name, &body.plan, &call.actor(&principal))?;
    Ok(Response::created(&json!(project)))
}

fn get_project(call: &Call<'_>, tenant: &str, project: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("project:{project}");
    call.require_in_tenant(
        &principal,
        Permission::ProjectRead,
        tenant,
        "project.read",
        &resource,
    )?;
    let found = call.store().project(tenant, project)?;
    call.audit(
        &principal.actor(),
        Some(tenant),
        "project.read",
        &resource,
        true,
    );
    Ok(Response::ok(&json!(found)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PlanBody {
    plan: Value,
    revision: i64,
}

fn update_plan(call: &Call<'_>, tenant: &str, project: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("project:{project}");
    call.require_in_tenant(
        &principal,
        Permission::ProjectWrite,
        tenant,
        "project.update_plan",
        &resource,
    )?;
    let body: PlanBody = call.body()?;
    let updated = call.store().update_plan(
        tenant,
        project,
        &body.plan,
        body.revision,
        &call.actor(&principal),
    )?;
    Ok(Response::ok(&json!(updated)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApproveBody {
    revision: i64,
}

fn approve_project(call: &Call<'_>, tenant: &str, project: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("project:{project}");
    call.require_in_tenant(
        &principal,
        Permission::ProjectApprove,
        tenant,
        "project.approve",
        &resource,
    )?;
    let body: ApproveBody = call.body()?;
    let approved = call.store().approve_project(
        tenant,
        project,
        body.revision,
        &principal.user_id,
        &call.actor(&principal),
    )?;
    Ok(Response::ok(&json!(approved)))
}

fn list_workers(call: &Call<'_>, tenant: &str) -> Handled {
    let principal = call.principal()?;
    call.require_in_tenant(
        &principal,
        Permission::WorkerManage,
        tenant,
        "worker.list",
        "worker",
    )?;
    Ok(Response::ok(
        &json!({ "workers": call.store().workers(tenant)? }),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollmentBody {
    name: String,
    #[serde(default = "default_enrollment_minutes")]
    expires_in_minutes: i64,
}

fn default_enrollment_minutes() -> i64 {
    15
}

fn create_enrollment(call: &Call<'_>, tenant: &str) -> Handled {
    let principal = call.principal()?;
    call.require_in_tenant(
        &principal,
        Permission::WorkerManage,
        tenant,
        "worker.enrollment_create",
        "worker",
    )?;
    let body: EnrollmentBody = call.body()?;
    let (token, expires_at) = call.store().create_enrollment(
        tenant,
        &body.name,
        body.expires_in_minutes,
        &call.actor(&principal),
    )?;
    Ok(Response::created(&json!({
        "enrollment_token": token.as_str(),
        "expires_at": expires_at,
        "note": "give this single-use token to the worker; it is not shown again",
    })))
}

fn revoke_worker(call: &Call<'_>, tenant: &str, worker: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("worker:{worker}");
    call.require_in_tenant(
        &principal,
        Permission::WorkerManage,
        tenant,
        "worker.revoke",
        &resource,
    )?;
    let held = call
        .store()
        .revoke_worker(tenant, worker, &call.actor(&principal))?;
    Ok(Response::ok(
        &json!({ "revoked": worker, "leases_awaiting_expiry": held }),
    ))
}

fn list_assignments(call: &Call<'_>, tenant: &str, project: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("project:{project}");
    call.require_in_tenant(
        &principal,
        Permission::AssignmentRead,
        tenant,
        "assignment.list",
        &resource,
    )?;
    let assignments = call.store().assignments(tenant, project)?;
    call.audit(
        &principal.actor(),
        Some(tenant),
        "assignment.list",
        &resource,
        true,
    );
    Ok(Response::ok(&json!({ "assignments": assignments })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentsBody {
    mailboxes: Vec<String>,
}

fn create_assignments(call: &Call<'_>, tenant: &str, project: &str) -> Handled {
    let principal = call.principal()?;
    let resource = format!("project:{project}");
    call.require_in_tenant(
        &principal,
        Permission::AssignmentCreate,
        tenant,
        "assignment.create",
        &resource,
    )?;
    let body: AssignmentsBody = call.body()?;
    let created = call.store().create_assignments(
        tenant,
        project,
        &body.mailboxes,
        &call.actor(&principal),
    )?;
    Ok(Response::created(&json!({ "assignments": created })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnrollBody {
    #[serde(default)]
    capabilities: Vec<String>,
}

fn enroll_worker(call: &Call<'_>) -> Handled {
    let Some(token) = call.bearer() else {
        return Err(Response::unauthorized());
    };
    let body: EnrollBody = call.body()?;
    match call
        .store()
        .enroll_worker(token, &body.capabilities, &call.request_id)
    {
        Ok((worker, credential)) => Ok(Response::created(&json!({
            "worker": worker,
            "credential": credential.as_str(),
            "note": "store this credential on the worker; it is not shown again",
        }))),
        Err(StoreError::NotFound) => Err(Response::unauthorized()),
        Err(error) => Err(error.into()),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseBody {
    #[serde(default = "default_lease_seconds")]
    lease_seconds: i64,
}

fn default_lease_seconds() -> i64 {
    workers::DEFAULT_LEASE_SECONDS
}

fn lease(call: &Call<'_>) -> Handled {
    let worker = call.worker()?;
    let body: LeaseBody = call.body()?;
    let lease = call
        .store()
        .lease(&worker, body.lease_seconds, &call.request_id)?;
    Ok(Response::ok(&json!({ "lease": lease })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RenewBody {
    fencing_token: i64,
    #[serde(default = "default_lease_seconds")]
    lease_seconds: i64,
}

fn renew(call: &Call<'_>, assignment: &str) -> Handled {
    let worker = call.worker()?;
    let body: RenewBody = call.body()?;
    let expires_at = call.store().renew_lease(
        &worker,
        assignment,
        body.fencing_token,
        body.lease_seconds,
        &call.request_id,
    )?;
    Ok(Response::ok(&json!({ "lease_expires_at": expires_at })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompleteBody {
    fencing_token: i64,
    outcome: String,
    #[serde(default = "empty_plan")]
    result: Value,
}

fn complete(call: &Call<'_>, assignment: &str) -> Handled {
    let worker = call.worker()?;
    let body: CompleteBody = call.body()?;
    let outcome = Outcome::parse(&body.outcome).ok_or_else(|| {
        Response::error(
            422,
            "Unprocessable Content",
            "outcome must be completed or failed",
        )
    })?;
    call.store().complete(
        &worker,
        assignment,
        body.fencing_token,
        outcome,
        &body.result,
        &call.request_id,
    )?;
    Ok(Response::ok(&json!({ "recorded": assignment })))
}

fn audit_log(call: &Call<'_>, tenant: Option<&str>, query: &str) -> Handled {
    let principal = call.principal()?;
    match tenant {
        Some(tenant) => call.require_in_tenant(
            &principal,
            Permission::AuditRead,
            tenant,
            "audit.read",
            "audit",
        )?,
        None => call.require(
            &principal,
            Permission::ServiceAdmin,
            None,
            "audit.read",
            "audit",
        )?,
    }
    let parameter = |name: &str| {
        url::form_urlencoded::parse(query.as_bytes())
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.parse::<i64>().ok())
    };
    let entries = call.store().audit_entries(
        tenant,
        parameter("before"),
        parameter("limit").unwrap_or(100),
    )?;
    call.audit(&principal.actor(), tenant, "audit.read", "audit", true);
    Ok(Response::ok(&json!({ "entries": entries })))
}

fn verify_audit(call: &Call<'_>) -> Handled {
    let principal = call.principal()?;
    call.require(
        &principal,
        Permission::ServiceAdmin,
        None,
        "audit.verify",
        "audit",
    )?;
    match call.store().verify_audit_chain() {
        Ok(entries) => Ok(Response::ok(&json!({ "intact": true, "entries": entries }))),
        Err(error) => Ok(Response::json(
            409,
            "Conflict",
            &json!({ "intact": false, "error": error }),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Harness {
        context: Context,
        admin_password: String,
    }

    impl Harness {
        fn new() -> Self {
            let store = Store::in_memory();
            let admin_password = crate::cli::control::authn::test_secret();
            store
                .bootstrap_administrator("admin@msp.example", "Admin", &admin_password)
                .unwrap();
            Self {
                context: Context { store, oidc: None },
                admin_password,
            }
        }

        fn call(
            &self,
            method: &str,
            target: &str,
            token: Option<&str>,
            body: Option<Value>,
        ) -> (u16, Value) {
            let request = Request {
                method: method.into(),
                target: target.into(),
                authorization: token.map(|token| format!("Bearer {token}")),
                content_type: body.as_ref().map(|_| "application/json".into()),
                body: body
                    .map(|body| body.to_string().into_bytes())
                    .unwrap_or_default(),
                peer: None,
            };
            let response = handle(&self.context, &request);
            (
                response.status,
                serde_json::from_str(&response.body).unwrap(),
            )
        }

        fn login(&self, email: &str, password: &str) -> String {
            let (status, body) = self.call(
                "POST",
                "/v1/auth/login",
                None,
                Some(json!({ "email": email, "password": password })),
            );
            assert_eq!(status, 201, "{body}");
            body["token"].as_str().unwrap().to_owned()
        }

        fn user(&self, admin: &str, email: &str, role: &str, tenant: &str) -> String {
            let password = crate::cli::control::authn::test_secret();
            let (status, body) = self.call(
                "POST",
                "/v1/users",
                Some(admin),
                Some(json!({ "email": email, "display_name": email, "password": password })),
            );
            assert_eq!(status, 201, "{body}");
            let id = body["id"].as_str().unwrap().to_owned();
            let (status, body) = self.call(
                "POST",
                &format!("/v1/users/{id}/grants"),
                Some(admin),
                Some(json!({ "role": role, "tenant_id": tenant })),
            );
            assert_eq!(status, 200, "{body}");
            self.login(email, &password)
        }

        fn tenant(&self, admin: &str, name: &str) -> String {
            let (status, body) = self.call(
                "POST",
                "/v1/tenants",
                Some(admin),
                Some(json!({ "name": name })),
            );
            assert_eq!(status, 201, "{body}");
            body["id"].as_str().unwrap().to_owned()
        }
    }

    #[test]
    fn requests_without_valid_credentials_are_rejected() {
        let harness = Harness::new();
        let wrong_password = crate::cli::control::authn::test_secret();
        assert_eq!(harness.call("GET", "/v1/me", None, None).0, 401);
        assert_eq!(
            harness.call("GET", "/v1/me", Some("mss_s_forged"), None).0,
            401
        );
        assert_eq!(harness.call("GET", "/healthz", None, None).0, 200);
        let (status, _) = harness.call(
            "POST",
            "/v1/auth/login",
            None,
            Some(json!({ "email": "admin@msp.example", "password": &wrong_password })),
        );
        assert_eq!(status, 401);
    }

    #[test]
    fn locked_and_unknown_accounts_have_identical_login_responses() {
        let harness = Harness::new();
        let wrong_password = crate::cli::control::authn::test_secret();
        for _ in 0..store::LOCKOUT_FAILURES {
            assert_eq!(
                harness
                    .call(
                        "POST",
                        "/v1/auth/login",
                        None,
                        Some(json!({
                            "email": "admin@msp.example",
                            "password": &wrong_password
                        })),
                    )
                    .0,
                401
            );
        }
        let locked = harness.call(
            "POST",
            "/v1/auth/login",
            None,
            Some(json!({
                "email": "admin@msp.example",
                "password": &wrong_password
            })),
        );
        let unknown = harness.call(
            "POST",
            "/v1/auth/login",
            None,
            Some(json!({
                "email": "nobody@msp.example",
                "password": &wrong_password
            })),
        );
        assert_eq!(locked, unknown);
    }

    #[test]
    fn cross_tenant_identifiers_are_not_found_and_audited() {
        let harness = Harness::new();
        let admin = harness.login("admin@msp.example", &harness.admin_password);
        let acme = harness.tenant(&admin, "Acme");
        let globex = harness.tenant(&admin, "Globex");
        let acme_manager = harness.user(&admin, "m@acme.example", "manager", &acme);
        let globex_manager = harness.user(&admin, "m@globex.example", "manager", &globex);

        let (status, project) = harness.call(
            "POST",
            &format!("/v1/tenants/{acme}/projects"),
            Some(&acme_manager),
            Some(json!({ "name": "Move", "plan": { "source_host": "imap.acme.example" } })),
        );
        assert_eq!(status, 201, "{project}");
        let project = project["id"].as_str().unwrap();

        // Guessing the tenant, or substituting your own tenant for the
        // project's, both look like nothing exists.
        for target in [
            format!("/v1/tenants/{acme}/projects/{project}"),
            format!("/v1/tenants/{acme}/projects"),
            format!("/v1/tenants/{globex}/projects/{project}"),
        ] {
            assert_eq!(
                harness.call("GET", &target, Some(&globex_manager), None).0,
                404,
                "{target}"
            );
        }
        let (_, tenants) = harness.call("GET", "/v1/tenants", Some(&globex_manager), None);
        assert_eq!(tenants["tenants"].as_array().unwrap().len(), 1);

        let (_, audit) = harness.call("GET", "/v1/audit?limit=20", Some(&admin), None);
        assert!(
            audit["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["decision"] == "deny" && entry["tenant_id"] == acme.as_str()),
            "{audit}"
        );
        let (status, verified) = harness.call("GET", "/v1/audit/verify", Some(&admin), None);
        assert_eq!((status, verified["intact"].clone()), (200, json!(true)));
    }

    #[test]
    fn roles_limit_actions_within_a_tenant() {
        let harness = Harness::new();
        let admin = harness.login("admin@msp.example", &harness.admin_password);
        let acme = harness.tenant(&admin, "Acme");
        let customer = harness.user(&admin, "c@acme.example", "customer", &acme);
        let manager = harness.user(&admin, "m@acme.example", "manager", &acme);
        let projects = format!("/v1/tenants/{acme}/projects");

        assert_eq!(
            harness
                .call(
                    "POST",
                    &projects,
                    Some(&customer),
                    Some(json!({ "name": "x" }))
                )
                .0,
            403
        );
        assert_eq!(harness.call("GET", &projects, Some(&customer), None).0, 200);
        assert_eq!(
            harness.call("GET", "/v1/users", Some(&manager), None).0,
            403
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    "/v1/tenants",
                    Some(&manager),
                    Some(json!({ "name": "Mine" }))
                )
                .0,
            403
        );
        assert_eq!(
            harness
                .call(
                    "GET",
                    &format!("/v1/tenants/{acme}/audit"),
                    Some(&customer),
                    None
                )
                .0,
            403
        );
        assert_eq!(
            harness
                .call(
                    "GET",
                    &format!("/v1/tenants/{acme}/audit"),
                    Some(&manager),
                    None
                )
                .0,
            200
        );
    }

    #[test]
    fn scoped_api_keys_reach_only_their_tenant_and_scopes() {
        let harness = Harness::new();
        let admin = harness.login("admin@msp.example", &harness.admin_password);
        let acme = harness.tenant(&admin, "Acme");
        let globex = harness.tenant(&admin, "Globex");
        let manager = harness.user(&admin, "m@acme.example", "manager", &acme);
        let (status, created) = harness.call(
            "POST",
            &format!("/v1/tenants/{acme}/api-keys"),
            Some(&manager),
            Some(json!({ "name": "psa", "scopes": ["tenant:read", "project:read"], "expires_in_days": 30 })),
        );
        assert_eq!(status, 201, "{created}");
        let key = created["token"].as_str().unwrap().to_owned();
        let key_id = created["api_key"]["id"].as_str().unwrap().to_owned();

        assert_eq!(
            harness
                .call(
                    "GET",
                    &format!("/v1/tenants/{acme}/projects"),
                    Some(&key),
                    None
                )
                .0,
            200
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    &format!("/v1/tenants/{acme}/projects"),
                    Some(&key),
                    Some(json!({ "name": "x" }))
                )
                .0,
            403
        );
        assert_eq!(
            harness
                .call(
                    "GET",
                    &format!("/v1/tenants/{globex}/projects"),
                    Some(&key),
                    None
                )
                .0,
            404
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    &format!("/v1/tenants/{acme}/api-keys"),
                    Some(&key),
                    Some(
                        json!({ "name": "child", "scopes": ["tenant:read"], "expires_in_days": 1 })
                    )
                )
                .0,
            403,
            "a key cannot mint keys"
        );

        assert_eq!(
            harness
                .call(
                    "POST",
                    &format!("/v1/api-keys/{key_id}/revoke"),
                    Some(&manager),
                    None
                )
                .0,
            200
        );
        assert_eq!(
            harness
                .call(
                    "GET",
                    &format!("/v1/tenants/{acme}/projects"),
                    Some(&key),
                    None
                )
                .0,
            401
        );
    }

    #[test]
    fn approval_requires_a_second_person_and_the_current_revision() {
        let harness = Harness::new();
        let admin = harness.login("admin@msp.example", &harness.admin_password);
        let acme = harness.tenant(&admin, "Acme");
        let first = harness.user(&admin, "one@acme.example", "manager", &acme);
        let second = harness.user(&admin, "two@acme.example", "manager", &acme);
        let (_, project) = harness.call(
            "POST",
            &format!("/v1/tenants/{acme}/projects"),
            Some(&first),
            Some(json!({ "name": "Move", "plan": {} })),
        );
        let approve = format!(
            "/v1/tenants/{acme}/projects/{}/approve",
            project["id"].as_str().unwrap()
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    &approve,
                    Some(&first),
                    Some(json!({ "revision": 1 }))
                )
                .0,
            409
        );
        let (status, approved) = harness.call(
            "POST",
            &approve,
            Some(&second),
            Some(json!({ "revision": 1 })),
        );
        assert_eq!(status, 200, "{approved}");
        assert_eq!(approved["approved"], true);
    }

    #[test]
    fn workers_enroll_lease_and_are_fenced_over_http() {
        let harness = Harness::new();
        let admin = harness.login("admin@msp.example", &harness.admin_password);
        let acme = harness.tenant(&admin, "Acme");
        let globex = harness.tenant(&admin, "Globex");
        let manager = harness.user(&admin, "m@acme.example", "manager", &acme);
        let engineer = harness.user(&admin, "e@acme.example", "engineer", &acme);
        let globex_manager = harness.user(&admin, "m@globex.example", "manager", &globex);

        let enroll_path = format!("/v1/tenants/{acme}/workers/enrollments");
        assert_eq!(
            harness
                .call(
                    "POST",
                    &enroll_path,
                    Some(&engineer),
                    Some(json!({ "name": "w1" }))
                )
                .0,
            403
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    &enroll_path,
                    Some(&globex_manager),
                    Some(json!({ "name": "w1" }))
                )
                .0,
            404
        );
        let (status, enrollment) = harness.call(
            "POST",
            &enroll_path,
            Some(&manager),
            Some(json!({ "name": "w1" })),
        );
        assert_eq!(status, 201, "{enrollment}");
        let enrollment = enrollment["enrollment_token"].as_str().unwrap();
        let (status, enrolled) = harness.call(
            "POST",
            "/v1/worker/enroll",
            Some(enrollment),
            Some(json!({ "capabilities": ["imapsync"] })),
        );
        assert_eq!(status, 201, "{enrolled}");
        let credential = enrolled["credential"].as_str().unwrap().to_owned();
        let worker_id = enrolled["worker"]["id"].as_str().unwrap().to_owned();
        assert_eq!(
            harness
                .call(
                    "POST",
                    "/v1/worker/enroll",
                    Some(enrollment),
                    Some(json!({}))
                )
                .0,
            401,
            "enrollment tokens are single-use"
        );

        // Credentials do not cross: a worker cannot call user endpoints and a
        // user session cannot lease work.
        assert_eq!(
            harness.call("GET", "/v1/me", Some(&credential), None).0,
            401
        );
        assert_eq!(
            harness
                .call("POST", "/v1/worker/lease", Some(&manager), Some(json!({})))
                .0,
            401
        );

        let (_, project) = harness.call(
            "POST",
            &format!("/v1/tenants/{acme}/projects"),
            Some(&manager),
            Some(json!({ "name": "Move", "plan": { "source_host": "imap.acme.example" } })),
        );
        let project = project["id"].as_str().unwrap().to_owned();
        let assignments = format!("/v1/tenants/{acme}/projects/{project}/assignments");
        let body = json!({ "mailboxes": ["a@acme.example"] });
        assert_eq!(
            harness
                .call("POST", &assignments, Some(&engineer), Some(body.clone()))
                .0,
            409
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    &format!("/v1/tenants/{acme}/projects/{project}/approve"),
                    Some(&admin),
                    Some(json!({ "revision": 1 }))
                )
                .0,
            200
        );
        assert_eq!(
            harness
                .call("POST", &assignments, Some(&engineer), Some(body))
                .0,
            201
        );

        let (status, leased) = harness.call(
            "POST",
            "/v1/worker/lease",
            Some(&credential),
            Some(json!({})),
        );
        assert_eq!(status, 200, "{leased}");
        let assignment = leased["lease"]["assignment_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let token = leased["lease"]["fencing_token"].as_i64().unwrap();
        let renew = format!("/v1/worker/assignments/{assignment}/renew");
        assert_eq!(
            harness
                .call(
                    "POST",
                    &renew,
                    Some(&credential),
                    Some(json!({ "fencing_token": token }))
                )
                .0,
            200
        );
        assert_eq!(
            harness
                .call(
                    "POST",
                    &renew,
                    Some(&credential),
                    Some(json!({ "fencing_token": token - 1 }))
                )
                .0,
            409
        );

        assert_eq!(
            harness
                .call(
                    "POST",
                    &format!("/v1/tenants/{acme}/workers/{worker_id}/revoke"),
                    Some(&manager),
                    None
                )
                .0,
            200
        );
        let complete = format!("/v1/worker/assignments/{assignment}/complete");
        assert_eq!(
            harness
                .call(
                    "POST",
                    &complete,
                    Some(&credential),
                    Some(json!({ "fencing_token": token, "outcome": "completed" }))
                )
                .0,
            401,
            "a revoked worker's credential stops working"
        );
        // The revoked worker may still be stopping its engine, so the lease
        // stays assigned until it expires and the reassignment grace passes.
        let (_, listed) = harness.call("GET", &assignments, Some(&engineer), None);
        assert_eq!(listed["assignments"][0]["state"], "leased");
    }

    #[test]
    fn oidc_sign_in_runs_over_http_and_issues_a_session() {
        use super::super::oidc::tests as idp;
        let harness = Harness::new();
        assert_eq!(
            harness.call("GET", "/v1/auth/oidc/idp/start", None, None).0,
            404,
            "OIDC is off by default"
        );
        let admin = harness.login("admin@msp.example", &harness.admin_password);
        harness.call(
            "POST",
            "/v1/users",
            Some(&admin),
            Some(json!({ "email": "eng@msp.example", "display_name": "Eng" })),
        );
        let fake = idp::fake_idp();
        let harness = Harness {
            context: Context {
                store: harness.context.store,
                oidc: Some(idp::oidc(&fake, true)),
            },
            admin_password: harness.admin_password,
        };
        let request = |target: &str| Request {
            method: "GET".into(),
            target: target.into(),
            authorization: None,
            content_type: None,
            body: Vec::new(),
            peer: None,
        };
        let started = handle(&harness.context, &request("/v1/auth/oidc/idp/start"));
        assert_eq!(started.status, 302);
        let location = started
            .headers
            .iter()
            .find(|(name, _)| *name == "Location")
            .unwrap()
            .1
            .clone();
        let location = url::Url::parse(&location).unwrap();
        let query = |name: &str| {
            location
                .query_pairs()
                .find(|(key, _)| key == name)
                .unwrap()
                .1
                .into_owned()
        };
        *fake.next_claims.lock().unwrap() =
            idp::claims(&query("nonce"), "sub-1", "eng@msp.example", true);

        let callback = format!(
            "/v1/auth/oidc/idp/callback?code=abc&state={}",
            query("state")
        );
        let (status, body) = harness.call("GET", &callback, None, None);
        assert_eq!(status, 201, "{body}");
        let session = body["token"].as_str().unwrap();
        let (status, me) = harness.call("GET", "/v1/me", Some(session), None);
        assert_eq!(
            (status, me["user"]["email"].clone()),
            (200, json!("eng@msp.example"))
        );

        assert_eq!(
            harness.call("GET", &callback, None, None).0,
            401,
            "a callback cannot be replayed"
        );
        assert_eq!(
            harness
                .call(
                    "GET",
                    "/v1/auth/oidc/idp/callback?error=access_denied",
                    None,
                    None
                )
                .0,
            401
        );
        assert_eq!(
            harness
                .call(
                    "GET",
                    "/v1/auth/oidc/idp/callback?code=a&code=b&state=s",
                    None,
                    None
                )
                .0,
            400
        );
        assert_eq!(
            harness
                .call("GET", "/v1/auth/oidc/other/start", None, None)
                .0,
            404
        );
    }

    #[test]
    fn bodies_must_be_declared_json_and_match_the_schema() {
        let harness = Harness::new();
        let mut request = Request {
            method: "POST".into(),
            target: "/v1/auth/login".into(),
            authorization: None,
            content_type: Some("text/plain".into()),
            body: json!({
                "email": "admin@msp.example",
                "password": crate::cli::control::authn::test_secret()
            })
            .to_string()
            .into_bytes(),
            peer: None,
        };
        assert_eq!(handle(&harness.context, &request).status, 415);
        request.content_type = Some("application/json; charset=utf-8".into());
        request.body = br#"{"email":"a","password":"b","extra":1}"#.to_vec();
        assert_eq!(handle(&harness.context, &request).status, 422);
    }
}
