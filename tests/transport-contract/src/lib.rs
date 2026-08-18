// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A schema-driven generator of deliberately invalid documents.
//!
//! # Why this exists
//!
//! The existing conformance suites ask two questions: does a Rust value
//! serialize to something the schema accepts, and does a schema-valid
//! document deserialize into the Rust type. Both are POSITIVE. Neither
//! can catch the case where the schema **rejects** a document and Rust
//! **accepts** it — which is the boundary being more permissive than the
//! contract, and is the only direction that lets a peer send something
//! the implementation will act on and a conforming validator would have
//! refused.
//!
//! Three defects of exactly that shape reached `main`: a framed `null`
//! decoding as a valid frame, nine duplicate capabilities collapsing to
//! one inside a `BTreeSet` before the cardinality check ran, and feature
//! names outside their length bounds. Every one of them passed both
//! positive suites.
//!
//! # Why it is generated rather than written
//!
//! A hand-written negative suite tests what its author remembered, which
//! is the same faculty that wrote the bug. [`mutations`] instead walks
//! the schema and derives one candidate per DECLARED constraint: every
//! `required`, every `maxLength`, every `uniqueItems`, every closed
//! object. Adding a constraint to a schema therefore adds a test with no
//! one deciding to, and the generator has no opinion about which
//! constraints matter.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// One deliberately invalid document, and why it should be invalid.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Human-readable description of the constraint being violated.
    pub label: String,
    /// JSON Pointer to the mutated location, for the failure message.
    pub pointer: String,
    /// The document.
    pub document: Value,
}

/// Every schema in the tree, keyed by `$id`, for `$ref` resolution.
pub type SchemaIndex = BTreeMap<String, Value>;

/// Resolve a schema node through any `$ref` it carries.
///
/// Handles both forms this repository uses: a `urn:interweave:...`
/// pointing at another document, and a local `#/$defs/...` pointing
/// inside `root`.
///
/// Refuses to follow a reference it cannot find rather than treating an
/// unresolved one as an unconstrained node — that would make every
/// constraint behind it invisible, and the generator would report full
/// coverage while testing nothing.
#[must_use]
pub fn resolve<'a>(node: &'a Value, index: &'a SchemaIndex, root: &'a Value) -> Option<&'a Value> {
    match node.get("$ref").and_then(Value::as_str) {
        None => Some(node),
        Some(target) => match target.strip_prefix('#') {
            Some(fragment) => root.pointer(fragment),
            None => index.get(target),
        },
    }
}

fn set_at(doc: &mut Value, pointer: &str, value: Value) {
    if pointer.is_empty() {
        *doc = value;
        return;
    }
    if let Some(slot) = doc.pointer_mut(pointer) {
        *slot = value;
    }
}

fn insert_at(doc: &mut Value, pointer: &str, key: &str, value: Value) {
    let target = if pointer.is_empty() {
        Some(doc)
    } else {
        doc.pointer_mut(pointer)
    };
    if let Some(Value::Object(map)) = target {
        map.insert(key.to_owned(), value);
    }
}

fn remove_at(doc: &mut Value, pointer: &str, key: &str) {
    let target = if pointer.is_empty() {
        Some(doc)
    } else {
        doc.pointer_mut(pointer)
    };
    if let Some(Value::Object(map)) = target {
        map.remove(key);
    }
}

fn push(
    out: &mut Vec<Candidate>,
    seed: &Value,
    pointer: &str,
    label: String,
    f: impl Fn(&mut Value),
) {
    let mut doc = seed.clone();
    f(&mut doc);
    out.push(Candidate {
        label,
        pointer: if pointer.is_empty() {
            "<root>".to_owned()
        } else {
            pointer.to_owned()
        },
        document: doc,
    });
}

/// A value of a type the node does not declare.
fn wrong_type_for(declared: Option<&str>) -> Value {
    match declared {
        Some("string") => json!(0),
        Some("integer" | "number") => json!("not a number"),
        Some("array") => json!({}),
        Some("object") => json!([]),
        Some("boolean") => json!("true"),
        _ => json!(null),
    }
}

/// Generate one invalid document per constraint the schema declares.
///
/// `seed` must be a VALID instance; every candidate is that instance with
/// exactly one thing wrong, so a failure names a single cause rather than
/// leaving the reader to work out which of several violations mattered.
#[must_use]
pub fn mutations(schema: &Value, seed: &Value, index: &SchemaIndex) -> Vec<Candidate> {
    let mut out = Vec::new();

    // Root type confusion, emitted unconditionally. Every message class
    // here is discriminated by a PROPERTY, so a scalar or array has
    // nowhere to carry one; passing it on as "decoded" hands the next
    // layer something it cannot classify and cannot report a framing
    // error about.
    //
    // Emitted without checking whether the schema declares `type:
    // object`, because THE GENERATOR DOES NOT DECIDE VALIDITY. The
    // differential loop runs a real validator over every candidate and
    // discards the ones that turn out to be legal. That is what lets this
    // work against `oneOf` roots, where no single branch is the schema
    // and a sound generator would need to reason about all of them.
    for (name, v) in [
        ("null", json!(null)),
        ("array", json!([])),
        ("number", json!(7)),
        ("string", json!("text")),
        ("boolean", json!(true)),
    ] {
        out.push(Candidate {
            label: format!("root is a JSON {name}, not an object"),
            pointer: "<root>".to_owned(),
            document: v,
        });
    }

    walk(schema, seed, "", index, &mut out, seed, schema);
    out
}

#[allow(clippy::too_many_lines)]
fn walk(
    schema: &Value,
    node: &Value,
    pointer: &str,
    index: &SchemaIndex,
    out: &mut Vec<Candidate>,
    seed: &Value,
    root: &Value,
) {
    let Some(schema) = resolve(schema, index, root) else {
        // An unresolved `$ref`. Reported as a candidate so it fails
        // loudly instead of silently skipping every constraint behind it.
        out.push(Candidate {
            label: format!(
                "UNRESOLVABLE $ref at {pointer}: the generator cannot see the constraints behind it"
            ),
            pointer: pointer.to_owned(),
            document: json!({"__unresolvable_ref": true}),
        });
        return;
    };

    let declared = schema.get("type").and_then(Value::as_str);

    // A value of the wrong type, everywhere a type is declared.
    if declared.is_some() && !pointer.is_empty() {
        let wrong = wrong_type_for(declared);
        push(
            out,
            seed,
            "",
            format!(
                "{pointer} is a {} instead of {}",
                kind_of(&wrong),
                declared.unwrap_or("?")
            ),
            |d| set_at(d, pointer, wrong.clone()),
        );
    }

    // enum / const membership.
    if let Some(members) = schema.get("enum").and_then(Value::as_array) {
        let bogus = json!("__not_a_member__");
        if !members.contains(&bogus) {
            push(
                out,
                seed,
                "",
                format!("{pointer} is outside its enum"),
                |d| {
                    set_at(d, pointer, bogus.clone());
                },
            );
        }
    }
    if let Some(fixed) = schema.get("const") {
        let other = if fixed == &json!(2) {
            json!(3)
        } else {
            json!("__not_the_const__")
        };
        push(
            out,
            seed,
            "",
            format!("{pointer} is not its const value"),
            |d| {
                set_at(d, pointer, other.clone());
            },
        );
    }

    // Composition keywords: descend into EVERY branch. A mutation that
    // one branch rejects may be rescued by another, which is exactly why
    // the validator and not the generator decides — a branch-aware
    // generator would have to solve the same problem the validator
    // already solves correctly.
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(branches) = schema.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                walk(branch, node, pointer, index, out, seed, root);
            }
        }
    }

    match declared {
        Some("object") => walk_object(schema, node, pointer, index, out, seed, root),
        Some("array") => walk_array(schema, node, pointer, index, out, seed, root),
        Some("string") => walk_string(schema, pointer, out, seed),
        Some("integer" | "number") => walk_number(schema, pointer, out, seed),
        _ => {}
    }
}

/// A pointer as it should read in a message: `""` is the document root.
fn show(pointer: &str) -> &str {
    if pointer.is_empty() {
        "<root>"
    } else {
        pointer
    }
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn walk_object(
    schema: &Value,
    node: &Value,
    pointer: &str,
    index: &SchemaIndex,
    out: &mut Vec<Candidate>,
    seed: &Value,
    root: &Value,
) {
    // A closed object that accepts an extra property is not merely
    // permissive: it SILENTLY IGNORES a field the sender may believe is
    // meaningful, and for send-params it is the one thing standing
    // between a client and claiming another endpoint as its source.
    if schema.get("additionalProperties") == Some(&json!(false)) {
        push(
            out,
            seed,
            pointer,
            format!(
                "{} is closed but carries an unknown property",
                show(pointer)
            ),
            |d| insert_at(d, pointer, "__unexpected__", json!(true)),
        );
    }

    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for name in required.iter().filter_map(Value::as_str) {
            push(
                out,
                seed,
                pointer,
                format!("{} is missing required `{name}`", show(pointer)),
                |d| remove_at(d, pointer, name),
            );
        }
    }

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    for (name, sub) in properties {
        let child_pointer = format!("{pointer}/{name}");

        // An OPTIONAL property set to explicit null. A missing property
        // is absence; `null` is a value, and the schema does not include
        // it in any of these types. A parser that treats the two the same
        // is more permissive than every schema-driven implementation it
        // has to interoperate with.
        if !required.contains(&name.as_str()) {
            push(
                out,
                seed,
                "",
                format!("{child_pointer} is explicit null rather than absent"),
                |d| {
                    insert_at(d, pointer, name, json!(null));
                },
            );
        }

        if let Some(child) = node.get(name) {
            walk(sub, child, &child_pointer, index, out, seed, root);
        }
    }
}

fn walk_array(
    schema: &Value,
    node: &Value,
    pointer: &str,
    index: &SchemaIndex,
    out: &mut Vec<Candidate>,
    seed: &Value,
    root: &Value,
) {
    let items = schema.get("items");
    let first = node.as_array().and_then(|a| a.first()).cloned();

    // uniqueItems, on its own and within the cap. This is the mutation
    // that finds a set-typed field: duplicates vanish during
    // deserialization, so the cardinality check downstream never sees
    // them.
    if schema.get("uniqueItems") == Some(&json!(true))
        && let Some(item) = first.clone()
    {
        let doubled = json!([item.clone(), item.clone()]);
        push(out, seed, "", format!("{pointer} repeats an item"), |d| {
            set_at(d, pointer, doubled.clone());
        });
    }

    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
        let n = usize::try_from(max).unwrap_or(usize::MAX).saturating_add(1);
        // Distinct members where the schema names them, so the ONLY
        // violation is the count.
        let over: Vec<Value> = match items
            .and_then(|i| resolve(i, index, root))
            .and_then(|i| i.get("enum"))
            .and_then(Value::as_array)
        {
            Some(members) if members.len() >= n => members.iter().take(n).cloned().collect(),
            _ => (0..n).map(|i| json!(format!("item{i}"))).collect(),
        };
        push(
            out,
            seed,
            "",
            format!("{pointer} has {n} items, over maxItems"),
            |d| {
                set_at(d, pointer, json!(over.clone()));
            },
        );

        // And the same count reached by repetition, which a set-typed
        // field collapses to one before any cap is consulted.
        if let Some(item) = first.clone() {
            let repeated: Vec<Value> = std::iter::repeat_n(item, n).collect();
            push(
                out,
                seed,
                "",
                format!("{pointer} has {n} copies of one item, over maxItems"),
                |d| set_at(d, pointer, json!(repeated.clone())),
            );
        }
    }

    if let (Some(items), Some(first)) = (items, first) {
        walk(
            items,
            &first,
            &format!("{pointer}/0"),
            index,
            out,
            seed,
            root,
        );
    }
}

fn walk_string(schema: &Value, pointer: &str, out: &mut Vec<Candidate>, seed: &Value) {
    if let Some(max) = schema.get("maxLength").and_then(Value::as_u64) {
        let over = "x".repeat(usize::try_from(max).unwrap_or(1024).saturating_add(1));
        push(
            out,
            seed,
            "",
            format!("{pointer} is one over maxLength {max}"),
            |d| {
                set_at(d, pointer, json!(over.clone()));
            },
        );
    }
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
        && min > 0
    {
        let under = "x".repeat(usize::try_from(min).unwrap_or(1).saturating_sub(1));
        push(
            out,
            seed,
            "",
            format!("{pointer} is one under minLength {min}"),
            |d| {
                set_at(d, pointer, json!(under.clone()));
            },
        );
    }
    if schema.get("pattern").is_some() {
        // A byte outside every pattern in this repository: they are all
        // printable-ASCII or base58/base64 alphabets.
        push(
            out,
            seed,
            "",
            format!("{pointer} violates its pattern"),
            |d| {
                set_at(d, pointer, json!("\u{7f}\u{1}not matching\u{0}"));
            },
        );
    }
}

fn walk_number(schema: &Value, pointer: &str, out: &mut Vec<Candidate>, seed: &Value) {
    if let Some(min) = schema.get("minimum").and_then(Value::as_i64) {
        push(
            out,
            seed,
            "",
            format!("{pointer} is below minimum {min}"),
            |d| {
                set_at(d, pointer, json!(min - 1));
            },
        );
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_i64) {
        push(
            out,
            seed,
            "",
            format!("{pointer} is above maximum {max}"),
            |d| {
                set_at(d, pointer, json!(max + 1));
            },
        );
    }
}

/// Build a `$id` -> schema index from a directory of schema documents.
#[must_use]
pub fn index_from(docs: impl IntoIterator<Item = Value>) -> SchemaIndex {
    let mut map = Map::new();
    for doc in docs {
        if let Some(id) = doc.get("$id").and_then(Value::as_str) {
            map.insert(id.to_owned(), doc);
        }
    }
    map.into_iter().collect()
}
