use serde_json::{Map, Value};
use std::collections::HashSet;

pub enum FilterMode {
    OptIn,  // Only include specified paths
    OptOut, // Include everything except specified paths
}

pub struct ConfigFilter {
    paths: HashSet<String>,
    mode: FilterMode,
}

impl ConfigFilter {
    pub fn new(paths: &[String], mode: FilterMode) -> Self {
        let paths: HashSet<String> = paths.iter().cloned().collect();
        Self { paths, mode }
    }

    pub fn filter_json(&self, json: Value) -> Value {
        if self.paths.is_empty() {
            match self.mode {
                FilterMode::OptIn => Value::Object(Map::new()), // Empty result if nothing opted in
                FilterMode::OptOut => json, // Everything included if nothing opted out
            }
        } else {
            self.filter_value(json, String::new())
        }
    }

    fn filter_value(&self, value: Value, current_path: String) -> Value {
        match value {
            Value::Object(obj) => {
                let filtered_obj = self.filter_object(obj, current_path);
                Value::Object(filtered_obj)
            }
            Value::Array(arr) => {
                let filtered_arr = self.filter_array(arr, current_path);
                Value::Array(filtered_arr)
            }
            _ => value,
        }
    }

    fn should_include_path(&self, path: &str) -> bool {
        match self.mode {
            FilterMode::OptIn => {
                // Check if the exact path is included
                if self.paths.contains(path) {
                    return true;
                }

                // Check if any parent path is included
                let base_path = path.split('[').next().unwrap(); // Get path without array index
                let mut parts: Vec<&str> = base_path.split('.').collect();

                // Check the exact parent path (for array elements)
                if self.paths.contains(&parts.join(".")) {
                    return true;
                }

                while parts.len() > 1 {
                    parts.pop();
                    let parent_path = parts.join(".");
                    if self.paths.contains(&parent_path) {
                        return true;
                    }
                }

                // Check if this is a parent of any included path
                self.paths.iter().any(|included_path| {
                    included_path.starts_with(base_path)
                        && (included_path.len() == base_path.len()
                            || included_path.chars().nth(base_path.len()) == Some('.'))
                })
            }
            FilterMode::OptOut => {
                // Check if the exact path is excluded
                if self.paths.contains(path) {
                    return false;
                }

                // Check if any parent path is excluded (hierarchical exclusion)
                let mut parts: Vec<&str> = path.split('.').collect();
                while parts.len() > 1 {
                    parts.pop();
                    let parent_path = parts.join(".");
                    if self.paths.contains(&parent_path) {
                        return false;
                    }
                }
                true
            }
        }
    }

    fn filter_object(&self, obj: Map<String, Value>, path: String) -> Map<String, Value> {
        let mut filtered_obj = Map::new();

        for (key, value) in obj {
            let current_path = if path.is_empty() {
                key.clone()
            } else {
                format!("{}.{}", path, key)
            };

            let should_include = self.should_include_path(&current_path);

            if should_include {
                filtered_obj.insert(key, self.filter_value(value, current_path));
            }
        }

        filtered_obj
    }

    fn filter_array(&self, arr: Vec<Value>, path: String) -> Vec<Value> {
        let mut filtered_arr = Vec::new();

        for (idx, value) in arr.into_iter().enumerate() {
            let current_path = format!("{}[{}]", path, idx);

            let should_include = self.should_include_path(&current_path);

            if should_include {
                filtered_arr.push(self.filter_value(value, current_path));
            }
        }

        filtered_arr
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(mode: FilterMode, keys: &[&str], input: Value) -> Value {
        let keys: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
        ConfigFilter::new(&keys, mode).filter_json(input)
    }

    #[test]
    fn include_nested_key_keeps_only_that_branch() {
        let input =
            json!({"dns": {"upstreams": ["1.1.1.1"], "hosts": []}, "webserver": {"port": 80}});
        let out = run(FilterMode::OptIn, &["dns.upstreams"], input);
        assert_eq!(out, json!({"dns": {"upstreams": ["1.1.1.1"]}}));
    }

    #[test]
    fn include_empty_keys_yields_empty_object() {
        let out = run(FilterMode::OptIn, &[], json!({"dns": {"a": 1}}));
        assert_eq!(out, json!({}));
    }

    #[test]
    fn exclude_nested_key_keeps_siblings() {
        let input = json!({"dns": {"upstreams": ["1.1.1.1"], "hosts": ["h"]}, "x": 1});
        let out = run(FilterMode::OptOut, &["dns.upstreams"], input);
        assert_eq!(out, json!({"dns": {"hosts": ["h"]}, "x": 1}));
    }

    #[test]
    fn exclude_empty_keys_is_unchanged() {
        let input = json!({"dns": {"a": 1}});
        assert_eq!(run(FilterMode::OptOut, &[], input.clone()), input);
    }

    #[test]
    fn include_array_key_keeps_whole_array() {
        let input = json!({"dns": {"hosts": ["a", "b", {"k": 1}], "other": 1}});
        let out = run(FilterMode::OptIn, &["dns.hosts"], input);
        assert_eq!(out, json!({"dns": {"hosts": ["a", "b", {"k": 1}]}}));
    }

    #[test]
    fn include_does_not_match_sibling_with_shared_prefix() {
        let input = json!({"dns": {"a": 1}, "dnsmasq_lines": ["x"]});
        let out = run(FilterMode::OptIn, &["dns"], input);
        assert_eq!(out, json!({"dns": {"a": 1}}));

        let input = json!({"dns": {"hosts": ["h"], "hostRecord": "r"}});
        let out = run(FilterMode::OptIn, &["dns.hosts"], input);
        assert_eq!(out, json!({"dns": {"hosts": ["h"]}}));
    }
}
