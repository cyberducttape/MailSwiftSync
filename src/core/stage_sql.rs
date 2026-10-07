//! Statement execution for the private verification stage.
//!
//! Reconciliation runs its SQL through these helpers so test builds can
//! record the `EXPLAIN QUERY PLAN` of every statement as it actually runs,
//! against the intermediate tables that exist at that moment. Release builds
//! execute the SQL unchanged and record nothing.

use rusqlite::Connection;

/// Execute a semicolon-separated batch of constant stage SQL.
pub(crate) fn execute_batch(connection: &Connection, sql: &str) -> rusqlite::Result<()> {
    #[cfg(test)]
    if plans::recording() {
        // Plan each statement only once the statements before it have run,
        // so it sees the tables they create. Stage SQL is constant and holds
        // no semicolons inside literals.
        for statement in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            plans::record(connection, statement);
            connection.execute_batch(statement)?;
        }
        return Ok(());
    }
    connection.execute_batch(sql)
}

/// `Connection::execute` with plan recording.
pub(crate) fn execute<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    parameters: P,
) -> rusqlite::Result<usize> {
    note(connection, sql);
    connection.execute(sql, parameters)
}

/// `Connection::prepare` with plan recording.
pub(crate) fn prepare<'c>(
    connection: &'c Connection,
    sql: &str,
) -> rusqlite::Result<rusqlite::Statement<'c>> {
    note(connection, sql);
    connection.prepare(sql)
}

/// `Connection::prepare_cached` with plan recording.
pub(crate) fn prepare_cached<'c>(
    connection: &'c Connection,
    sql: &str,
) -> rusqlite::Result<rusqlite::CachedStatement<'c>> {
    note(connection, sql);
    connection.prepare_cached(sql)
}

/// Record the plan of a statement about to be prepared or executed.
#[cfg_attr(not(test), allow(unused_variables))]
pub(crate) fn note(connection: &Connection, sql: &str) {
    #[cfg(test)]
    plans::record(connection, sql);
}

#[cfg(test)]
pub(crate) mod plans {
    use rusqlite::Connection;
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static PLANS: RefCell<Option<Vec<(String, String)>>> = const { RefCell::new(None) };
    }

    /// Start recording plans on this thread, discarding earlier ones.
    pub(crate) fn start() {
        PLANS.with(|plans| *plans.borrow_mut() = Some(Vec::new()));
    }

    /// Stop recording and return `(statement, plan)` in first-run order.
    pub(crate) fn finish() -> Vec<(String, String)> {
        PLANS.with(|plans| plans.borrow_mut().take().unwrap_or_default())
    }

    pub(super) fn recording() -> bool {
        PLANS.with(|plans| plans.borrow().is_some())
    }

    pub(super) fn record(connection: &Connection, sql: &str) {
        let sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let seen = PLANS.with(|plans| {
            plans
                .borrow()
                .as_ref()
                .is_none_or(|plans| plans.iter().any(|(known, _)| *known == sql))
        });
        if seen {
            return;
        }
        let plan =
            explain(connection, &sql).unwrap_or_else(|error| format!("unavailable: {error}"));
        PLANS.with(|plans| {
            if let Some(plans) = plans.borrow_mut().as_mut() {
                plans.push((sql, plan));
            }
        });
    }

    /// `EXPLAIN QUERY PLAN` as an indented tree. Parameters stay unbound,
    /// which does not change the chosen plan.
    fn explain(connection: &Connection, sql: &str) -> rusqlite::Result<String> {
        let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let mut rows = statement.raw_query();
        let mut depth = HashMap::<i64, usize>::new();
        let mut lines = Vec::new();
        while let Some(row) = rows.next()? {
            let (id, parent, detail): (i64, i64, String) = (row.get(0)?, row.get(1)?, row.get(3)?);
            let level = depth.get(&parent).map_or(0, |level| level + 1);
            depth.insert(id, level);
            lines.push(format!("{}{detail}", "  ".repeat(level)));
        }
        Ok(lines.join("\n"))
    }
}
