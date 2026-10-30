//! Roles, permissions, and the principal every request is evaluated as.
//!
//! Deny by default. Grants are additive within their scope: the
//! administrator role is service-wide, every other role is held per tenant.
//! An API key acts as its owner, narrowed to one tenant and to the key's
//! scopes, and re-evaluated against the owner's current grants on every
//! request so revoking a grant also narrows every key the owner holds.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Role {
    Administrator,
    Manager,
    Engineer,
    Technician,
    Auditor,
    Customer,
}

impl Role {
    pub(super) const ALL: [Role; 6] = [
        Role::Administrator,
        Role::Manager,
        Role::Engineer,
        Role::Technician,
        Role::Auditor,
        Role::Customer,
    ];

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Role::Administrator => "administrator",
            Role::Manager => "manager",
            Role::Engineer => "engineer",
            Role::Technician => "technician",
            Role::Auditor => "auditor",
            Role::Customer => "customer",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == value)
    }

    /// Whether this role is granted service-wide rather than per tenant.
    pub(super) fn is_global(self) -> bool {
        self == Role::Administrator
    }

    fn permissions(self) -> &'static [Permission] {
        use Permission::*;
        match self {
            Role::Administrator => &Permission::ALL,
            Role::Manager => &[
                TenantRead,
                ProjectRead,
                ProjectWrite,
                ProjectApprove,
                AssignmentRead,
                AssignmentCreate,
                WorkerManage,
                EvidenceRead,
                AuditRead,
            ],
            Role::Engineer => &[
                TenantRead,
                ProjectRead,
                AssignmentRead,
                AssignmentCreate,
                EvidenceRead,
            ],
            Role::Technician => &[TenantRead, ProjectRead, AssignmentRead, EvidenceRead],
            Role::Auditor => &[
                TenantRead,
                ProjectRead,
                AssignmentRead,
                EvidenceRead,
                AuditRead,
            ],
            Role::Customer => &[TenantRead, ProjectRead, EvidenceRead],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Permission {
    /// Create tenants and manage users, grants, and other users' keys.
    ServiceAdmin,
    TenantRead,
    ProjectRead,
    ProjectWrite,
    ProjectApprove,
    AssignmentRead,
    AssignmentCreate,
    WorkerManage,
    EvidenceRead,
    AuditRead,
}

impl Permission {
    pub(super) const ALL: [Permission; 10] = [
        Permission::ServiceAdmin,
        Permission::TenantRead,
        Permission::ProjectRead,
        Permission::ProjectWrite,
        Permission::ProjectApprove,
        Permission::AssignmentRead,
        Permission::AssignmentCreate,
        Permission::WorkerManage,
        Permission::EvidenceRead,
        Permission::AuditRead,
    ];

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Permission::ServiceAdmin => "service:admin",
            Permission::TenantRead => "tenant:read",
            Permission::ProjectRead => "project:read",
            Permission::ProjectWrite => "project:write",
            Permission::ProjectApprove => "project:approve",
            Permission::AssignmentRead => "assignment:read",
            Permission::AssignmentCreate => "assignment:create",
            Permission::WorkerManage => "worker:manage",
            Permission::EvidenceRead => "evidence:read",
            Permission::AuditRead => "audit:read",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|permission| permission.as_str() == value)
    }

    /// Scopes an API key may carry. Service administration (users, grants,
    /// tenants) always needs an interactive session.
    pub(super) fn grantable_to_api_key(self) -> bool {
        self != Permission::ServiceAdmin
    }
}

/// One role held by a user, service-wide (`tenant` is `None`) or in a tenant.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Grant {
    pub(super) role: Role,
    pub(super) tenant: Option<String>,
}

/// The authenticated caller of one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Principal {
    pub(super) user_id: String,
    pub(super) grants: Vec<Grant>,
    pub(super) via: Credential,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Credential {
    Session,
    ApiKey {
        key_id: String,
        tenant: String,
        scopes: BTreeSet<Permission>,
    },
}

impl Principal {
    /// The audit actor label: the user, and the key when one was used.
    pub(super) fn actor(&self) -> String {
        match &self.via {
            Credential::Session => format!("user:{}", self.user_id),
            Credential::ApiKey { key_id, .. } => {
                format!("user:{} key:{key_id}", self.user_id)
            }
        }
    }

    fn is_administrator(&self) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.role == Role::Administrator)
    }

    /// Permissions the user's grants give in `tenant` (`None` asks about
    /// service-wide permissions), before any API-key narrowing.
    fn granted(&self, tenant: Option<&str>) -> BTreeSet<Permission> {
        self.grants
            .iter()
            .filter(|grant| {
                grant.tenant.is_none() || (tenant.is_some() && grant.tenant.as_deref() == tenant)
            })
            .flat_map(|grant| grant.role.permissions().iter().copied())
            .collect()
    }

    /// Whether the caller may perform `permission` in `tenant`.
    pub(super) fn allows(&self, permission: Permission, tenant: Option<&str>) -> bool {
        if !self.granted(tenant).contains(&permission) {
            return false;
        }
        match &self.via {
            Credential::Session => true,
            Credential::ApiKey {
                tenant: key_tenant,
                scopes,
                ..
            } => tenant == Some(key_tenant.as_str()) && scopes.contains(&permission),
        }
    }

    /// Whether the caller can see `tenant` at all. Resources in a tenant the
    /// caller cannot see are reported as not found, so tenant and project
    /// identifiers cannot be enumerated.
    pub(super) fn sees_tenant(&self, tenant: &str) -> bool {
        self.allows(Permission::TenantRead, Some(tenant))
    }

    /// Tenants the caller may list; `None` means every tenant.
    pub(super) fn visible_tenants(&self) -> Option<BTreeSet<String>> {
        match &self.via {
            Credential::ApiKey { tenant, .. } => Some(
                self.sees_tenant(tenant)
                    .then(|| tenant.clone())
                    .into_iter()
                    .collect(),
            ),
            Credential::Session if self.is_administrator() => None,
            Credential::Session => Some(
                self.grants
                    .iter()
                    .filter_map(|grant| grant.tenant.clone())
                    .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(grants: &[(Role, Option<&str>)], via: Credential) -> Principal {
        Principal {
            user_id: "u1".into(),
            grants: grants
                .iter()
                .map(|(role, tenant)| Grant {
                    role: *role,
                    tenant: tenant.map(str::to_owned),
                })
                .collect(),
            via,
        }
    }

    #[test]
    fn tenant_roles_never_reach_another_tenant() {
        let engineer = principal(&[(Role::Engineer, Some("acme"))], Credential::Session);
        assert!(engineer.allows(Permission::AssignmentCreate, Some("acme")));
        assert!(!engineer.allows(Permission::AssignmentCreate, Some("globex")));
        assert!(!engineer.sees_tenant("globex"));
        assert!(!engineer.allows(Permission::ProjectApprove, Some("acme")));
        assert!(!engineer.allows(Permission::ServiceAdmin, None));
        assert!(!engineer.allows(Permission::TenantRead, None));
    }

    #[test]
    fn administrator_is_service_wide() {
        let admin = principal(&[(Role::Administrator, None)], Credential::Session);
        assert!(admin.allows(Permission::ServiceAdmin, None));
        assert!(admin.allows(Permission::ProjectApprove, Some("any")));
        assert_eq!(admin.visible_tenants(), None);
    }

    #[test]
    fn api_keys_are_narrowed_to_their_tenant_scopes_and_the_owner_grants() {
        let key = |scopes: &[Permission]| Credential::ApiKey {
            key_id: "k1".into(),
            tenant: "acme".into(),
            scopes: scopes.iter().copied().collect(),
        };
        let manager = principal(
            &[
                (Role::Manager, Some("acme")),
                (Role::Manager, Some("globex")),
            ],
            key(&[Permission::TenantRead, Permission::ProjectRead]),
        );
        assert!(manager.allows(Permission::ProjectRead, Some("acme")));
        assert!(!manager.allows(Permission::ProjectWrite, Some("acme")));
        assert!(!manager.allows(Permission::ProjectRead, Some("globex")));
        assert_eq!(
            manager.visible_tenants(),
            Some(BTreeSet::from(["acme".to_owned()]))
        );

        // A scope the owner no longer holds grants nothing.
        let customer = principal(
            &[(Role::Customer, Some("acme"))],
            key(&[Permission::ProjectWrite, Permission::TenantRead]),
        );
        assert!(!customer.allows(Permission::ProjectWrite, Some("acme")));

        // Even an administrator's key cannot administer the service.
        let admin_key = principal(
            &[(Role::Administrator, None)],
            key(&[Permission::ServiceAdmin]),
        );
        assert!(!admin_key.allows(Permission::ServiceAdmin, None));
    }

    #[test]
    fn names_round_trip() {
        for role in Role::ALL {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        for permission in Permission::ALL {
            assert_eq!(Permission::parse(permission.as_str()), Some(permission));
        }
        assert!(!Permission::ServiceAdmin.grantable_to_api_key());
    }
}
