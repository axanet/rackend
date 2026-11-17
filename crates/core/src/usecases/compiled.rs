//! The schema cache with compiled rules (SPEC §5.4 step 7, §9.5).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use uuid::Uuid;

use crate::domain::{Collection, RuleOp, Schema};
use crate::rules::{RuleState, compile_collection_rule};

/// A schema snapshot plus the compiled rules of every collection.
#[derive(Debug, Clone, Default)]
pub struct CompiledSchema {
    /// The schema.
    pub schema: Schema,
    rules: HashMap<(Uuid, RuleOp), RuleState>,
}

impl CompiledSchema {
    /// Compiles all rules. A rule that fails to compile is treated as
    /// locked (superusers only) and logged; it never crashes the server.
    pub fn build(schema: Schema) -> Self {
        let mut rules = HashMap::new();
        for c in schema.all() {
            for op in RuleOp::ALL {
                let state = compile_collection_rule(&schema, c, op).unwrap_or_else(|e| {
                    tracing::error!(
                        collection = %c.name,
                        operation = op.as_str(),
                        error = %e.message,
                        "stored rule does not compile; the operation is restricted to superusers"
                    );
                    RuleState::Locked
                });
                rules.insert((c.id, op), state);
            }
        }
        Self { schema, rules }
    }

    /// The compiled rule of a collection operation.
    pub fn rule(&self, c: &Collection, op: RuleOp) -> &RuleState {
        self.rules.get(&(c.id, op)).unwrap_or(&RuleState::Locked)
    }
}

/// Holder of the current [`CompiledSchema`], swapped after schema changes.
#[derive(Debug, Default)]
pub struct SchemaCache {
    current: RwLock<Arc<CompiledSchema>>,
}

impl SchemaCache {
    /// The current snapshot.
    pub fn get(&self) -> Arc<CompiledSchema> {
        self.current
            .read()
            .map_or_else(|p| Arc::clone(&p.into_inner()), |g| Arc::clone(&g))
    }

    /// Replaces the snapshot.
    pub fn set(&self, schema: Schema) {
        let compiled = Arc::new(CompiledSchema::build(schema));
        match self.current.write() {
            Ok(mut g) => *g = compiled,
            Err(p) => *p.into_inner() = compiled,
        }
    }
}
