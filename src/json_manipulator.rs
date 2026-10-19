//! `Composer\Json\JsonManipulator`: formatting-preserving edits of composer.json.
//!
//! Composer does this with recursive PCRE patterns; here a small scanner finds the
//! same spans (the value of a top-level key, a JSON value starting at an offset) and
//! the text surgery around them is ported as is. Decoded data follows PHP array
//! semantics (`json_decode($json, true)`), see `php_get` and `php_array`.

use komposer_package::is_platform_package;
use komposer_package::json::{self, Flags};
use komposer_package::loader::php_empty;
use regex::Regex;
use serde_json::{Map, Value};
use std::cmp::Ordering;

pub struct JsonManipulator {
    contents: String,
    newline: &'static str,
    indent: String,
}

impl JsonManipulator {
    pub fn new(contents: &str) -> anyhow::Result<JsonManipulator> {
        let mut contents = contents.trim_matches([' ', '\t', '\n', '\r', '\0', '\x0b']).to_string();
        if contents.is_empty() {
            contents = "{}".into();
        }
        if !(contents.starts_with('{') && contents.ends_with('}')) {
            anyhow::bail!("The json file must be an object ({{}})");
        }
        let newline = if contents.contains("\r\n") { "\r\n" } else { "\n" };
        if contents == "{}" {
            contents = format!("{{{newline}}}");
        }
        let indent = detect_indenting(&contents);
        Ok(JsonManipulator { contents, newline, indent })
    }

    pub fn contents(&self) -> String {
        format!("{}{}", self.contents, self.newline)
    }

    /// `JsonFile::parseJson()`; `None` where it throws on invalid JSON.
    fn decoded(&self) -> Option<Value> {
        json::parse(&self.contents).ok()
    }

    /// `json_decode($this->contents, false)->repositories`: objects stay objects, even empty ones.
    fn raw_repositories(&self) -> Option<Value> {
        serde_json::from_str::<Value>(&self.contents).ok()?.get_mut("repositories").map(Value::take)
    }

    fn splice(&mut self, span: &MemberSpan, value: &str) {
        self.contents.replace_range(span.value_start..span.value_end, value);
    }

    /// `addLink()`.
    pub fn add_link(&mut self, kind: &str, package: &str, constraint: &str, sort_packages: bool) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        if !isset(&decoded, kind) {
            return self.add_main_key(kind, &object(package, constraint.into()));
        }
        let Some(span) = top_level_member(&self.contents, kind, None) else { return false };
        let mut links = self.contents[span.value_start..span.value_end].to_string();

        let existing = Regex::new(&format!(r#"(?i)"({})"(\s*:\s*){STRING}"#, escaped_slash_pattern(&strip_x(package)))).unwrap();
        if let Some(c) = existing.captures(&links) {
            let existing_name = c[1].replace("\\/", "/");
            let replace = Regex::new(&format!(r#"(?i)"{}"(\s*:\s*){STRING}"#, escaped_slash_pattern(&c[1]))).unwrap();
            let encoded = encode_str(&existing_name);
            // the constraint is not escaped
            links = replace.replace_all(&links, |m: &regex::Captures| format!("{encoded}{}\"{constraint}\"", &m[1])).into_owned();
        } else if let Some(c) = Regex::new(r"(?s)^\s*\{\s*\S+.*?(\s*\}\s*)$").unwrap().captures(&links) {
            // link missing but non empty links
            let tail = c[1].to_string();
            links.truncate(links.len() - tail.len());
            links = format!("{links},{nl}{i}{i}{}: {}{tail}", encode_str(package), encode_str(constraint), nl = self.newline, i = self.indent);
        } else {
            links = format!("{{{nl}{i}{i}{}: {}{nl}{i}}}", encode_str(package), encode_str(constraint), nl = self.newline, i = self.indent);
        }

        if sort_packages {
            let Ok(Value::Object(mut reqs)) = json::parse(&links) else { return false };
            sort_packages_map(&mut reqs);
            links = self.format(&Value::Object(reqs), 0, false);
        }

        self.splice(&span, &links);
        true
    }

    /// `addRepository()`: `config` is a definition or `false` (disables the named repository).
    pub fn add_repository(&mut self, name: &str, config: &Value, append: bool) -> bool {
        if (!name.is_empty() && !self.do_remove_repository(name)) || !self.do_convert_repositories_from_assoc_to_list() {
            return false;
        }
        let item = match config {
            Value::Bool(false) => object(name, Value::Bool(false)),
            _ => named_repository(name, config),
        };
        self.add_list_item("repositories", &item, append)
    }

    /// `doConvertRepositoriesFromAssocToList()`.
    fn do_convert_repositories_from_assoc_to_list(&mut self) -> bool {
        let Some(Value::Object(repos)) = self.raw_repositories() else { return true };
        // delete from bottom to top, to ensure keys stay the same
        if !repos.keys().rev().all(|key| self.remove_sub_node("repositories", key)) {
            return false;
        }
        self.change_empty_main_key_from_assoc_to_list("repositories");
        repos.into_iter().all(|(name, repo)| {
            let item = match repo {
                Value::Object(_) if is_numeric(&name) => repo,
                Value::Object(_) => named_repository(&name, &repo),
                other => object(&name, other),
            };
            self.add_list_item("repositories", &item, true)
        })
    }

    /// `setRepositoryUrl()`.
    pub fn set_repository_url(&mut self, name: &str, url: &str) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        let Some(repos) = php_get(&decoded, "repositories") else { return false };
        let Some(repos_span) = top_level_member(&self.contents, "repositories", None) else { return false };
        let repos_text = &self.contents[repos_span.value_start..repos_span.value_end];
        // the span of the repository object within `repos_text`
        let repo_span = match repos {
            Value::Array(list) => list.iter().position(|r| has_name(r, name)).and_then(|index| {
                let (start, end) = *list_items(repos_text)?.get(index)?;
                (repos_text.as_bytes()[start] == b'{').then_some((start, end))
            }),
            Value::Object(m) => m
                .iter()
                .find(|(key, repo)| (!is_int_key(key) && *key == name) || has_name(repo, name))
                // an integer key is matched unquoted by Composer's pattern, which never matches
                .filter(|(key, _)| !is_int_key(key))
                .and_then(|(key, _)| top_level_member(repos_text, key, Some(b'{')))
                .map(|s| (s.value_start, s.value_end)),
            _ => None,
        };
        let Some((start, end)) = repo_span else { return false };
        let (start, end) = (repos_span.value_start + start, repos_span.value_start + end);
        if let Some(url_span) = top_level_member(&self.contents[start..end], "url", Some(b'"')) {
            self.contents.replace_range(start + url_span.value_start..start + url_span.value_end, &encode_str(url));
        }
        true
    }

    /// `insertRepository()`.
    pub fn insert_repository(&mut self, name: &str, config: &Value, reference: &str, offset: usize) -> bool {
        if (!name.is_empty() && !self.do_remove_repository(name)) || !self.do_convert_repositories_from_assoc_to_list() {
            return false;
        }
        let Some(decoded) = self.decoded() else { return false };
        let repos = php_array(php_get(&decoded, "repositories").cloned().unwrap_or_default());
        let Some(index) = repos.iter().position(|(key, repo)| {
            has_name(repo, reference) || (!is_int_key(key) && key == reference) || *repo == object(reference, Value::Bool(false))
        }) else {
            return false;
        };
        let item = match config {
            // Composer quirk: not `[$name => false]` as in addRepository()
            Value::Bool(false) => object("name", Value::Bool(false)),
            _ => named_repository(name, config),
        };
        self.insert_list_item("repositories", &item, index + offset)
    }

    /// `removeRepository()`.
    pub fn remove_repository(&mut self, name: &str) -> bool {
        self.do_remove_repository(name) && self.remove_main_key_if_empty("repositories")
    }

    /// `doRemoveRepository()`.
    fn do_remove_repository(&mut self, name: &str) -> bool {
        let (entries, is_assoc): (Vec<(String, Value)>, bool) = match self.raw_repositories() {
            Some(Value::Object(m)) => (m.into_iter().collect(), true),
            Some(Value::Array(a)) => (a.into_iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(), false),
            _ => return true,
        };
        for (i, (key, repo)) in entries.iter().enumerate() {
            // integer-like object keys are integers in PHP and never `===` a string
            if is_assoc && !is_int_key(key) && key == name {
                return self.remove_sub_node("repositories", key);
            }
            if has_name(repo, name) {
                return if is_assoc { self.remove_sub_node("repositories", key) } else { self.remove_list_item("repositories", i) };
            }
            if !is_assoc && repo.as_object().is_some_and(|m| m.len() == 1 && m.get(name) == Some(&Value::Bool(false))) {
                return self.remove_list_item("repositories", i);
            }
        }
        true
    }

    /// `addConfigSetting()`.
    pub fn add_config_setting(&mut self, name: &str, value: &Value) -> bool {
        // policy.<list>.<field> becomes a policy.<list> update with the merged list block
        if let Some((list, field)) = policy_field(name) {
            if field.contains('.') {
                return false;
            }
            let mut block = self.policy_list_block(list);
            block.insert(field.into(), value.clone());
            return self.add_sub_node("config", &format!("policy.{list}"), &Value::Object(block), true);
        }
        self.add_sub_node("config", name, value, true)
    }

    /// `removeConfigSetting()`.
    pub fn remove_config_setting(&mut self, name: &str) -> bool {
        if let Some((list, field)) = policy_field(name) {
            if field.contains('.') {
                return false;
            }
            let mut block = self.policy_list_block(list);
            if block.shift_remove(field).is_none() {
                return true;
            }
            if block.is_empty() {
                return self.remove_config_setting(&format!("policy.{list}"));
            }
            return self.add_sub_node("config", &format!("policy.{list}"), &Value::Object(block), true);
        }
        self.remove_sub_node("config", name)
    }

    /// `getCurrentPolicyListBlock()`.
    fn policy_list_block(&self, list: &str) -> Map<String, Value> {
        let decoded = self.decoded().unwrap_or_default();
        let block = php_get(&decoded, "config").and_then(|c| php_get(c, "policy")).and_then(|p| php_get(p, list));
        php_array(block.cloned().unwrap_or_default())
    }

    /// `addProperty()`.
    pub fn add_property(&mut self, name: &str, value: &Value) -> bool {
        for prefix in ["suggest", "extra", "scripts"] {
            if let Some(rest) = name.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) {
                return self.add_sub_node(prefix, rest, value, true);
            }
        }
        self.add_main_key(name, value)
    }

    /// `removeProperty()`.
    pub fn remove_property(&mut self, name: &str) -> bool {
        for prefix in ["suggest", "extra", "scripts", "autoload", "autoload-dev"] {
            if let Some(rest) = name.strip_prefix(prefix).and_then(|r| r.strip_prefix('.')) {
                return self.remove_sub_node(prefix, rest);
            }
        }
        self.remove_main_key(name)
    }

    /// `addSubNode()`.
    pub fn add_sub_node(&mut self, main_node: &str, name: &str, value: &Value, append: bool) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        let (name, sub_name) = split_sub_name(main_node, name);
        let entry_value = || match &sub_name {
            Some(sub) => object(sub, value.clone()),
            None => value.clone(),
        };

        if !isset(&decoded, main_node) {
            self.add_main_key(main_node, &object(&name, entry_value()));
            return true;
        }

        let Some(span) = top_level_member(&self.contents, main_node, Some(b'{')) else { return false };
        let mut children = self.contents[span.value_start..span.value_end].to_string();
        if serde_json::from_str::<Value>(&children).is_err() {
            return false;
        }

        // every `"name": value` in the node, nested ones included
        let child = Regex::new(&format!(r#""{}"\s*:\s*"#, regex::escape(&strip_x(&name)))).unwrap();
        let occurrences = find_members(&children, &child);
        if !occurrences.is_empty() {
            let mut out = String::new();
            let mut last = 0;
            for (_, value_start, value_end) in occurrences {
                let new_value = match &sub_name {
                    Some(sub) => {
                        let mut cur = php_array(json::parse(&children[value_start..value_end]).unwrap_or_default());
                        cur.insert(sub.clone(), value.clone());
                        Value::Object(cur)
                    }
                    None => value.clone(),
                };
                out.push_str(&children[last..value_start]);
                out.push_str(&self.format(&new_value, 1, false));
                last = value_end;
            }
            out.push_str(&children[last..]);
            children = out;
        } else if let Some(c) = Regex::new(r"(?s)^\{(\s*?)(\S+.*?)?(\s*)\}$").unwrap().captures(&children) {
            let (leading, trailing) = (c[1].to_string(), c[3].to_string());
            let entry = format!("{}: {}", encode_str(&name), self.format(&entry_value(), 1, false));
            let (nl, i) = (self.newline, &self.indent);
            children = if c.get(2).is_none() {
                format!("{{{nl}{i}{i}{entry}{trailing}}}")
            } else if append {
                format!("{},{nl}{i}{i}{entry}{trailing}}}", &children[..children.len() - trailing.len() - 1])
            } else {
                format!("{{{leading}{entry},{nl}{i}{i}{}", &children[1 + leading.len()..])
            };
        } else {
            return false;
        }

        self.splice(&span, &children);
        true
    }

    /// `removeSubNode()`.
    pub fn remove_sub_node(&mut self, main_node: &str, name: &str) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        let Some(node) = php_get(&decoded, main_node).filter(|v| !php_empty(v)) else { return true };
        let Some(span) = top_level_member(&self.contents, main_node, Some(b'{')) else { return false };
        let children = self.contents[span.value_start..span.value_end].to_string();
        if !json::parse(&children).is_ok_and(|v| !php_empty(&v)) {
            return false;
        }

        let (name, sub_name) = split_sub_name(main_node, name);
        let Some(current) = php_get(node, &name).filter(|v| !v.is_null()) else { return true };
        // `$subName && !isset(...)`: "0" and "" are falsy
        if let Some(sub) = sub_name.as_deref().filter(|s| !matches!(*s, "" | "0"))
            && !isset(current, sub)
        {
            return true;
        }

        let mut children_clean = children.clone();
        if Regex::new(&format!(r#"(?i)"{}"\s*:"#, escaped_slash_pattern(&name))).unwrap().is_match(&children) {
            // unlike the check above, this lookup is case-sensitive and under the `x` modifier
            let key = Regex::new(&format!(r#""{}"\s*:\s*"#, escaped_slash_pattern(&strip_x(&name)))).unwrap();
            // the first of the longest matches
            let Some(best) = find_members(&children, &key)
                .into_iter()
                .map(|(start, _, end)| &children[start..end])
                .reduce(|best, m| if m.len() > best.len() { m } else { best })
            else {
                return false; // Composer throws here
            };
            let quoted = regex::escape(best);
            let (cleaned, count) = replace_counting(&children, &format!(r"(?i),\s*{quoted}"));
            children_clean = cleaned;
            if count != 1 {
                let (cleaned, count) = replace_counting(&children_clean, &format!(r"(?i){quoted}\s*,?\s*"));
                if count != 1 {
                    return false;
                }
                children_clean = cleaned;
            }
        }

        // `unset($curVal[$name][$subName])`; an emptied node stays an object
        let without_sub = |sub: &str| {
            let mut cur = php_array(json::parse(&children).unwrap_or_default());
            if let Some(node) = cur.get_mut(&name).filter(|v| v.is_array() || v.is_object()) {
                let mut inner = php_array(node.take());
                inner.shift_remove(sub);
                *node = Value::Object(inner);
            }
            cur
        };

        let no_children_left = Regex::new(r"(?s)^\{\s*?(\S+.*?)?(\s*)\}$").unwrap().captures(&children_clean).is_some_and(|c| c.get(1).is_none());
        if no_children_left {
            let empty = format!("{{{}{}}}", self.newline, self.indent);
            self.splice(&span, &empty);
            if let Some(sub) = &sub_name {
                let rest = without_sub(sub).shift_remove(&name).unwrap_or_default();
                self.add_sub_node(main_node, &name, &rest, true);
            }
            return true;
        }

        let replacement = match &sub_name {
            Some(sub) => self.format(&Value::Object(without_sub(sub)), 0, true),
            None => children_clean,
        };
        self.splice(&span, &replacement);
        true
    }

    /// `addListItem()`.
    pub fn add_list_item(&mut self, main_node: &str, value: &Value, append: bool) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        if !isset(&decoded, main_node) && !self.add_main_key(main_node, &Value::Array(vec![])) {
            return false;
        }
        let Some(span) = top_level_member(&self.contents, main_node, Some(b'[')) else { return false };
        let children = &self.contents[span.value_start..span.value_end];
        let inner = &children[1..children.len() - 1];
        let lead = skip_ws(inner.as_bytes(), 0);
        let empty = lead == inner.len();
        // `^\[(\s*?)(\S+.*?)?(\s*)\]$`
        let (leading, trailing) = if empty { ("", inner) } else { (&inner[..lead], &inner[inner.trim_end_matches(is_ws).len()..]) };
        let (nl, i) = (self.newline, &self.indent);
        // one-line lists stay on one line
        let (item_lead, item_trail, depth) =
            if trailing.contains(nl) { (format!("{nl}{i}{i}"), trailing, 1) } else { (leading.to_string(), leading, 0) };
        let item = self.format(value, depth, false);
        let children = if empty {
            format!("[{item_lead}{item}{item_trail}]")
        } else if append {
            format!("{},{item_lead}{item}{item_trail}]", &children[..children.len() - trailing.len() - 1])
        } else {
            format!("[{leading}{item},{item_lead}{}", &children[1 + leading.len()..])
        };
        self.splice(&span, &children);
        true
    }

    /// `insertListItem()`.
    pub fn insert_list_item(&mut self, main_node: &str, value: &Value, index: usize) -> bool {
        if index == 0 {
            return self.add_list_item(main_node, value, false);
        }
        let Some(decoded) = self.decoded() else { return false };
        let count = php_get(&decoded, main_node).map_or(0, |v| php_array(v.clone()).len());
        if !isset(&decoded, main_node) && !self.add_main_key(main_node, &Value::Array(vec![])) {
            return false;
        }
        if count == index {
            return self.add_list_item(main_node, value, true);
        }
        let Some(span) = top_level_member(&self.contents, main_node, Some(b'[')) else { return false };
        let children = &self.contents[span.value_start..span.value_end];
        // `^\[\s*((?&json)\s*+,\s*?){index}(\s*)`: right after the index-th comma, then its whitespace
        let Some(items) = list_items(children) else { return false };
        let (Some(prev), Some(next)) = (items.get(index - 1), items.get(index)) else { return true };
        let comma = skip_ws(children.as_bytes(), prev.1);
        let space = &children[comma + 1..next.0];
        let children = format!("{}{space}{},{space}{}", &children[..=comma], self.format(value, 1, false), &children[next.0..]);
        self.splice(&span, &children);
        true
    }

    /// `removeListItem()`.
    pub fn remove_list_item(&mut self, main_node: &str, index: usize) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        let node = php_get(&decoded, main_node);
        if node.is_some_and(|v| v.as_array().is_some_and(Vec::is_empty)) {
            return true;
        }
        let Some(span) = top_level_member(&self.contents, main_node, Some(b'[')) else { return false };
        if !node.is_some_and(|v| isset(v, &index.to_string())) {
            return true;
        }
        let children = &self.contents[span.value_start..span.value_end];
        let Some(items) = list_items(children) else { return false };
        // the first item goes with the separator after it, the others with the separator before
        let (from, to) = match index {
            0 => (items[0].0, items.get(1).map_or(children.len() - 1, |next| next.0)),
            _ => (skip_ws(children.as_bytes(), items[index - 1].1), items[index].1),
        };
        let children = format!("{}{}", &children[..from], &children[to..]);
        self.splice(&span, &children);
        true
    }

    /// `addMainKey()`.
    pub fn add_main_key(&mut self, key: &str, content: &Value) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        let content = self.format(content, 0, false);
        let member = format!("{}: {content}", encode_str(key));

        if isset(&decoded, key)
            && let Some(span) = top_level_member(&self.contents, key, None)
        {
            self.contents.replace_range(span.key_start..span.value_end, &member);
            return true;
        }

        // append at the end of the file, keeping the whitespace before `}` if there are members
        let (cut, sep) = match Regex::new(r"[^{\s](\s*)\}$").unwrap().captures(&self.contents) {
            Some(c) => (c[1].len() + 1, format!(",{}", self.newline)),
            None => (1, String::new()),
        };
        self.contents.truncate(self.contents.len() - cut);
        self.contents = format!("{}{sep}{}{member}{}}}", self.contents, self.indent, self.newline);
        true
    }

    /// `removeMainKey()`.
    pub fn remove_main_key(&mut self, key: &str) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        if php_get(&decoded, key).is_none() {
            return true;
        }
        let Some(span) = top_level_member(&self.contents, key, None) else { return false };
        // `\s*,?\s*` after the removed member
        let b = self.contents.as_bytes();
        let mut end_at = skip_ws(b, span.value_end);
        if b.get(end_at) == Some(&b',') {
            end_at = skip_ws(b, end_at + 1);
        }
        let mut start = self.contents[..span.key_start].to_string();
        let end = &self.contents[end_at..];

        // do not leave a dangling comma on the previous line if the last member was removed
        let dangling = Regex::new(r",(\s*)$").unwrap();
        if end == "}" && dangling.is_match(&start) {
            start = dangling.replace(&start, "$1").into_owned();
            let indent_chars: Vec<char> = self.indent.chars().collect();
            start.truncate(start.trim_end_matches(&indent_chars[..]).len());
        }
        self.contents = start + end;
        if Regex::new(r"^\{\s*\}\s*$").unwrap().is_match(&self.contents) {
            self.contents = "{\n}".into();
        }
        true
    }

    /// `changeEmptyMainKeyFromAssocToList()`.
    pub fn change_empty_main_key_from_assoc_to_list(&mut self, key: &str) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        if php_get(&decoded, key).is_none() {
            return true;
        }
        let Some(span) = top_level_member(&self.contents, key, Some(b'{')) else { return false };
        let space = &self.contents[span.value_start + 1..span.value_end - 1];
        if skip_ws(space.as_bytes(), 0) != space.len() {
            return false;
        }
        let list = format!("[{space}]");
        self.splice(&span, &list);
        true
    }

    /// `removeMainKeyIfEmpty()`.
    pub fn remove_main_key_if_empty(&mut self, key: &str) -> bool {
        let Some(decoded) = self.decoded() else { return false };
        match php_get(&decoded, key) {
            Some(v @ (Value::Array(_) | Value::Object(_))) if php_empty(v) => self.remove_main_key(key),
            _ => true,
        }
    }

    /// `format()`: an empty `Value::Object` is a `stdClass`, non-empty maps keyed `0..n` are PHP lists.
    pub fn format(&self, data: &Value, depth: usize, was_object: bool) -> String {
        let close = format!("{}{}}}", self.newline, self.indent.repeat(depth + 1));
        match data {
            Value::Object(m) if m.is_empty() => format!("{{{close}"),
            Value::Array(a) if a.is_empty() => {
                if was_object {
                    format!("{{{close}")
                } else {
                    "[]".into()
                }
            }
            Value::Array(a) => self.format_list(a.iter(), depth),
            Value::Object(m) if is_list(m) => self.format_list(m.values(), depth),
            Value::Object(m) => {
                let elems: Vec<String> = m
                    .iter()
                    .map(|(k, v)| format!("{}{}: {}", self.indent.repeat(depth + 2), encode_str(k), self.format(v, depth + 1, false)))
                    .collect();
                format!("{{{}{}{close}", self.newline, elems.join(&format!(",{}", self.newline)))
            }
            v => json::encode(v, Flags::FILE),
        }
    }

    fn format_list<'a>(&self, items: impl Iterator<Item = &'a Value>, depth: usize) -> String {
        format!("[{}]", items.map(|v| self.format(v, depth + 1, false)).collect::<Vec<_>>().join(", "))
    }
}

/// `RequireCommand`/`JsonManipulator::sortPackages()`: platform packages first, natural order.
pub fn sort_packages_map(packages: &mut Map<String, Value>) {
    let prefix = |name: &str| -> String {
        if is_platform_package(name) {
            for (i, p) in ["php", "hhvm", "ext", "lib"].iter().enumerate() {
                if name.starts_with(p) {
                    return format!("{i}-{name}");
                }
            }
            if !name.starts_with(|c: char| c.is_ascii_digit()) {
                return format!("4-{name}");
            }
            return name.to_string();
        }
        format!("5-{name}")
    };
    let mut entries: Vec<(String, Value)> = std::mem::take(packages).into_iter().collect();
    entries.sort_by(|(a, _), (b, _)| strnatcmp(&prefix(a), &prefix(b)));
    packages.extend(entries);
}

/// PHP `strnatcmp()`, a port of `strnatcmp_ex()`.
pub fn strnatcmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.is_empty() || b.is_empty() {
        return a.len().cmp(&b.len());
    }
    // reading at the end yields the C string terminator
    let at = |s: &[u8], i: usize| s.get(i).copied().unwrap_or(0);
    let ended = |i: usize, j: usize| match (i >= a.len(), j >= b.len()) {
        (false, false) => None,
        (a_ended, b_ended) => Some(b_ended.cmp(&a_ended)),
    };
    let (mut i, mut j) = (0, 0);
    // leading zeros are only skipped at the start
    while a[i] == b'0' && at(a, i + 1).is_ascii_digit() {
        i += 1;
    }
    while b[j] == b'0' && at(b, j + 1).is_ascii_digit() {
        j += 1;
    }
    loop {
        while matches!(at(a, i), b' ' | b'\t'..=b'\r') {
            i += 1;
        }
        while matches!(at(b, j), b' ' | b'\t'..=b'\r') {
            j += 1;
        }
        if at(a, i).is_ascii_digit() && at(b, j).is_ascii_digit() {
            // digit runs starting with 0 compare left-aligned, others by length first
            let fractional = at(a, i) == b'0' || at(b, j) == b'0';
            let mut bias = Ordering::Equal;
            loop {
                match (at(a, i).is_ascii_digit(), at(b, j).is_ascii_digit()) {
                    (false, false) => break,
                    (false, true) => return Ordering::Less,
                    (true, false) => return Ordering::Greater,
                    (true, true) => {}
                }
                let ord = at(a, i).cmp(&at(b, j));
                if fractional && ord.is_ne() {
                    return ord;
                }
                bias = bias.then(ord);
                i += 1;
                j += 1;
            }
            if bias.is_ne() {
                return bias;
            }
            if let Some(ord) = ended(i, j) {
                return ord;
            }
        }
        let ord = at(a, i).cmp(&at(b, j));
        if ord.is_ne() {
            return ord;
        }
        i += 1;
        j += 1;
        if let Some(ord) = ended(i, j) {
            return ord;
        }
    }
}

fn split_sub_name(main_node: &str, name: &str) -> (String, Option<String>) {
    if matches!(main_node, "config" | "extra" | "scripts")
        && let Some((n, sub)) = name.split_once('.')
    {
        return (n.to_string(), Some(sub.to_string()));
    }
    (name.to_string(), None)
}

/// `$array[$key]` on decoded JSON, where list keys are the canonical integers.
pub(crate) fn php_get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key),
        Value::Array(a) => key.parse::<usize>().ok().filter(|i| i.to_string() == key).and_then(|i| a.get(i)),
        _ => None,
    }
}

/// `isset($array[$key])`.
fn isset(v: &Value, key: &str) -> bool {
    php_get(v, key).is_some_and(|v| !v.is_null())
}

/// Decoded JSON as a PHP array with string keys, `[]` for anything but an array.
pub(crate) fn php_array(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        Value::Array(a) => a.into_iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => Map::new(),
    }
}

/// `array_is_list()`.
fn is_list(m: &Map<String, Value>) -> bool {
    m.keys().enumerate().all(|(i, k)| *k == i.to_string())
}

/// `[$key => $value]`.
fn object(key: &str, value: Value) -> Value {
    Value::Object(Map::from_iter([(key.to_string(), value)]))
}

/// `['name' => $name] + $config` for a named (not numeric, not empty) repository.
fn named_repository(name: &str, config: &Value) -> Value {
    if !(config.is_object() || config.is_array()) || name.is_empty() || is_numeric(name) {
        return config.clone();
    }
    let mut out = Map::from_iter([("name".to_string(), Value::String(name.into()))]);
    for (k, v) in php_array(config.clone()) {
        out.entry(k).or_insert(v);
    }
    Value::Object(out)
}

/// `$repository['name'] === $name`.
fn has_name(repo: &Value, name: &str) -> bool {
    repo.get("name").and_then(Value::as_str) == Some(name)
}

/// `policy.<list>.<field>` (at least two dots) as (list, field).
fn policy_field(name: &str) -> Option<(&str, &str)> {
    name.strip_prefix("policy.").filter(|_| name.matches('.').count() >= 2)?.split_once('.')
}

/// PHP `is_numeric()` on a string.
pub(crate) fn is_numeric(s: &str) -> bool {
    static RE: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"^[ \t\n\r\x0b\x0c]*[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?[ \t\n\r\x0b\x0c]*$").unwrap());
    RE.is_match(s)
}

/// Whether PHP turns this array key into an integer.
pub(crate) fn is_int_key(s: &str) -> bool {
    s.parse::<i64>().is_ok_and(|i| i.to_string() == s)
}

/// Items of the JSON list `[...]` as (start, end) offsets.
fn list_items(list: &str) -> Option<Vec<(usize, usize)>> {
    let b = list.as_bytes();
    let mut i = skip_ws(b, 1);
    let mut items = vec![];
    if b.get(i) == Some(&b']') {
        return Some(items);
    }
    loop {
        let end = value_end(list, i)?;
        items.push((i, end));
        let j = skip_ws(b, end);
        match b.get(j)? {
            b',' => i = skip_ws(b, j + 1),
            b']' => return Some(items),
            _ => return None,
        }
    }
}

/// PCRE `\s`.
fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t'..='\r')
}

/// `JsonFile::detectIndenting()`.
fn detect_indenting(json: &str) -> String {
    Regex::new(r#"(?m)^([ \t]+)""#).unwrap().captures(json).map(|c| c[1].to_string()).unwrap_or_else(|| "    ".into())
}

/// `JsonFile::encode($string)`.
fn encode_str(s: &str) -> String {
    json::encode(&Value::String(s.to_string()), Flags::FILE)
}

/// `str_replace('/', '\\\\?/', preg_quote($name))`.
fn escaped_slash_pattern(name: &str) -> String {
    regex::escape(name).replace('/', r"\\?/")
}

/// The literal a `preg_quote($s)` pattern matches under the `x` modifier, which ignores
/// the whitespace `preg_quote()` leaves unescaped.
fn strip_x(s: &str) -> String {
    s.replace([' ', '\t', '\n', '\x0b', '\x0c', '\r'], "")
}

/// The `(?&string)` define.
const STRING: &str = r#""(?:[^"\\]|\\["\\bfnrt/]|\\u[0-9A-Fa-f]{4})*""#;

fn replace_counting(haystack: &str, pattern: &str) -> (String, usize) {
    let re = Regex::new(pattern).unwrap();
    let count = re.find_iter(haystack).count();
    (re.replace_all(haystack, "").into_owned(), count)
}

/// Non-overlapping `key: value` occurrences of `key_re` (which must end after the `:` and
/// whitespace) followed by a JSON value: (member start, value start, value end).
fn find_members(text: &str, key_re: &Regex) -> Vec<(usize, usize, usize)> {
    let mut out = vec![];
    let mut pos = 0;
    while let Some(m) = key_re.find_at(text, pos) {
        match value_end(text, m.end()) {
            Some(end) => {
                out.push((m.start(), m.end(), end));
                pos = end;
            }
            None => pos = m.start() + 1,
        }
    }
    out
}

struct MemberSpan {
    key_start: usize,
    value_start: usize,
    value_end: usize,
}

/// The first top-level `"key": value` member (whose value starts with `open` if given), as the
/// `^\s*\{\s*(?:(?&string)\s*:\s*(?&json)\s*,\s*)*?"key"\s*:\s*` prefix of Composer's patterns finds it.
fn top_level_member(contents: &str, key: &str, open: Option<u8>) -> Option<MemberSpan> {
    let encoded = strip_x(&encode_str(key));
    let b = contents.as_bytes();
    let mut i = skip_ws(b, 0);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i = skip_ws(b, i + 1);
    loop {
        let key_start = i;
        let key_end = string_end(b, i)?;
        let mut j = skip_ws(b, key_end);
        if b.get(j) != Some(&b':') {
            return None;
        }
        j = skip_ws(b, j + 1);
        let value_end = value_end(contents, j)?;
        if contents[key_start..key_end] == encoded && open.is_none_or(|o| b[j] == o) {
            return Some(MemberSpan { key_start, value_start: j, value_end });
        }
        let k = skip_ws(b, value_end);
        if b.get(k) != Some(&b',') {
            return None;
        }
        i = skip_ws(b, k + 1);
    }
}

/// Skips PCRE `\s`.
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t'..=b'\r') {
        i += 1;
    }
    i
}

fn string_end(b: &[u8], i: usize) -> Option<usize> {
    if b.get(i) != Some(&b'"') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'"' => return Some(j + 1),
            b'\\' => j += 2,
            _ => j += 1,
        }
    }
    None
}

/// End offset of the JSON value starting at `i` (after optional whitespace).
fn value_end(text: &str, i: usize) -> Option<usize> {
    let b = text.as_bytes();
    let i = skip_ws(b, i);
    match *b.get(i)? {
        b'"' => string_end(b, i),
        b'{' | b'[' => {
            let close = if b[i] == b'{' { b'}' } else { b']' };
            let mut j = skip_ws(b, i + 1);
            if b.get(j) == Some(&close) {
                return Some(j + 1);
            }
            loop {
                if close == b'}' {
                    j = skip_ws(b, string_end(b, j)?);
                    if b.get(j) != Some(&b':') {
                        return None;
                    }
                    j += 1;
                }
                j = skip_ws(b, value_end(text, j)?);
                match b.get(j)? {
                    b',' => j = skip_ws(b, j + 1),
                    c if *c == close => return Some(j + 1),
                    _ => return None,
                }
            }
        }
        _ => {
            let end =
                b[i..].iter().position(|c| !matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E' | b'a'..=b'z')).map_or(b.len(), |p| i + p);
            (end > i).then_some(end)
        }
    }
}

#[cfg(test)]
#[path = "json_manipulator_tests.rs"]
mod php_tests;

#[cfg(test)]
mod tests {
    use super::*;

    const LARAVEL: &str = "{\n    \"name\": \"laravel/laravel\",\n    \"require\": {\n        \"php\": \"^8.3\",\n        \"laravel/framework\": \"^13.0\"\n    },\n    \"require-dev\": {\n        \"phpunit/phpunit\": \"^12.0\"\n    },\n    \"config\": {\n        \"sort-packages\": true\n    }\n}\n";

    #[test]
    fn adds_link_sorted() {
        let mut m = JsonManipulator::new(LARAVEL).unwrap();
        assert!(m.add_link("require", "guzzlehttp/guzzle", "^7.0", true));
        assert_eq!(
            m.contents(),
            "{\n    \"name\": \"laravel/laravel\",\n    \"require\": {\n        \"php\": \"^8.3\",\n        \"guzzlehttp/guzzle\": \"^7.0\",\n        \"laravel/framework\": \"^13.0\"\n    },\n    \"require-dev\": {\n        \"phpunit/phpunit\": \"^12.0\"\n    },\n    \"config\": {\n        \"sort-packages\": true\n    }\n}\n"
        );
    }

    #[test]
    fn appends_and_updates_links() {
        let mut m = JsonManipulator::new(LARAVEL).unwrap();
        assert!(m.add_link("require", "a/b", "^1.0", false));
        assert!(m.add_link("require", "Laravel/Framework", "^13.5", false));
        let out = m.contents();
        assert!(out.contains("\"laravel/framework\": \"^13.5\",\n        \"a/b\": \"^1.0\"\n    },"), "{out}");
    }

    #[test]
    fn removes_sub_node_and_empty_main_key() {
        let mut m = JsonManipulator::new(LARAVEL).unwrap();
        assert!(m.remove_sub_node("require-dev", "phpunit/phpunit"));
        assert!(m.remove_main_key_if_empty("require-dev"));
        assert_eq!(
            m.contents(),
            "{\n    \"name\": \"laravel/laravel\",\n    \"require\": {\n        \"php\": \"^8.3\",\n        \"laravel/framework\": \"^13.0\"\n    },\n    \"config\": {\n        \"sort-packages\": true\n    }\n}\n"
        );
    }

    #[test]
    fn adds_main_key_and_config() {
        let mut m = JsonManipulator::new("{}").unwrap();
        assert!(m.add_link("require", "a/b", "^1.0", false));
        assert!(m.add_config_setting("platform.php", &Value::String("8.3.0".into())));
        assert_eq!(
            m.contents(),
            "{\n    \"require\": {\n        \"a/b\": \"^1.0\"\n    },\n    \"config\": {\n        \"platform\": {\n            \"php\": \"8.3.0\"\n        }\n    }\n}\n"
        );
    }

    #[test]
    fn natural_sort() {
        assert_eq!(strnatcmp("a2", "a10"), Ordering::Less);
        assert_eq!(strnatcmp("0-php", "5-a/b"), Ordering::Less);
        // runs starting with 0 compare left-aligned, leading zeros only skipped at the start
        assert_eq!(strnatcmp("a05", "a1"), Ordering::Less);
        assert_eq!(strnatcmp("a010", "a02"), Ordering::Less);
        assert_eq!(strnatcmp("007", "7"), Ordering::Equal);
    }

    #[test]
    fn php_quirks() {
        // a list value is a PHP array keyed 0..n
        let mut m = JsonManipulator::new("{\n    \"config\": {\n        \"foo\": [\"a\"]\n    }\n}").unwrap();
        assert!(m.add_config_setting("foo.bar", &"x".into()));
        assert_eq!(
            m.contents(),
            "{\n    \"config\": {\n        \"foo\": {\n            \"0\": \"a\",\n            \"bar\": \"x\"\n        }\n    }\n}\n"
        );

        // the first of equally long matches is removed, even if nested
        let mut m = JsonManipulator::new("{\n    \"extra\": {\n        \"x\": {\"a\": 1},\n        \"a\": 2\n    }\n}").unwrap();
        assert!(m.remove_property("extra.a"));
        assert_eq!(m.contents(), "{\n    \"extra\": {\n        \"x\": {},\n        \"a\": 2\n    }\n}\n");

        // `isset()` is false for null, so the key is added again
        let mut m = JsonManipulator::new("{\n    \"require\": null\n}").unwrap();
        assert!(m.add_link("require", "a/b", "^1", false));
        assert_eq!(m.contents(), "{\n    \"require\": null,\n    \"require\": {\n        \"a/b\": \"^1\"\n    }\n}\n");

        // keys are matched under the `x` modifier, which drops their spaces
        let mut m = JsonManipulator::new("{\n    \"a b\": \"x\"\n}").unwrap();
        assert!(m.add_main_key("a b", &"y".into()));
        assert_eq!(m.contents(), "{\n    \"a b\": \"x\",\n    \"a b\": \"y\"\n}\n");
    }
}
