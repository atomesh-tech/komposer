//! `JsonSchema\Validator` of justinrainbow/json-schema 6.10 (the one Composer bundles) in its default mode,
//! for the draft-04 keywords composer-schema.json and composer-lock-schema.json use, with its error order.

use komposer_package::json::{self, Flags};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

/// One `getErrors()` entry; the final `array_unique()` compares them whole.
#[derive(Clone, PartialEq)]
struct Error {
    property: String,
    pointer: String,
    message: String,
}

/// A schema and the document its `#/...` references point into.
#[derive(Clone, Copy)]
struct Node<'a> {
    schema: &'a Value,
    doc: &'a Value,
}

/// `Validator::validate()` + `JsonFile::validateJsonSchema()`'s `property : message` lines. `composer` is what
/// `./composer-schema.json` references resolve to.
pub fn validate(data: &Value, schema: &Value, composer: &Value) -> Vec<String> {
    let mut unique: Vec<Error> = vec![];
    for e in (Checker { composer }).check(data, Node { schema, doc: schema }, &[]) {
        if !unique.contains(&e) {
            unique.push(e);
        }
    }
    unique.into_iter().map(|e| if e.property.is_empty() { e.message } else { format!("{} : {}", e.property, e.message) }).collect()
}

struct Checker<'a> {
    composer: &'a Value,
}

impl<'a> Checker<'a> {
    /// `SchemaStorage::resolveRefSchema()`.
    fn resolve(&self, node: Node<'a>) -> Option<Node<'a>> {
        let Some(reference) = node.schema.get("$ref").and_then(Value::as_str) else { return Some(node) };
        let (file, fragment) = reference.split_once('#').unwrap_or((reference, ""));
        let doc = if file.is_empty() { node.doc } else { self.composer };
        let mut target = doc;
        for part in fragment.split('/').filter(|p| !p.is_empty()) {
            target = target.get(part.replace("~1", "/").replace("~0", "~"))?;
        }
        self.resolve(Node { schema: target, doc })
    }

    /// `UndefinedConstraint::check()` (via `checkUndefined()`) for a defined value: values that are missing never
    /// fail the draft-04 keywords.
    fn check(&self, value: &Value, node: Node<'a>, path: &[String]) -> Vec<Error> {
        let Some(node) = self.resolve(node) else { return vec![] };
        let Some(schema) = node.schema.as_object() else { return vec![] };
        let child = |schema: &'a Value| Node { schema, doc: node.doc };
        let mut errors = vec![];

        // validateCommonProperties()
        if let (Value::Object(object), Some(Value::Array(required))) = (value, schema.get("required")) {
            for name in required.iter().filter_map(Value::as_str).filter(|n| !object.contains_key(*n)) {
                errors.push(error(&with(path, name), format!("The property {name} is required")));
            }
        }
        errors.extend(type_error(value, schema, path));
        if let Some(not) = schema.get("not")
            && self.check(value, child(not), path).is_empty()
        {
            errors.push(error(path, "Matched a schema which it should not".into()));
        }

        // validateOfProperties()
        if let Some(Value::Array(all_of)) = schema.get("allOf") {
            let mut valid = true;
            for branch in all_of {
                let branch_errors = self.check(value, child(branch), path);
                valid &= branch_errors.is_empty();
                errors.extend(branch_errors);
            }
            if !valid {
                errors.push(error(path, "Failed to match all schemas".into()));
            }
        }
        if let Some(Value::Array(any_of)) = schema.get("anyOf") {
            let start = errors.len();
            let mut valid = false;
            for branch in any_of {
                let branch_errors = self.check(value, child(branch), path);
                valid = branch_errors.is_empty();
                if valid {
                    break;
                }
                errors.extend(branch_errors);
            }
            if valid {
                errors.truncate(start);
            } else {
                errors.push(error(path, "Failed to match at least one schema".into()));
            }
        }
        if let Some(Value::Array(one_of)) = schema.get("oneOf") {
            // each branch starts from no errors, and a failure keeps the last branch's, then all of them, then
            // the ones from before
            let before = std::mem::take(&mut errors);
            let (mut matched, mut all, mut last) = (0, vec![], None);
            for branch in one_of {
                let branch_errors = self.check(value, child(branch), path);
                matched += usize::from(branch_errors.is_empty());
                all.extend(branch_errors.iter().cloned());
                last = Some(branch_errors);
            }
            if matched == 1 {
                errors = before;
            } else {
                errors = last.unwrap_or_else(|| before.clone());
                errors.extend(all);
                errors.extend(before);
                errors.push(error(path, "Failed to match exactly one schema".into()));
            }
        }

        // validateTypes()
        match value {
            Value::Array(items) => errors.extend(self.collection(items, schema, node.doc, path)),
            Value::Object(object) => errors.extend(self.object(object, schema, node.doc, path)),
            _ => {}
        }
        if let Value::String(s) = value {
            if let Some(Value::String(pattern)) = schema.get("pattern")
                && !schema_regex(pattern).is_some_and(|re| re.is_match(s).unwrap_or(false))
            {
                errors.push(error(path, format!("Does not match the regex pattern {pattern}")));
            }
            errors.extend(format_error(value, schema, path));
        }
        if let Some(number) = numeric(value) {
            if let Some(minimum) = schema.get("minimum").and_then(numeric)
                && number < minimum
            {
                errors.push(error(path, format!("Must have a minimum value greater than or equal to {}", minimum as i64)));
            }
            if let Some(maximum) = schema.get("maximum").and_then(numeric)
                && number > maximum
            {
                errors.push(error(path, format!("Must have a maximum value less than or equal to {}", maximum as i64)));
            }
            errors.extend(format_error(value, schema, path));
        }
        if let Some(Value::Array(options)) = schema.get("enum")
            && !options
                .iter()
                .any(|o| (php_type(o) == php_type(value) && deep_equal(value, o)) || numeric(value).is_some_and(|n| numeric(o) == Some(n)))
        {
            errors.push(error(
                path,
                format!("Does not have a value in the enumeration {}", json::encode(&Value::Array(options.clone()), Flags::PLAIN)),
            ));
        }
        errors
    }

    /// `CollectionConstraint::check()`.
    fn collection(&self, items: &[Value], schema: &'a Map<String, Value>, doc: &'a Value, path: &[String]) -> Vec<Error> {
        let mut errors = vec![];
        if let Some(min) = schema.get("minItems").and_then(numeric)
            && (items.len() as f64) < min
        {
            errors.push(error(path, format!("There must be a minimum of {} items in the array, {} found", min as i64, items.len())));
        }
        match schema.get("items") {
            Some(item @ Value::Object(_)) => {
                for (k, v) in items.iter().enumerate() {
                    errors.extend(self.check(v, Node { schema: item, doc }, &with(path, &k.to_string())));
                }
            }
            Some(Value::Array(tuple)) => {
                for (k, (v, item)) in items.iter().zip(tuple).enumerate() {
                    errors.extend(self.check(v, Node { schema: item, doc }, &with(path, &k.to_string())));
                }
            }
            _ => {}
        }
        errors
    }

    /// `ObjectConstraint::check()`.
    fn object(&self, object: &Map<String, Value>, schema: &'a Map<String, Value>, doc: &'a Value, path: &[String]) -> Vec<Error> {
        let mut errors = vec![];
        let mut matched: Vec<&str> = vec![];
        for (pattern, sub) in schema.get("patternProperties").and_then(Value::as_object).into_iter().flatten() {
            let Some(re) = schema_regex(pattern) else {
                errors.push(error(path, format!("The pattern {pattern} is invalid")));
                continue;
            };
            for (name, v) in object.iter().filter(|(name, _)| re.is_match(name).unwrap_or(false)) {
                matched.push(name);
                errors.extend(self.check(v, Node { schema: sub, doc }, &with(path, name)));
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (name, definition) in properties.into_iter().flatten() {
            if let Some(v) = object.get(name)
                && definition.is_object()
            {
                errors.extend(self.check(v, Node { schema: definition, doc }, &with(path, name)));
            }
        }

        // validateElement()
        errors.extend(property_count_errors(object, Some(schema), path));
        let additional = schema.get("additionalProperties");
        for (name, v) in object {
            let definition = properties.and_then(|p| p.get(name)).filter(|d| truthy(d));
            if !matched.contains(&name.as_str()) && definition.is_none() {
                match additional {
                    Some(Value::Bool(false)) if name != "$schema" => {
                        errors
                            .push(error(path, format!("The property {name} is not defined and the definition does not allow additional properties")));
                    }
                    Some(sub @ Value::Object(_)) => errors.extend(self.check(v, Node { schema: sub, doc }, &with(path, name))),
                    _ => {}
                }
            }
            // a property's own count limits are checked here too, reported at the parent
            if let Value::Object(child) = v {
                errors.extend(property_count_errors(child, definition.and_then(Value::as_object), path));
            }
        }
        errors
    }
}

/// `ObjectConstraint::validateMinMaxConstraint()`.
fn property_count_errors(object: &Map<String, Value>, definition: Option<&Map<String, Value>>, path: &[String]) -> Vec<Error> {
    let limit = |key| definition.and_then(|d| d.get(key)).and_then(Value::as_i64);
    let mut errors = vec![];
    if let Some(min) = limit("minProperties")
        && (object.len() as i64) < min.max(0)
    {
        errors.push(error(path, format!("Must contain a minimum of {min} properties")));
    }
    if let Some(max) = limit("maxProperties")
        && (object.len() as i64) > max.max(0)
    {
        errors.push(error(path, format!("Must contain no more than {max} properties")));
    }
    errors
}

/// `TypeConstraint::check()`.
fn type_error(value: &Value, schema: &Map<String, Value>, path: &[String]) -> Option<Error> {
    let types: Vec<&str> = match schema.get("type")? {
        Value::String(t) => vec![t],
        Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
        _ => return None,
    };
    let is = |t: &str| match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "integer" => value.is_i64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        _ => true,
    };
    if types.iter().any(|t| is(t)) {
        return None;
    }
    let wording: Vec<&str> = types
        .iter()
        .map(|t| match *t {
            "integer" => "an integer",
            "number" => "a number",
            "boolean" => "a boolean",
            "object" => "an object",
            "array" => "an array",
            "string" => "a string",
            "null" => "a null",
            _ => "",
        })
        .collect();
    let expected = match wording.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => wording.join(", "),
    };
    Some(error(path, format!("{} value found, but {expected} is required", php_type(value))))
}

/// `FormatConstraint::check()` for the formats the schemas use.
fn format_error(value: &Value, schema: &Map<String, Value>, path: &[String]) -> Option<Error> {
    let s = value.as_str()?;
    match schema.get("format")?.as_str()? {
        "uri" if !is_uri(s) => Some(error(path, "Invalid URL format".into())),
        "email" if !is_email(s) => Some(error(path, "Invalid email".into())),
        _ => None,
    }
}

/// `UriValidator::isValid()`: PCRE without the `u` flag works on bytes, and its `$` also matches before a final
/// newline.
fn is_uri(uri: &str) -> bool {
    use regex::bytes::Regex;
    static HIERARCHICAL: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i-u)^([a-z][a-z0-9+\-.]*)://(?:([^:@/?#]+)(?::([^@/?#]*))?@)?([a-z0-9._~-]+|\[[a-f0-9:.]+\])(?::(\d{1,5}))?(/[a-zA-Z0-9._~!$&'()*+,;=:@/%-]*)*(\?([^#]*))?(\#(.*))?\n?\z").unwrap()
    });
    static OTHER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)^(mailto|data|urn|news|tel|file):(.+)\n?\z").unwrap());
    static MAILTO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}\n?\z").unwrap());
    static NEWS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i-u)^[a-z0-9]+(\.[a-z0-9]+)*\n?\z").unwrap());
    static TEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?-u)^\+?[0-9.\-() ]+\n?\z").unwrap());
    if let Some(m) = HIERARCHICAL.captures(uri.as_bytes()) {
        let host = m.get(4).map_or(&b""[..], |h| h.as_bytes());
        // `!empty()`: a port of "0" is not checked
        let port = m.get(5).map(|p| p.as_bytes()).filter(|p| *p != b"0");
        let bad_port = port.and_then(|p| std::str::from_utf8(p).ok()?.parse::<u32>().ok()).is_some_and(|p| !(1..=65535).contains(&p));
        return !host.windows(2).any(|w| w == b"..") && !bad_port;
    }
    let Some(m) = OTHER.captures(uri.as_bytes()) else { return false };
    let rest = &m[2];
    match m[1].to_ascii_lowercase().as_slice() {
        b"mailto" => MAILTO.is_match(rest),
        b"news" => NEWS.is_match(rest),
        b"tel" => TEL.is_match(rest),
        _ => true,
    }
}

/// `filter_var($email, FILTER_VALIDATE_EMAIL, FILTER_FLAG_EMAIL_UNICODE)`: php-src's regex (based on one by
/// Michael Rushton, routable addresses only), with its `iDu` flags.
///
/// The original regex carries this notice: Copyright © Michael Rushton 2009-10, http://squiloople.com/
/// Feel free to use and redistribute this code. But please keep this copyright notice.
fn is_email(email: &str) -> bool {
    static EMAIL: LazyLock<fancy_regex::Regex> = LazyLock::new(|| {
        fancy_regex::Regex::new(concat!(
            r"(?i)^(?!(?:(?:\x22?\x5C[\x00-\x7E]\x22?)|(?:\x22?[^\x5C\x22]\x22?)){255,})(?!(?:(?:\x22?\x5C[\x00-\x7E]\x22?)|(?:\x22?[^\x5C\x22]\x22?)){65,}@)",
            r"(?:(?:[\x21\x23-\x27\x2A\x2B\x2D\x2F-\x39\x3D\x3F\x5E-\x7E\pL\pN]+)|(?:\x22(?:[\x01-\x08\x0B\x0C\x0E-\x1F\x21\x23-\x5B\x5D-\x7F\pL\pN]|(?:\x5C[\x00-\x7F]))*\x22))",
            r"(?:\.(?:(?:[\x21\x23-\x27\x2A\x2B\x2D\x2F-\x39\x3D\x3F\x5E-\x7E\pL\pN]+)|(?:\x22(?:[\x01-\x08\x0B\x0C\x0E-\x1F\x21\x23-\x5B\x5D-\x7F\pL\pN]|(?:\x5C[\x00-\x7F]))*\x22)))*",
            r"@(?:(?:(?!.*[^.]{64,})(?:(?:(?:xn--)?[a-z0-9]+(?:-+[a-z0-9]+)*\.){1,126}){1,}(?:(?:[a-z][a-z0-9]*)|(?:(?:xn--)[a-z0-9]+))(?:-+[a-z0-9]+)*)",
            r"|(?:\[(?:(?:IPv6:(?:(?:[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){7})|(?:(?!(?:.*[a-f0-9][:\]]){7,})(?:[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){0,5})?::(?:[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){0,5})?)))",
            r"|(?:(?:IPv6:(?:(?:[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){5}:)|(?:(?!(?:.*[a-f0-9]:){5,})(?:[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){0,3})?::(?:[a-f0-9]{1,4}(?::[a-f0-9]{1,4}){0,3}:)?)))?",
            r"(?:(?:25[0-5])|(?:2[0-4][0-9])|(?:1[0-9]{2})|(?:[1-9]?[0-9]))(?:\.(?:(?:25[0-5])|(?:2[0-4][0-9])|(?:1[0-9]{2})|(?:[1-9]?[0-9]))){3}))\]))$",
        ))
        .unwrap()
    });
    // the maximum length of an e-mail address is 320 octets, per RFC 2821
    email.len() <= 320 && EMAIL.is_match(email).unwrap_or(false)
}

/// A schema's `pattern` as `preg_match('~pattern~u')` sees it: `$` also matches before a final newline and `\d` is
/// ASCII only. `None` when it does not compile.
fn schema_regex(pattern: &str) -> Option<fancy_regex::Regex> {
    static CACHE: LazyLock<Mutex<HashMap<String, Option<fancy_regex::Regex>>>> = LazyLock::new(Mutex::default);
    let mut cache = CACHE.lock().unwrap();
    cache
        .entry(pattern.to_string())
        .or_insert_with(|| {
            let (mut out, mut chars, mut in_class) = (String::new(), pattern.chars(), false);
            while let Some(c) = chars.next() {
                match c {
                    '\\' => match chars.next() {
                        Some('d') => out.push_str(if in_class { "0-9" } else { "[0-9]" }),
                        Some(e) => {
                            out.push('\\');
                            out.push(e);
                        }
                        None => out.push('\\'),
                    },
                    '[' if !in_class => {
                        in_class = true;
                        out.push(c);
                    }
                    ']' if in_class => {
                        in_class = false;
                        out.push(c);
                    }
                    '$' if !in_class => out.push_str(r"(?=\n?\z)"),
                    c => out.push(c),
                }
            }
            fancy_regex::Regex::new(&out).ok()
        })
        .clone()
}

fn error(path: &[String], message: String) -> Error {
    let property: String = path.iter().map(|p| if let Some(n) = numeric_str(p) { format!("[{}]", n as i64) } else { format!(".{p}") }).collect();
    let pointer = path.iter().map(|p| p.replace('~', "~0").replace('/', "~1")).collect::<Vec<_>>().join("/");
    let mut chars = message.chars();
    let message = chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default();
    Error { property: property.trim_matches('.').to_string(), pointer, message }
}

fn with(path: &[String], segment: &str) -> Vec<String> {
    let mut path = path.to_vec();
    path.push(segment.to_string());
    path
}

/// PHP `gettype()` of a decoded JSON value.
fn php_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NULL",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() => "integer",
        Value::Number(_) => "double",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// PHP truthiness of a decoded JSON value (objects are always true).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty() && s != "0",
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// `DeepComparer::isEqual()`: numbers compare by value, everything else strictly.
fn deep_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| deep_equal(x, y)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| deep_equal(v, w))),
        _ => a == b,
    }
}

/// What PHP skips around numeric strings.
const WHITESPACE: [char; 6] = [' ', '\t', '\n', '\r', '\x0B', '\x0C'];

/// The value of a number or PHP `is_numeric()` string.
fn numeric(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => numeric_str(s),
        _ => None,
    }
}

fn numeric_str(s: &str) -> Option<f64> {
    is_numeric(s).then(|| s.trim_matches(WHITESPACE).parse().ok())?
}

/// PHP 8 `is_numeric()` on a string: optional surrounding whitespace, sign, digits with an optional fraction and
/// exponent.
fn is_numeric(s: &str) -> bool {
    let s = s.trim_matches(WHITESPACE).as_bytes();
    let s = s.strip_prefix(b"+").or_else(|| s.strip_prefix(b"-")).unwrap_or(s);
    let digits = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
    let int = digits(s);
    let mut rest = &s[int..];
    let mut fraction = 0;
    if let Some(r) = rest.strip_prefix(b".") {
        fraction = digits(r);
        rest = &r[fraction..];
    }
    if int + fraction == 0 {
        return false;
    }
    if let Some(r) = rest.strip_prefix(b"e").or_else(|| rest.strip_prefix(b"E")) {
        let r = r.strip_prefix(b"+").or_else(|| r.strip_prefix(b"-")).unwrap_or(r);
        let exponent = digits(r);
        return exponent > 0 && exponent == r.len();
    }
    rest.is_empty()
}
