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

/// What one walk produced: the candidates, and the seeds it had to
/// invent to reach them.
#[derive(Debug, Default)]
pub struct Generated {
    /// One deliberately invalid document per declared constraint.
    pub candidates: Vec<Candidate>,
    /// Seeds augmented with a synthesized value for an optional property
    /// the original seed omitted, keyed by the pointer that was filled.
    ///
    /// Exposed because each one is a CLAIM — that the synthesized value
    /// is itself valid — and an untrue claim would make every mutation
    /// beneath it invalid for a second, unstated reason. The caller
    /// validates these, so a bad synthesis fails loudly rather than
    /// quietly weakening the suite it was added to strengthen.
    pub synthesized_seeds: Vec<(String, Value)>,
    /// Constraints the generator declined to build a mutation for.
    ///
    /// Empty is the only acceptable value, and the caller asserts that.
    /// A generator that quietly skips what it cannot construct reports
    /// full coverage while testing nothing — the exact blind spot this
    /// module exists to remove, reintroduced one `continue` at a time.
    pub uncovered: Vec<String>,
}

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

fn push(out: &mut Generated, seed: &Value, pointer: &str, label: String, f: impl Fn(&mut Value)) {
    let mut doc = seed.clone();
    f(&mut doc);
    out.candidates.push(Candidate {
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
    mutations_and_seeds(schema, seed, index).candidates
}

/// Root type confusion, emitted unconditionally. Every message class
/// here is discriminated by a PROPERTY, so a scalar or array has nowhere
/// to carry one; passing it on as "decoded" hands the next layer
/// something it cannot classify and cannot report a framing error about.
///
/// Emitted without checking whether the schema declares `type: object`,
/// because THE GENERATOR DOES NOT DECIDE VALIDITY. The differential loop
/// runs a real validator over every candidate and discards the ones that
/// turn out to be legal. That is what lets this work against `oneOf`
/// roots, where no single branch is the schema and a sound generator
/// would need to reason about all of them.
fn generated_root() -> Generated {
    let candidates = [
        ("null", json!(null)),
        ("array", json!([])),
        ("number", json!(7)),
        ("string", json!("text")),
        ("boolean", json!(true)),
    ]
    .into_iter()
    .map(|(name, v)| Candidate {
        label: format!("root is a JSON {name}, not an object"),
        pointer: "<root>".to_owned(),
        document: v,
    })
    .collect();
    Generated {
        candidates,
        synthesized_seeds: Vec::new(),
        uncovered: Vec::new(),
    }
}

/// Like [`mutations`], and also returns the seeds the walk had to invent.
///
/// Use this when the caller can validate: every synthesized seed is an
/// assertion that the value invented for an omitted optional property is
/// itself legal, and checking it is what keeps the extra coverage
/// honest.
#[must_use]
pub fn mutations_and_seeds(schema: &Value, seed: &Value, index: &SchemaIndex) -> Generated {
    let mut out = generated_root();
    walk(schema, seed, "", index, &mut out, seed, schema);
    out
}

/// Whether `schema` plausibly describes `node` — used to decide where a
/// value may be invented, never to decide validity.
///
/// A `oneOf` root is walked branch by branch against the same node, so
/// without this a `ping` seed gets a `request`-only optional property
/// synthesized into it and the augmented seed is invalid for a reason
/// that has nothing to do with the constraint being probed.
///
/// Deliberately structural and cheap: every `required` present, every
/// `const` discriminator matching, and nothing extra in a closed object.
/// It answers "is this the branch the seed is an instance of", which is
/// all the synthesis decision needs.
fn describes(schema: &Value, node: &Value, index: &SchemaIndex, root: &Value) -> bool {
    let Some(obj) = node.as_object() else {
        return false;
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !required.iter().all(|r| obj.contains_key(*r)) {
        return false;
    }
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return true;
    };
    if schema.get("additionalProperties") == Some(&json!(false))
        && obj.keys().any(|k| !props.contains_key(k))
    {
        return false;
    }
    for (name, sub) in props {
        if let Some(resolved) = resolve(sub, index, root)
            && let Some(expected) = resolved.get("const")
            && obj.get(name).is_some_and(|actual| actual != expected)
        {
            return false;
        }
    }
    true
}

/// A minimal instance satisfying `schema`, for reaching constraints the
/// seed leaves unreachable.
///
/// An OPTIONAL property the seed omits hides every constraint beneath
/// it. The candidate-peer seed omits `protocol_observations`, so its
/// `maxItems`, its `uniqueItems`, its closed item object, and the bounds
/// on `protocol_id` were all invisible — and the generator reported the
/// schema as covered while testing none of them.
///
/// Prefers what the schema states about itself — `const`, `enum`,
/// `default`, `examples` — and only then builds from `type`. A `pattern`
/// is NOT interpreted here: this returns a plausible string and the
/// caller is expected to validate the result. That is deliberate. A
/// generator that silently skipped patterned strings would recreate the
/// blind spot it exists to remove, whereas a synthesized seed that turns
/// out invalid fails loudly and is fixed by adding one `examples` entry
/// to the schema.
fn synthesize(schema: &Value, index: &SchemaIndex, root: &Value) -> Option<Value> {
    let schema = resolve(schema, index, root)?;

    if let Some(c) = schema.get("const") {
        return Some(c.clone());
    }
    if let Some(first) = schema
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|m| m.first())
    {
        return Some(first.clone());
    }
    if let Some(d) = schema.get("default") {
        return Some(d.clone());
    }
    if let Some(first) = schema
        .get("examples")
        .and_then(Value::as_array)
        .and_then(|m| m.first())
    {
        return Some(first.clone());
    }

    match schema.get("type").and_then(Value::as_str)? {
        "object" => {
            let props = schema.get("properties").and_then(Value::as_object);
            let mut map = Map::new();
            for name in schema
                .get("required")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .unwrap_or_default()
            {
                let sub = props.and_then(|p| p.get(name))?;
                map.insert(name.to_owned(), synthesize(sub, index, root)?);
            }
            Some(Value::Object(map))
        }
        "array" => {
            let item = synthesize(schema.get("items")?, index, root)?;
            let min = schema
                .get("minItems")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .max(1);
            let count = usize::try_from(min).ok()?;
            // Distinct members are not synthesizable from one template,
            // so a set that must hold several is left alone rather than
            // seeded with something the schema rejects for a second
            // reason.
            if count > 1 && schema.get("uniqueItems") == Some(&json!(true)) {
                return None;
            }
            Some(Value::Array(vec![item; count]))
        }
        "string" => {
            let min = schema.get("minLength").and_then(Value::as_u64).unwrap_or(1);
            let max = schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .unwrap_or(min.max(1));
            if max < min {
                return None;
            }
            let len = usize::try_from(min.max(1).min(max)).ok()?;
            Some(json!("a".repeat(len)))
        }
        "integer" | "number" => Some(json!(
            schema.get("minimum").and_then(Value::as_u64).unwrap_or(1)
        )),
        "boolean" => Some(json!(false)),
        "null" => Some(Value::Null),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn walk(
    schema: &Value,
    node: &Value,
    pointer: &str,
    index: &SchemaIndex,
    out: &mut Generated,
    seed: &Value,
    root: &Value,
) {
    let Some(schema) = resolve(schema, index, root) else {
        // An unresolved `$ref`. Reported as a candidate so it fails
        // loudly instead of silently skipping every constraint behind it.
        out.candidates.push(Candidate {
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
    out: &mut Generated,
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

        match node.get(name) {
            Some(child) => walk(sub, child, &child_pointer, index, out, seed, root),
            None => {
                // An OPTIONAL property the seed omits. Everything the
                // schema declares beneath it — item bounds, closed item
                // objects, string lengths — was unreachable, because
                // every mutation below a missing property writes through
                // a JSON Pointer that resolves to nothing and produces
                // the seed back unchanged. The generator reported the
                // schema covered and tested none of it.
                //
                // Synthesize a value, walk the subtree against a seed
                // that carries it, and record the seed so the caller can
                // check that the value really is legal.
                if describes(schema, node, index, root)
                    && let Some(value) = synthesize(sub, index, root)
                {
                    let mut augmented = seed.clone();
                    insert_at(&mut augmented, pointer, name, value.clone());
                    out.synthesized_seeds
                        .push((child_pointer.clone(), augmented.clone()));
                    walk(sub, &value, &child_pointer, index, out, &augmented, root);
                }
            }
        }
    }
}

/// `n` distinct values, each individually valid against `items`.
///
/// The point is that the ONLY thing wrong with the resulting array is
/// its length. A list of strings against an object item schema fails on
/// type; a list of one value repeated fails on `uniqueItems`. Either
/// way the cardinality ceiling is never the reason, so an implementation
/// missing it still passes.
///
/// Returns `None` rather than something weaker when the item schema
/// offers nothing to vary. The caller records that as uncovered.
fn distinct_items(
    items: Option<&Value>,
    seeded: Option<&Value>,
    n: usize,
    index: &SchemaIndex,
    root: &Value,
) -> Option<Vec<Value>> {
    let item = items.and_then(|i| resolve(i, index, root))?;

    // Named members are already distinct and already valid.
    if let Some(members) = item.get("enum").and_then(Value::as_array)
        && members.len() >= n
    {
        return Some(members.iter().take(n).cloned().collect());
    }

    // The seed's own item where there is one: it is valid by
    // construction, which a synthesized value only claims to be.
    let base = seeded.cloned().or_else(|| synthesize(item, index, root))?;
    match &base {
        Value::String(_) => {
            let vary = varied_string(item, n)?;
            Some(vary.into_iter().map(Value::String).collect())
        }
        Value::Object(map) => {
            // A property whose subschema is a free-form string is the
            // handle. `const`, `enum`, and `pattern` are all reasons a
            // value cannot be varied without becoming invalid for a
            // second, unstated reason.
            let props = item.get("properties").and_then(Value::as_object)?;
            // A string handle first because it reads best in a failure
            // message; a numeric one otherwise. `protocol_observations`
            // pins its only string with a `pattern` and leaves
            // `observed_at` free, which is why both are tried.
            let (name, values) = map
                .keys()
                .find_map(|name| {
                    let sub = props.get(name).and_then(|s| resolve(s, index, root))?;
                    varied_string(sub, n)
                        .map(|v| (name.clone(), v.into_iter().map(Value::String).collect()))
                })
                .or_else(|| {
                    map.keys().find_map(|name| {
                        let sub = props.get(name).and_then(|s| resolve(s, index, root))?;
                        varied_number(sub, n).map(|v| (name.clone(), v))
                    })
                })?;
            let values: Vec<Value> = values;
            Some(
                values
                    .into_iter()
                    .map(|v| {
                        let mut copy = map.clone();
                        copy.insert(name.clone(), v);
                        Value::Object(copy)
                    })
                    .collect(),
            )
        }
        Value::Number(_) => Some((0..n).map(|i| json!(i)).collect()),
        _ => None,
    }
}

/// `n` distinct numbers satisfying an unpinned numeric subschema.
///
/// Honours `minimum`/`maximum`, and refuses a range too narrow to hold
/// `n` distinct values -- a bound the caller then reports rather than
/// working around.
fn varied_number(schema: &Value, n: usize) -> Option<Vec<Value>> {
    if !matches!(
        schema.get("type").and_then(Value::as_str)?,
        "integer" | "number"
    ) {
        return None;
    }
    if schema.get("const").is_some() || schema.get("enum").is_some() {
        return None;
    }
    let min = schema.get("minimum").and_then(Value::as_i64).unwrap_or(0);
    let max = schema
        .get("maximum")
        .and_then(Value::as_i64)
        .unwrap_or(i64::MAX);
    // Saturating, because an absent `maximum` is `i64::MAX` and the
    // checked form overflowed there -- turning "unbounded above" into
    // "no room", which is the opposite answer.
    let span = max.saturating_sub(min).saturating_add(1);
    if span < i64::try_from(n).ok()? {
        return None;
    }
    Some(
        (0..n)
            .map(|i| json!(min.saturating_add(i64::try_from(i).unwrap_or(0))))
            .collect(),
    )
}

/// `n` distinct strings satisfying a free-form string subschema.
///
/// Refuses anything the schema pins down: a `pattern` cannot be
/// satisfied by construction here, and `const`/`enum` mean there is no
/// freedom to use. Length bounds are honoured, and a schema too narrow
/// to hold `n` distinct values of its own is also a refusal.
fn varied_string(schema: &Value, n: usize) -> Option<Vec<String>> {
    if schema.get("type").and_then(Value::as_str)? != "string" {
        return None;
    }
    if schema.get("pattern").is_some()
        || schema.get("const").is_some()
        || schema.get("enum").is_some()
        || schema.get("format").is_some()
    {
        return None;
    }
    let min = schema
        .get("minLength")
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(0);
    let max = schema
        .get("maxLength")
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(usize::MAX);

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let mut v = format!("v{i}");
        while v.len() < min {
            v.push('x');
        }
        if v.len() > max {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

fn walk_array(
    schema: &Value,
    node: &Value,
    pointer: &str,
    index: &SchemaIndex,
    out: &mut Generated,
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
        let unique = schema.get("uniqueItems") == Some(&json!(true));

        // THE ONLY VIOLATION MUST BE THE COUNT. The old fallback built
        // `["item0", "item1", ...]` whatever the item schema said, so
        // for an array of OBJECTS the document was rejected on item
        // type and an implementation that checked the item shape and
        // forgot the ceiling entirely still passed. The repeated-item
        // mutation did not close the gap either: where `uniqueItems`
        // holds it violates that as well, so a parser enforcing
        // uniqueness and not the count also passed. Between them the
        // ceiling was never the reason for a single rejection.
        //
        // Without `uniqueItems`, repetition IS the clean mutation --
        // the seed's own item is valid by construction, and n copies of
        // it break nothing but the length. That is the `words` case: a
        // BIP-39 phrase may legally repeat a word.
        let over = if unique {
            distinct_items(items, first.as_ref(), n, index, root)
        } else {
            first
                .clone()
                .or_else(|| items.and_then(|i| synthesize(i, index, root)))
                .map(|item| std::iter::repeat_n(item, n).collect())
        };

        // UNREACHABLE IS NOT UNCOVERED. `uniqueItems` over a finite
        // enum caps the array at the number of members, so a `maxItems`
        // above that cannot be violated by any document a validator
        // would otherwise accept -- `ipc.hello` allows 8 capabilities
        // from a five-member closed set. There is nothing to test, and
        // reporting it as a gap would train the reader to ignore the
        // list.
        let capped_by_uniqueness = unique
            && items
                .and_then(|i| resolve(i, index, root))
                .and_then(|i| i.get("enum"))
                .and_then(Value::as_array)
                .is_some_and(|members| members.len() < n);

        match over {
            Some(over) => push(
                out,
                seed,
                "",
                format!("{pointer} has {n} items, over maxItems"),
                |d| {
                    set_at(d, pointer, json!(over.clone()));
                },
            ),
            None if capped_by_uniqueness => {}
            // NOT SILENT. A generator that skips a constraint it cannot
            // build for reports full coverage while testing nothing,
            // which is the failure this whole module exists to avoid.
            None => out.uncovered.push(format!(
                "{pointer}: maxItems {max} has no independent mutation -- no way to \
                 build {n} individually valid items was found"
            )),
        }

        // And the same count reached by repetition, which a set-typed
        // field collapses to one before any cap is consulted. Only
        // where `uniqueItems` holds: elsewhere it is the mutation
        // above, and pushing it twice would say nothing new.
        if unique && let Some(item) = first.clone() {
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

fn walk_string(schema: &Value, pointer: &str, out: &mut Generated, seed: &Value) {
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

fn walk_number(schema: &Value, pointer: &str, out: &mut Generated, seed: &Value) {
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
